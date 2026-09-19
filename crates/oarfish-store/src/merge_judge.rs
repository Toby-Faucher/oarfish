//! The background merge judge: one judgment per close pair, never per line.
//!
//! Built to the shape [`crate::Judge`] proved: a bounded queue, a background
//! task, cache-aside over its keyspace, and the provider behind [`Decide`] so
//! production judges through the Jev client and tests judge through a fake —
//! no network, no billing, chosen answers. A dropped enqueue is harmless for
//! the same reason it is in `Judge`: the next new template refers the pair
//! again.
//!
//! One question, a `noul`: *do these describe the same event type?*

use std::collections::HashSet;
use std::sync::Arc;

use oarfish_core::TemplateId;
use time::OffsetDateTime;
use tokio::sync::{Mutex, Semaphore, mpsc};
use ulid::Ulid;

use crate::judge::Decide;
use crate::merge_cache::{AliasTarget, MergeShared, read_latest_pair};
use crate::merge_decision::MergeDecision;
use crate::merge_keys::merge_key;
use crate::record::{DecisionRecord, record_key};
use crate::verdict_cache::record_by_template_key;
use crate::verdicts::Error;

/// One unit of merge judging: the canonical pair for the key, both texts for
/// the state. The call needs the templates, not just their ids.
#[derive(Debug, Clone)]
struct MergeJob {
    id_lo: TemplateId,
    template_lo: String,
    id_hi: TemplateId,
    template_hi: String,
}

/// The off-path merge-judging task. Built over shared state and a [`Decide`]
/// adapter; closed on shutdown.
pub struct MergeJudge {
    sender: mpsc::Sender<MergeJob>,
    handle: Mutex<Option<tokio::task::JoinHandle<()>>>,
}

impl MergeJudge {
    /// Spawn the background task over shared state, judging through
    /// `decide`. Call from within a Tokio runtime.
    pub fn spawn<D: Decide>(
        shared: Arc<MergeShared>,
        decide: D,
        queue_capacity: usize,
        concurrency: usize,
    ) -> Self {
        let (sender, receiver) = mpsc::channel(queue_capacity);
        let handle = tokio::spawn(merge_loop(shared, decide, receiver, concurrency));
        Self {
            sender,
            handle: Mutex::new(Some(handle)),
        }
    }

    /// Enqueue one canonical pair for judging. A full or closed queue is
    /// harmless: the next new template refers the pair again.
    pub fn enqueue(
        &self,
        id_lo: TemplateId,
        template_lo: &str,
        id_hi: TemplateId,
        template_hi: &str,
    ) {
        if let Err(error) = self.sender.try_send(MergeJob {
            id_lo,
            template_lo: template_lo.to_owned(),
            id_hi,
            template_hi: template_hi.to_owned(),
        }) {
            tracing::debug!(
                id_lo = %id_lo,
                id_hi = %id_hi,
                %error,
                "merge queue unavailable; the next new template will refer the pair again"
            );
        }
    }

    /// Shut the merge judge down: close the queue, then join the task.
    pub async fn close(self) -> Result<(), Error> {
        drop(self.sender);
        if let Some(handle) = self.handle.lock().await.take() {
            handle.await?;
        }
        Ok(())
    }
}

/// The off-path task. Owns the in-flight set; the queue carries both template
/// texts because the call needs the state.
async fn merge_loop<D: Decide>(
    shared: Arc<MergeShared>,
    decide: D,
    mut receiver: mpsc::Receiver<MergeJob>,
    concurrency: usize,
) {
    let semaphore = Arc::new(Semaphore::new(concurrency.max(1)));
    let in_flight = Arc::new(Mutex::new(HashSet::<(TemplateId, TemplateId)>::new()));

    while let Some(job) = receiver.recv().await {
        {
            let mut flight = in_flight.lock().await;
            if !flight.insert((job.id_lo, job.id_hi)) {
                continue;
            }
        }
        // Judged while queued: serve it without spending a call. A `no` is
        // cached too, so this skips rejected pairs as well as merged ones.
        if let Some(cached) = shared
            .cache
            .get(&(job.id_lo, job.id_hi, shared.bundle_hash))
        {
            let _ = cached;
            in_flight.lock().await.remove(&(job.id_lo, job.id_hi));
            continue;
        }
        match read_latest_pair(&shared, &job.id_lo, &job.id_hi) {
            Ok(Some(decision)) => {
                shared
                    .cache
                    .insert((job.id_lo, job.id_hi, shared.bundle_hash), decision);
                in_flight.lock().await.remove(&(job.id_lo, job.id_hi));
                continue;
            }
            Ok(None) => {}
            Err(error) => {
                tracing::warn!(
                    id_lo = %job.id_lo,
                    id_hi = %job.id_hi,
                    %error,
                    "merge read failed inside the judge; attempting the call anyway"
                );
            }
        }

        let permit = semaphore
            .clone()
            .acquire_owned()
            .await
            .expect("the semaphore outlives the merge judge");
        let shared = Arc::clone(&shared);
        let decide = decide.clone();
        let in_flight = Arc::clone(&in_flight);
        let pair = (job.id_lo, job.id_hi);
        tokio::spawn(async move {
            let _permit = permit;
            judge_one(&shared, &decide, job).await;
            in_flight.lock().await.remove(&pair);
        });
    }
}

/// The single merge question id. Defined beside the judge that asks it, the
/// same discipline as the engine's `SEVERITY_QUESTION`: a rename breaks the
/// reader at compile time instead of silently judging nothing.
pub const MERGE_QUESTION: &str = "same_event";

/// One judgment: call, then record, then decision, then moka and the alias
/// table — in that order. A crash between writes leaves an orphan record,
/// which is harmless and replayable, rather than a cached merge with no
/// provenance. No retries: a failed call drops its key and the next new
/// template refers the pair again.
async fn judge_one<D: Decide>(shared: &MergeShared, decide: &D, job: MergeJob) {
    let state = serde_json::json!({
        "template_lo": job.template_lo,
        "template_id_lo": job.id_lo.to_string(),
        "template_hi": job.template_hi,
        "template_id_hi": job.id_hi.to_string(),
    });
    let decision = match decide.decide(&state, &shared.questions).await {
        Ok(decision) => decision,
        Err(error) => {
            tracing::warn!(
                id_lo = %job.id_lo,
                id_hi = %job.id_hi,
                %error,
                "merge call failed; the next new template will refer the pair again"
            );
            return;
        }
    };

    let noul = match decision.answers.get(MERGE_QUESTION) {
        Some(oarfish_jev::Answer::Noul { noul }) => *noul,
        other => {
            tracing::warn!(
                id_lo = %job.id_lo,
                id_hi = %job.id_hi,
                ?other,
                "merge answer is not a noul; skipping the decision"
            );
            return;
        }
    };

    let judged_at = OffsetDateTime::now_utc();
    let state_json = state.to_string();
    let questions_json = match serde_json::to_string(&shared.questions) {
        Ok(json) => json,
        Err(error) => {
            tracing::warn!(
                id_lo = %job.id_lo,
                id_hi = %job.id_hi,
                %error,
                "merge questions would not serialize; skipping the decision"
            );
            return;
        }
    };
    let answers_json = match serde_json::to_string(&decision.answers) {
        Ok(json) => json,
        Err(error) => {
            tracing::warn!(
                id_lo = %job.id_lo,
                id_hi = %job.id_hi,
                %error,
                "merge answers would not serialize; skipping the decision"
            );
            return;
        }
    };
    let (input_tokens, output_tokens, cost) = match &decision.usage {
        Some(usage) => (usage.input_tokens, usage.output_tokens, usage.cost),
        None => (0, 0, None),
    };
    let record = DecisionRecord {
        id: Ulid::generate(),
        template_id: job.id_lo,
        questions_hash: shared.questions_hash,
        model: decision.model.clone(),
        template: job.template_lo.clone(),
        state_json,
        questions_json,
        answers_json,
        input_tokens,
        output_tokens,
        cost,
        recorded_at_unix: judged_at.unix_timestamp(),
    };
    let merge = MergeDecision {
        id_lo: job.id_lo,
        id_hi: job.id_hi,
        template_lo: job.template_lo.clone(),
        template_hi: job.template_hi.clone(),
        questions_hash: shared.questions_hash,
        model: decision.model,
        noul,
        merged: noul >= shared.merge_threshold,
        judged_at_unix: judged_at.unix_timestamp(),
    };

    let record_bytes = match postcard::to_stdvec(&record) {
        Ok(bytes) => bytes,
        Err(error) => {
            tracing::warn!(
                id_lo = %job.id_lo,
                id_hi = %job.id_hi,
                %error,
                "merge record would not encode; skipping the decision"
            );
            return;
        }
    };
    if let Err(error) = shared.records.insert(record_key(&record.id), record_bytes) {
        tracing::warn!(
            id_lo = %job.id_lo,
            id_hi = %job.id_hi,
            %error,
            "merge record write failed; skipping the decision so nothing is cached without provenance"
        );
        return;
    }
    if let Err(error) = shared
        .records_by_template
        .insert(record_by_template_key(&record.template_id, &record.id), [])
    {
        tracing::warn!(
            id_lo = %job.id_lo,
            id_hi = %job.id_hi,
            %error,
            "merge record index write failed; the record stays readable via the legacy scan"
        );
    }
    let decision_bytes = match postcard::to_stdvec(&merge) {
        Ok(bytes) => bytes,
        Err(error) => {
            tracing::warn!(
                id_lo = %job.id_lo,
                id_hi = %job.id_hi,
                %error,
                "merge decision would not encode; leaving the record orphaned"
            );
            return;
        }
    };
    let key = merge_key(
        &merge.id_lo,
        &merge.id_hi,
        &merge.questions_hash,
        &shared.bundle_hash,
        &merge.model,
    );
    if let Err(error) = shared.merges.insert(key, decision_bytes) {
        tracing::warn!(
            id_lo = %job.id_lo,
            id_hi = %job.id_hi,
            %error,
            "merge decision write failed; leaving the record orphaned"
        );
        return;
    }
    shared
        .cache
        .insert((job.id_lo, job.id_hi, shared.bundle_hash), merge.clone());
    // The lower id always wins: every alias points strictly downward, so
    // chains terminate and cycles are unrepresentable. A rejection hooks
    // nothing — but it stays cached, so it is never re-asked.
    if merge.merged
        && let Ok(mut aliases) = shared.aliases.write()
    {
        aliases.insert(
            job.id_hi,
            AliasTarget {
                canonical: job.id_lo,
                template: job.template_lo,
            },
        );
    }
}

/// One judgment: call, then record, then decision, then moka and the alias
/// table — in that order, and the tests proving the seam below.
#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    use oarfish_core::{QuestionsHash, TemplateId};
    use oarfish_jev::{Decision, Question};
    use oarfish_mask::BundleHash;

    /// The second adapter: a canned provider. No network, no billing, and
    /// chosen answers — whatever the test needs the pair to say.
    #[derive(Debug, Clone)]
    struct FakeDecide {
        noul: f64,
    }

    impl Decide for FakeDecide {
        fn decide<'a>(
            &'a self,
            _state: &'a serde_json::Value,
            _questions: &'a BTreeMap<String, Question>,
        ) -> impl std::future::Future<Output = Result<Decision, oarfish_jev::Error>> + Send + 'a
        {
            let noul = self.noul;
            async move {
                Ok(Decision {
                    answers: [(
                        MERGE_QUESTION.to_owned(),
                        oarfish_jev::Answer::Noul { noul },
                    )]
                    .into_iter()
                    .collect(),
                    model: "fake-build-1".to_owned(),
                    usage: None,
                })
            }
        }
    }

    fn shared(dir: &std::path::Path) -> Arc<MergeShared> {
        use std::collections::HashMap;
        use std::sync::RwLock;
        let db = fjall::Database::builder(dir)
            .open()
            .expect("temp store opens");
        let keyspace = |db: &fjall::Database, name: &str| {
            db.keyspace(name, fjall::KeyspaceCreateOptions::default)
                .expect("keyspace opens")
        };
        Arc::new(MergeShared {
            merges: keyspace(&db, "merges"),
            records: keyspace(&db, "records"),
            records_by_template: keyspace(&db, "records_by_template"),
            cache: moka::sync::Cache::new(64),
            aliases: RwLock::new(HashMap::new()),
            questions: BTreeMap::from([(
                MERGE_QUESTION.to_owned(),
                Question::noul(
                    serde_json::json!("Do these describe the same event type?"),
                    None,
                ),
            )]),
            questions_hash: QuestionsHash::of(b"merge"),
            bundle_hash: BundleHash::from_bytes([7u8; 32]),
            merge_threshold: 0.5,
        })
    }

    fn job() -> MergeJob {
        let a = TemplateId::of("backup done files ok");
        let b = TemplateId::of("backup done files ok extra");
        let (lo, tlo, hi, thi) = crate::merge_keys::canonical_pair(
            a,
            "backup done files ok",
            b,
            "backup done files ok extra",
        );
        MergeJob {
            id_lo: lo,
            template_lo: tlo,
            id_hi: hi,
            template_hi: thi,
        }
    }

    #[tokio::test]
    async fn one_judgment_lands_a_decision_a_record_an_alias_and_a_cache_entry() {
        let dir = std::env::temp_dir().join(format!("oarfish-merge-test-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let shared = shared(&dir);

        judge_one(&shared, &FakeDecide { noul: 0.91 }, job()).await;

        let (lo, hi) = (job().id_lo, job().id_hi);
        let decision = read_latest_pair(&shared, &lo, &hi)
            .expect("read")
            .expect("judged");
        assert_eq!(decision.model, "fake-build-1");
        assert!(decision.merged);
        assert!(shared.cache.get(&(lo, hi, shared.bundle_hash)).is_some());
        // The alias hooks the greater id to the lesser with its text.
        let aliases = shared.aliases.read().expect("lock");
        assert_eq!(
            aliases.get(&hi),
            Some(&AliasTarget {
                canonical: lo,
                template: job().template_lo.clone(),
            })
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn a_rejection_is_cached_but_hooks_no_alias() {
        let dir =
            std::env::temp_dir().join(format!("oarfish-merge-no-test-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let shared = shared(&dir);

        judge_one(&shared, &FakeDecide { noul: 0.12 }, job()).await;

        let (lo, hi) = (job().id_lo, job().id_hi);
        let decision = read_latest_pair(&shared, &lo, &hi)
            .expect("read")
            .expect("judged");
        assert!(!decision.merged);
        assert!(
            shared.cache.get(&(lo, hi, shared.bundle_hash)).is_some(),
            "a `no` is cached too"
        );
        assert!(
            shared.aliases.read().expect("lock").get(&hi).is_none(),
            "a rejection hooks nothing"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The seam, proven through the real wiring: a spawned merge judge
    /// judging through the fake lands a decision the cache serves, with no
    /// network anywhere in the path.
    #[tokio::test]
    async fn a_spawned_merge_judge_with_a_fake_judges_through_the_wiring() {
        let dir =
            std::env::temp_dir().join(format!("oarfish-merge-spawn-test-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let shared = shared(&dir);
        let judge = MergeJudge::spawn(Arc::clone(&shared), FakeDecide { noul: 0.91 }, 16, 2);
        let cache = crate::merge_cache::MergeCache::new(Arc::clone(&shared));

        let job = job();
        judge.enqueue(job.id_lo, &job.template_lo, job.id_hi, &job.template_hi);
        let (lo, hi) = (job.id_lo, job.id_hi);
        let decision = tokio::time::timeout(std::time::Duration::from_secs(5), async {
            loop {
                if let Some(decision) = cache.lookup(&lo, &hi) {
                    break decision;
                }
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("the fake judges within the timeout");
        assert!(decision.merged);

        judge.close().await.expect("close");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
