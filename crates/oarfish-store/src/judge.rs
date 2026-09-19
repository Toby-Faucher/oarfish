//! The background judge: one judgment per new template, never per line.
//!
//! The queue carries `(TemplateId, template_text)` because the call needs
//! the state, not just the key. Judging is off the every-line path by
//! construction: the cache side enqueues and moves on, and this task spends
//! the 1–2s per call. A first run meets a few hundred new templates at once,
//! so up to [`crate::DEFAULT_JUDGE_CONCURRENCY`] calls fly together.
//!
//! The provider sits behind [`Decide`]. Production judges through the Jev
//! client; tests judge through a fake — two adapters, one real seam — so
//! judging is provable with no network, no billing, and chosen answers.
//!
//! A dropped enqueue is harmless. If the queue is full the send fails and
//! nothing happens; the next line carrying that template misses again and
//! re-enqueues. The queue needs no unbounded growth, no retry logic and no
//! persistence, because the log stream is itself the retry mechanism.

use std::collections::HashSet;
use std::sync::Arc;

use oarfish_core::TemplateId;
use oarfish_jev::{Client, Decision, Question};
use time::OffsetDateTime;
use tokio::sync::{Mutex, Semaphore, mpsc};
use ulid::Ulid;

use crate::keys::verdict_key;
use crate::record::{DecisionRecord, record_key};
use crate::shared::Shared;
use crate::verdict_cache::record_by_template_key;
use crate::verdicts::Error;

/// How one judgment is requested: what the provider decides. The returned
/// future is `Send` so judging can ride on spawned tasks.
pub trait Decide: Clone + Send + Sync + 'static {
    fn decide<'a>(
        &'a self,
        state: &'a serde_json::Value,
        questions: &'a std::collections::BTreeMap<String, Question>,
    ) -> impl std::future::Future<Output = Result<Decision, oarfish_jev::Error>> + Send + 'a;
}

impl Decide for Client {
    fn decide<'a>(
        &'a self,
        state: &'a serde_json::Value,
        questions: &'a std::collections::BTreeMap<String, Question>,
    ) -> impl std::future::Future<Output = Result<Decision, oarfish_jev::Error>> + Send + 'a {
        Client::decide(self, state, questions)
    }
}

/// One unit of judging: the id for the key, the text for the state. The call
/// needs the template, not just its id.
#[derive(Debug, Clone)]
struct JudgeJob {
    template_id: TemplateId,
    template: String,
}

/// The off-path judging task. Built over shared state and a [`Decide`]
/// adapter; closed on shutdown.
pub struct Judge {
    sender: mpsc::Sender<JudgeJob>,
    handle: Mutex<Option<tokio::task::JoinHandle<()>>>,
}

impl Judge {
    /// Spawn the background task over shared state, judging through
    /// `decide`. Call from within a Tokio runtime.
    pub fn spawn<D: Decide>(
        shared: Arc<Shared>,
        decide: D,
        queue_capacity: usize,
        concurrency: usize,
    ) -> Self {
        let (sender, receiver) = mpsc::channel(queue_capacity);
        let handle = tokio::spawn(judge_loop(shared, decide, receiver, concurrency));
        Self {
            sender,
            handle: Mutex::new(Some(handle)),
        }
    }

    /// Enqueue one template for judging. A full or closed queue is harmless:
    /// the next line carrying the template misses again and re-enqueues.
    pub fn enqueue(&self, template_id: TemplateId, template: &str) {
        if let Err(error) = self.sender.try_send(JudgeJob {
            template_id,
            template: template.to_owned(),
        }) {
            tracing::debug!(
                template_id = %template_id,
                %error,
                "judge queue unavailable; the log stream will retry"
            );
        }
    }

    /// Shut the judge down: close the queue, then join the task.
    pub async fn close(self) -> Result<(), Error> {
        drop(self.sender);
        if let Some(handle) = self.handle.lock().await.take() {
            handle.await?;
        }
        Ok(())
    }
}

/// The off-path task. Owns the in-flight set; the queue carries
/// `(TemplateId, template_text)` because the call needs the state.
async fn judge_loop<D: Decide>(
    shared: Arc<Shared>,
    decide: D,
    mut receiver: mpsc::Receiver<JudgeJob>,
    concurrency: usize,
) {
    let semaphore = Arc::new(Semaphore::new(concurrency.max(1)));
    let in_flight = Arc::new(Mutex::new(HashSet::<TemplateId>::new()));

    while let Some(job) = receiver.recv().await {
        {
            let mut flight = in_flight.lock().await;
            if !flight.insert(job.template_id) {
                continue;
            }
        }
        // Judged while queued: serve it without spending a call.
        if let Some(cached) = shared.cache.get(&(job.template_id, shared.bundle_hash)) {
            let _ = cached;
            in_flight.lock().await.remove(&job.template_id);
            continue;
        }
        match crate::verdict_cache::read_latest(&shared, &job.template_id) {
            Ok(Some(verdict)) => {
                shared
                    .cache
                    .insert((job.template_id, shared.bundle_hash), verdict);
                in_flight.lock().await.remove(&job.template_id);
                continue;
            }
            Ok(None) => {}
            Err(error) => {
                tracing::warn!(
                    template_id = %job.template_id,
                    %error,
                    "verdict read failed inside the judge; attempting the call anyway"
                );
            }
        }

        let permit = semaphore
            .clone()
            .acquire_owned()
            .await
            .expect("the semaphore outlives the judge");
        let shared = Arc::clone(&shared);
        let decide = decide.clone();
        let in_flight = Arc::clone(&in_flight);
        let template_id = job.template_id;
        tokio::spawn(async move {
            let _permit = permit;
            judge_one(&shared, &decide, job).await;
            in_flight.lock().await.remove(&template_id);
        });
    }
}

/// One judgment: call, then record, then verdict, then moka — in that order.
/// A crash between writes leaves an orphan record, which is harmless and
/// replayable, rather than a cached verdict with no provenance. No retries:
/// a failed call drops its key and the log stream re-enqueues.
async fn judge_one<D: Decide>(shared: &Shared, decide: &D, job: JudgeJob) {
    let state = serde_json::json!({
        "template": job.template,
        "template_id": job.template_id.to_string(),
    });
    let decision = match decide.decide(&state, &shared.questions).await {
        Ok(decision) => decision,
        Err(error) => {
            tracing::warn!(
                template_id = %job.template_id,
                %error,
                "judge call failed; the log stream will retry"
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
                template_id = %job.template_id,
                %error,
                "questions would not serialize; skipping the verdict"
            );
            return;
        }
    };
    let answers_json = match serde_json::to_string(&decision.answers) {
        Ok(json) => json,
        Err(error) => {
            tracing::warn!(
                template_id = %job.template_id,
                %error,
                "answers would not serialize; skipping the verdict"
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
        template_id: job.template_id,
        questions_hash: shared.questions_hash,
        model: decision.model.clone(),
        template: job.template.clone(),
        state_json,
        questions_json,
        answers_json,
        input_tokens,
        output_tokens,
        cost,
        recorded_at_unix: judged_at.unix_timestamp(),
    };
    let verdict = oarfish_core::Verdict {
        template_id: job.template_id,
        questions_hash: shared.questions_hash,
        model: decision.model,
        answers: decision
            .answers
            .into_iter()
            .map(|(key, answer)| (key, answer.into_verdict_answer()))
            .collect(),
        judged_at,
    };

    let record_bytes = match postcard::to_stdvec(&record) {
        Ok(bytes) => bytes,
        Err(error) => {
            tracing::warn!(
                template_id = %job.template_id,
                %error,
                "decision record would not encode; skipping the verdict"
            );
            return;
        }
    };
    if let Err(error) = shared.records.insert(record_key(&record.id), record_bytes) {
        tracing::warn!(
            template_id = %job.template_id,
            %error,
            "decision record write failed; skipping the verdict so nothing is cached without provenance"
        );
        return;
    }
    if let Err(error) = shared
        .records_by_template
        .insert(record_by_template_key(&record.template_id, &record.id), [])
    {
        tracing::warn!(
            template_id = %job.template_id,
            %error,
            "record index write failed; the record stays readable via the legacy scan"
        );
    }
    let verdict_bytes = match postcard::to_stdvec(&verdict) {
        Ok(bytes) => bytes,
        Err(error) => {
            tracing::warn!(
                template_id = %job.template_id,
                %error,
                "verdict would not encode; leaving the record orphaned"
            );
            return;
        }
    };
    let key = verdict_key(
        &verdict.template_id,
        &verdict.questions_hash,
        &shared.bundle_hash,
        &verdict.model,
    );
    if let Err(error) = shared.verdicts.insert(key, verdict_bytes) {
        tracing::warn!(
            template_id = %job.template_id,
            %error,
            "verdict write failed; leaving the record orphaned"
        );
        return;
    }
    shared
        .cache
        .insert((job.template_id, shared.bundle_hash), verdict);
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The second adapter: a canned provider. No network, no billing, and
    /// chosen answers — whatever the test needs the verdict to say.
    #[derive(Debug, Clone)]
    struct FakeDecide {
        result: Result<Decision, String>,
    }

    impl FakeDecide {
        fn ok() -> Self {
            Self {
                result: Ok(Decision {
                    answers: [(
                        "actionable".to_owned(),
                        oarfish_jev::Answer::Noul { noul: 0.88 },
                    )]
                    .into_iter()
                    .collect(),
                    model: "fake-build-1".to_owned(),
                    usage: None,
                }),
            }
        }

        fn failing() -> Self {
            Self {
                result: Err("the provider is down".to_owned()),
            }
        }
    }

    impl Decide for FakeDecide {
        fn decide<'a>(
            &'a self,
            _state: &'a serde_json::Value,
            _questions: &'a std::collections::BTreeMap<String, Question>,
        ) -> impl std::future::Future<Output = Result<Decision, oarfish_jev::Error>> + Send + 'a
        {
            let result = self.result.clone().map_err(oarfish_jev::Error::Parse);
            async move { result }
        }
    }

    fn shared(dir: &std::path::Path) -> Arc<Shared> {
        let db = fjall::Database::builder(dir)
            .open()
            .expect("temp store opens");
        let keyspace = |db: &fjall::Database, name: &str| {
            db.keyspace(name, fjall::KeyspaceCreateOptions::default)
                .expect("keyspace opens")
        };
        let verdicts = keyspace(&db, "verdicts");
        let records = keyspace(&db, "records");
        let records_by_template = keyspace(&db, "records_by_template");
        Shared::new(
            db,
            crate::shared::Keyspaces {
                verdicts,
                records,
                records_by_template,
            },
            64,
            Default::default(),
            oarfish_core::QuestionsHash::of(b"{}"),
            oarfish_mask::BundleHash::from_bytes([7u8; 32]),
        )
    }

    fn job() -> JudgeJob {
        JudgeJob {
            template_id: TemplateId::of("task <VAR:NUM> failed"),
            template: "task <VAR:NUM> failed".to_owned(),
        }
    }

    #[tokio::test]
    async fn one_judgment_lands_a_verdict_a_record_and_a_cache_entry() {
        let dir = std::env::temp_dir().join(format!("oarfish-judge-test-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let shared = shared(&dir);
        let decide = FakeDecide::ok();

        judge_one(&shared, &decide, job()).await;

        let verdict = crate::verdict_cache::read_latest(&shared, &job().template_id)
            .expect("read")
            .expect("judged");
        assert_eq!(verdict.model, "fake-build-1");
        assert!(
            shared
                .cache
                .get(&(job().template_id, shared.bundle_hash))
                .is_some()
        );
        let cache = crate::verdict_cache::VerdictCache::new(Arc::clone(&shared));
        assert_eq!(cache.records_for_template(&job().template_id).len(), 1);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn a_failed_call_judges_nothing_and_leaves_no_record() {
        let dir =
            std::env::temp_dir().join(format!("oarfish-judge-fail-test-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let shared = shared(&dir);
        let decide = FakeDecide::failing();

        judge_one(&shared, &decide, job()).await;

        assert!(
            crate::verdict_cache::read_latest(&shared, &job().template_id)
                .expect("read")
                .is_none()
        );
        let cache = crate::verdict_cache::VerdictCache::new(Arc::clone(&shared));
        assert!(cache.records_for_template(&job().template_id).is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The seam, proven through the real wiring: a spawned judge judging
    /// through the fake lands a verdict the cache serves, with no network
    /// anywhere in the path.
    #[tokio::test]
    async fn a_spawned_judge_with_a_fake_judges_through_the_wiring() {
        let dir =
            std::env::temp_dir().join(format!("oarfish-judge-spawn-test-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let shared = shared(&dir);
        let judge = Judge::spawn(Arc::clone(&shared), FakeDecide::ok(), 16, 2);
        let cache = crate::verdict_cache::VerdictCache::new(Arc::clone(&shared));

        judge.enqueue(job().template_id, &job().template);
        let id = job().template_id;
        let verdict = tokio::time::timeout(std::time::Duration::from_secs(5), async {
            loop {
                if let Some(verdict) = cache.lookup(&id) {
                    break verdict;
                }
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("the fake judges within the timeout");
        assert_eq!(verdict.model, "fake-build-1");

        judge.close().await.expect("close");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
