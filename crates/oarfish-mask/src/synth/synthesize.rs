//! Orchestrating one synthesis run: find candidates, ask the model, validate
//! and merge each proposed slot, retry a rejection once with the error as
//! feedback, then drop what still fails. Ends with an idempotency check over
//! the full corpus, per §5 of the design.

use crate::synth::{Candidate, build_fix_prompt, build_prompt, find_candidates, merge};
use crate::{Bundle, SlotDef};

/// What talks to the model. Implemented for [`oarfish_synth::Client`] in
/// production; tests implement it with a fake, no network — the same seam
/// discipline as `Decide` everywhere else in this codebase.
pub trait Synthesize: Send + Sync {
    fn synthesize<'a>(
        &'a self,
        prompt: &'a str,
    ) -> impl std::future::Future<Output = Result<String, oarfish_synth::Error>> + Send + 'a;
}

impl Synthesize for oarfish_synth::Client {
    fn synthesize<'a>(
        &'a self,
        prompt: &'a str,
    ) -> impl std::future::Future<Output = Result<String, oarfish_synth::Error>> + Send + 'a {
        oarfish_synth::Client::synthesize(self, prompt)
    }
}

/// What one run produced: the merged bundle, and the name of any slot the
/// model could not fix within one retry.
#[derive(Debug)]
pub struct SynthesisReport {
    pub bundle: Bundle,
    pub dropped: Vec<String>,
}

/// Why a run produced nothing at all — the provider call itself failed, or
/// its response was not JSON. A slot that fails *validation* is not this: it
/// is dropped and reported in [`SynthesisReport::dropped`] instead.
#[derive(Debug, thiserror::Error)]
pub enum SynthesisError {
    #[error("the model's response was not valid JSON: {0}")]
    BadJson(String),
    #[error("the provider call failed: {0}")]
    Provider(#[from] oarfish_synth::Error),
}

/// Run the full synthesis pipeline over one corpus.
pub async fn run<S: Synthesize>(
    synth: &S,
    starting: &Bundle,
    corpus: &[String],
    min_occurrences: u64,
    max_candidates: usize,
) -> Result<SynthesisReport, SynthesisError> {
    let mut candidates: Vec<Candidate> = find_candidates(starting, corpus, min_occurrences);
    candidates.truncate(max_candidates);

    if candidates.is_empty() {
        return Ok(SynthesisReport {
            bundle: starting.clone(),
            dropped: Vec::new(),
        });
    }

    let prompt = build_prompt(&candidates);
    let response = synth.synthesize(&prompt).await?;
    let proposed: Vec<SlotDef> =
        serde_json::from_str(&response).map_err(|e| SynthesisError::BadJson(e.to_string()))?;

    let mut accepted: Vec<SlotDef> = Vec::new();
    let mut dropped: Vec<String> = Vec::new();

    for mut slot in proposed {
        // Traceable at a glance: a synthesized slot's `why` is marked as
        // such, distinct from a curated slot's own reasoning (spec §5).
        slot.why = format!("synthesized: {}", slot.why);
        let mut candidate_list = accepted.clone();
        candidate_list.push(slot.clone());
        match merge(starting, &candidate_list) {
            Ok(_) => accepted = candidate_list,
            Err(error) => {
                let fix_prompt = build_fix_prompt(&slot, &error);
                let fixed = match synth.synthesize(&fix_prompt).await {
                    Ok(text) => text,
                    Err(_) => {
                        dropped.push(slot.name);
                        continue;
                    }
                };
                match serde_json::from_str::<Option<SlotDef>>(&fixed) {
                    Ok(Some(mut fixed_slot)) => {
                        fixed_slot.why = format!("synthesized: {}", fixed_slot.why);
                        let mut retry_list = accepted.clone();
                        retry_list.push(fixed_slot);
                        match merge(starting, &retry_list) {
                            Ok(_) => accepted = retry_list,
                            Err(_) => dropped.push(slot.name),
                        }
                    }
                    Ok(None) | Err(_) => dropped.push(slot.name),
                }
            }
        }
    }

    // `merge` re-validates the accumulated list on every call above, so this
    // final call is redundant work — one more Bundle::parse over a list
    // already proven to validate. It's kept for simplicity: this is a rare,
    // once-at-install path, and re-deriving the last successful `Bundle`
    // from inside the loop instead would save one regex compile at the cost
    // of a more tangled loop body.
    let mut bundle = merge(starting, &accepted).expect("accepted slots already validated above");

    if !accepted.is_empty() {
        for line in corpus {
            let once = bundle.mask(line).template().to_owned();
            let twice = bundle.mask(&once).template().to_owned();
            if once != twice {
                dropped.extend(accepted.iter().map(|s| s.name.clone()));
                bundle = starting.clone();
                break;
            }
        }
    }

    Ok(SynthesisReport { bundle, dropped })
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use crate::curated;

    use super::*;

    /// The second adapter: a canned provider with chosen responses in
    /// sequence — no network, no billing.
    struct FakeSynth {
        responses: std::sync::Mutex<Vec<String>>,
        calls: AtomicUsize,
    }

    impl FakeSynth {
        fn new(responses: Vec<&str>) -> Self {
            Self {
                responses: std::sync::Mutex::new(
                    responses.into_iter().map(str::to_owned).collect(),
                ),
                calls: AtomicUsize::new(0),
            }
        }
    }

    impl Synthesize for FakeSynth {
        fn synthesize<'a>(
            &'a self,
            _prompt: &'a str,
        ) -> impl std::future::Future<Output = Result<String, oarfish_synth::Error>> + Send + 'a
        {
            self.calls.fetch_add(1, Ordering::SeqCst);
            let mut guard = self.responses.lock().expect("lock");
            let next = if guard.is_empty() {
                "[]".to_owned()
            } else {
                guard.remove(0)
            };
            async move { Ok(next) }
        }
    }

    fn corpus() -> Vec<String> {
        // Ten tokens: a single varying position is 9/10 similarity, exactly
        // the daemon's 0.90 threshold, so Drain merges these — the same
        // pattern as the candidates tests. Three-token lines would only
        // reach 2/3 and never cluster.
        vec![
            "batch xk92 completed extra alpha beta gamma delta epsilon zeta".to_owned(),
            "batch qm14 completed extra alpha beta gamma delta epsilon zeta".to_owned(),
            "batch zt77 completed extra alpha beta gamma delta epsilon zeta".to_owned(),
        ]
    }

    #[tokio::test]
    async fn a_valid_proposal_merges_and_masks_the_corpus() {
        let synth = FakeSynth::new(vec![
            r#"[{"name": "JOBID", "pattern": "[a-z]{2}\\d{2}", "why": "job codes"}]"#,
        ]);
        let report = run(&synth, curated(), &corpus(), 3, 50).await.expect("run");
        assert!(report.dropped.is_empty(), "got {:?}", report.dropped);
        let slot = report
            .bundle
            .slots()
            .iter()
            .find(|s| s.name == "JOBID")
            .expect("JOBID present");
        assert!(
            slot.why.starts_with("synthesized: "),
            "why should be marked synthesized, got {:?}",
            slot.why
        );
        assert_eq!(
            report.bundle.mask("batch xk92 completed").template(),
            "batch <VAR:JOBID> completed"
        );
    }

    #[tokio::test]
    async fn a_rejected_slot_recovers_on_the_one_retry() {
        let synth = FakeSynth::new(vec![
            r#"[{"name": "JOBID", "pattern": "job-(a-z0-9)", "why": "bad, has a group"}]"#,
            r#"{"name": "JOBID", "pattern": "[a-z]{2}\\d{2}", "why": "fixed"}"#,
        ]);
        let report = run(&synth, curated(), &corpus(), 3, 50).await.expect("run");
        assert!(report.dropped.is_empty(), "got {:?}", report.dropped);
        assert!(report.bundle.slots().iter().any(|s| s.name == "JOBID"));
    }

    #[tokio::test]
    async fn a_twice_rejected_slot_is_dropped_not_blocking() {
        let synth = FakeSynth::new(vec![
            r#"[{"name": "JOBID", "pattern": "job-(a-z0-9)", "why": "bad"}]"#,
            r#"{"name": "JOBID", "pattern": "job-(a-z0-9)", "why": "still bad"}"#,
        ]);
        let report = run(&synth, curated(), &corpus(), 3, 50).await.expect("run");
        assert_eq!(report.dropped, vec!["JOBID".to_owned()]);
        assert_eq!(report.bundle.slots(), curated().slots());
    }

    #[tokio::test]
    async fn no_candidates_returns_the_starting_bundle_unchanged() {
        let synth = FakeSynth::new(vec!["[]"]);
        let corpus = vec!["nothing unusual here".to_owned(); 3];
        let report = run(&synth, curated(), &corpus, 3, 50).await.expect("run");
        assert_eq!(report.dropped, Vec::<String>::new());
        assert_eq!(report.bundle.slots(), curated().slots());
        assert_eq!(
            synth.calls.load(Ordering::SeqCst),
            0,
            "no candidates, no call"
        );
    }
}
