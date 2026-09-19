//! The verdict cache: judged once, served from memory, explained by records.
//!
//! `Verdicts` holds the moka cache, the fjall keyspaces and — through the
//! background [`Judge`] — the Jev client, and exposes the cache-aside flow.
//! This puts the policy next to the cache it manages, and makes M4 provably
//! complete on its own rather than waiting for M5 to join two libraries.
//!
//! The cost is a dependency edge from `oarfish-store` to `oarfish-jev`: a
//! chain where the rest of the workspace is a star, accepted deliberately.
//! The alternative is opening `oarfish-engine` a milestone early, where the
//! boundary of "just the caching part" would blur under the first thing that
//! needed it.
//!
//! [`Verdicts::verdict_for`] is synchronous and returns `Option<Verdict>`
//! rather than a `Result`, because invariant 2 forbids a network call on the
//! every-line path:
//!
//! 1. moka hit — return it.
//! 2. moka miss — read fjall, populate moka, return it.
//! 3. fjall miss — `try_send` the template to the judge and return `None`
//!    **immediately**.
//!
//! A fjall read error is logged and treated as a miss: the every-line path
//! has no useful response to a storage fault, and degrading to "unjudged, so
//! dashboard-only" is the same safe lane a genuine miss takes. An unjudged
//! template surfaces and never pages — Jev being unreachable degrades to
//! "nothing new pages" rather than stalling ingest or paging on everything
//! unknown.
//!
//! A dropped enqueue is harmless. If the queue is full the send fails and
//! nothing happens; the next line carrying that template misses again and
//! re-enqueues. The queue needs no unbounded growth, no retry logic and no
//! persistence, because the log stream is itself the retry mechanism.

use std::collections::{BTreeMap, HashSet};
use std::path::Path;
use std::sync::Arc;

use moka::sync::Cache;
use oarfish_core::{Alarm, AlarmId, QuestionsHash, TemplateId, Verdict};
use oarfish_jev::{Client, Question};
use time::OffsetDateTime;
use tokio::sync::{Mutex, Semaphore, mpsc};
use ulid::Ulid;

use crate::alarms::{ALARMS_KEYSPACE, alarm_key, decode_alarm, encode_alarm};
use crate::keys::{verdict_key, verdict_questions_prefix, verdict_template_prefix};
use crate::record::{DecisionRecord, record_key};

/// The `verdicts` keyspace: [`verdict_key`] → `postcard` [`Verdict`].
pub const VERDICTS_KEYSPACE: &str = "verdicts";
/// The `records` keyspace: [`record_key`] → `postcard` [`DecisionRecord`].
pub const RECORDS_KEYSPACE: &str = "records";
/// The `records_by_template` secondary index: `template_id ++ record_ulid`
/// → empty. One extra write per judged template — never per line — turns
/// [`Verdicts::records_for_template`] from a full keyspace scan into a
/// prefix scan that stays flat for the life of the install.
pub const RECORDS_BY_TEMPLATE_KEYSPACE: &str = "records_by_template";

/// Moka entries. Sized by "templates a homelab has", shared with the Drain
/// table's 65_536: one fewer thing to tune. Eviction costs a fjall read,
/// never a re-judge.
pub const DEFAULT_CACHE_CAPACITY: u64 = 65_536;
/// Bounded on purpose: the log stream is the retry mechanism.
pub const DEFAULT_QUEUE_CAPACITY: usize = 1024;
/// A first run meets a few hundred new templates at once; at 1–2s per call a
/// sequential judge would leave the board empty for minutes.
pub const DEFAULT_JUDGE_CONCURRENCY: usize = 4;

/// Tuning for [`Verdicts::open_with`]. Defaults are the constants above.
#[derive(Debug, Clone)]
pub struct VerdictsConfig {
    pub cache_capacity: u64,
    pub queue_capacity: usize,
    pub judge_concurrency: usize,
}

impl Default for VerdictsConfig {
    fn default() -> Self {
        Self {
            cache_capacity: DEFAULT_CACHE_CAPACITY,
            queue_capacity: DEFAULT_QUEUE_CAPACITY,
            judge_concurrency: DEFAULT_JUDGE_CONCURRENCY,
        }
    }
}

/// What can go wrong opening or closing the store. Reads on the every-line
/// path never surface errors — they log and degrade to a miss — so this only
/// covers setup and shutdown.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("open store at {0}: {1}")]
    Open(String, #[source] fjall::Error),
    #[error(transparent)]
    Join(#[from] tokio::task::JoinError),
}

/// One unit of judging: the id for the key, the text for the state. The call
/// needs the template, not just its id.
#[derive(Debug, Clone)]
struct JudgeJob {
    template_id: TemplateId,
    template: String,
}

struct Inner {
    db: fjall::Database,
    verdicts: fjall::Keyspace,
    records: fjall::Keyspace,
    records_by_template: fjall::Keyspace,
    alarms: fjall::Keyspace,
    cache: Cache<TemplateId, Verdict>,
    questions: BTreeMap<String, Question>,
    questions_hash: QuestionsHash,
}

/// The cache-aside verdict store.
///
/// Call [`Verdicts::open`] from within a Tokio runtime: it spawns the
/// background judge task. The question set is fixed per instance — it is
/// policy owned by `oarfish-engine`, taken here as a parameter and hashed
/// into every key.
pub struct Verdicts {
    inner: Arc<Inner>,
    sender: mpsc::Sender<JudgeJob>,
    judge_handle: Mutex<Option<tokio::task::JoinHandle<()>>>,
}

impl Verdicts {
    /// Open (or create) the store at `path` with default tuning.
    pub fn open(
        path: impl AsRef<Path>,
        client: Client,
        questions: BTreeMap<String, Question>,
    ) -> Result<Self, Error> {
        Self::open_with(path, client, questions, VerdictsConfig::default())
    }

    /// Open with explicit tuning. Tests use this to shrink the queue.
    pub fn open_with(
        path: impl AsRef<Path>,
        client: Client,
        questions: BTreeMap<String, Question>,
        config: VerdictsConfig,
    ) -> Result<Self, Error> {
        let path = path.as_ref();
        let db = fjall::Database::builder(path)
            .open()
            .map_err(|e| Error::Open(path.display().to_string(), e))?;
        let verdicts = db
            .keyspace(VERDICTS_KEYSPACE, fjall::KeyspaceCreateOptions::default)
            .map_err(|e| Error::Open(path.display().to_string(), e))?;
        let records = db
            .keyspace(RECORDS_KEYSPACE, fjall::KeyspaceCreateOptions::default)
            .map_err(|e| Error::Open(path.display().to_string(), e))?;
        let records_by_template = db
            .keyspace(
                RECORDS_BY_TEMPLATE_KEYSPACE,
                fjall::KeyspaceCreateOptions::default,
            )
            .map_err(|e| Error::Open(path.display().to_string(), e))?;
        let alarms = db
            .keyspace(ALARMS_KEYSPACE, fjall::KeyspaceCreateOptions::default)
            .map_err(|e| Error::Open(path.display().to_string(), e))?;

        let questions_hash = oarfish_jev::questions_hash(&questions);
        let inner = Arc::new(Inner {
            db,
            verdicts,
            records,
            records_by_template,
            alarms,
            cache: Cache::new(config.cache_capacity),
            questions,
            questions_hash,
        });

        let (sender, receiver) = mpsc::channel(config.queue_capacity);
        let handle = tokio::spawn(judge_loop(
            Arc::clone(&inner),
            client,
            receiver,
            config.judge_concurrency,
        ));

        Ok(Self {
            inner,
            sender,
            judge_handle: Mutex::new(Some(handle)),
        })
    }

    /// The question-set hash this instance judges under. A reworded question
    /// changes this, which moves every key.
    pub fn questions_hash(&self) -> QuestionsHash {
        self.inner.questions_hash
    }

    /// The every-line read: moka, then fjall, then an enqueue and `None`.
    ///
    /// Takes the template text as well as the id because step 3 needs the
    /// state, not just the key. Synchronous and infallible by design — a
    /// storage fault logs and degrades to the same unjudged lane as a miss.
    pub fn verdict_for(&self, template_id: &TemplateId, template: &str) -> Option<Verdict> {
        if let Some(hit) = self.inner.cache.get(template_id) {
            return Some(hit);
        }
        match read_latest(&self.inner, template_id) {
            Ok(Some(verdict)) => {
                self.inner.cache.insert(*template_id, verdict.clone());
                Some(verdict)
            }
            Ok(None) => {
                self.enqueue(*template_id, template);
                None
            }
            Err(error) => {
                tracing::warn!(
                    template_id = %template_id,
                    %error,
                    "verdict read failed; degrading to unjudged"
                );
                self.enqueue(*template_id, template);
                None
            }
        }
    }

    /// The exact-key read, for replay and debugging. A changed question set
    /// or a changed resolved model misses here even when other verdicts for
    /// the template exist — that is the key doing its job.
    pub fn verdict_by_key(
        &self,
        template_id: &TemplateId,
        questions_hash: &QuestionsHash,
        model: &str,
    ) -> Option<Verdict> {
        let key = verdict_key(template_id, questions_hash, model);
        match self.inner.verdicts.get(&key) {
            Ok(Some(bytes)) => match postcard::from_bytes::<Verdict>(bytes.as_ref()) {
                Ok(verdict) => Some(verdict),
                Err(error) => {
                    tracing::warn!(
                        template_id = %template_id,
                        %error,
                        "cached verdict would not decode; treating as absent"
                    );
                    None
                }
            },
            Ok(None) => None,
            Err(error) => {
                tracing::warn!(
                    template_id = %template_id,
                    %error,
                    "verdict read failed; treating as absent"
                );
                None
            }
        }
    }

    /// Every verdict one template has ever received, oldest first. The
    /// answer to "why did this wake me three weeks ago" at the template
    /// level. A replay and debugging helper, not a hot-path read.
    pub fn verdicts_for_template(&self, template_id: &TemplateId) -> Vec<Verdict> {
        let prefix = verdict_template_prefix(template_id);
        let mut out = Vec::new();
        for guard in self.inner.verdicts.prefix(prefix) {
            let bytes = match guard.value() {
                Ok(bytes) => bytes,
                Err(error) => {
                    tracing::warn!(%error, "verdict scan hit an unreadable entry");
                    continue;
                }
            };
            match postcard::from_bytes::<Verdict>(bytes.as_ref()) {
                Ok(verdict) => {
                    if verdict.template_id == *template_id {
                        out.push(verdict);
                    }
                }
                Err(error) => {
                    tracing::warn!(%error, "cached verdict would not decode; skipping");
                }
            }
        }
        out.sort_by_key(|verdict| verdict.judged_at);
        out
    }

    /// Every decision record for one template, oldest first. A prefix scan
    /// over the `records_by_template` secondary index (`template_id ++
    /// record_ulid`), so the cost stays flat for the life of the install.
    /// Pre-index databases fall back to the legacy full scan once.
    pub fn records_for_template(&self, template_id: &TemplateId) -> Vec<DecisionRecord> {
        let mut out = Vec::new();
        let mut indexed = false;
        for guard in self
            .inner
            .records_by_template
            .prefix(template_id.as_bytes())
        {
            indexed = true;
            let key = match guard.key() {
                Ok(key) => key,
                Err(error) => {
                    tracing::warn!(%error, "record index hit an unreadable key");
                    continue;
                }
            };
            let bytes = key.as_ref();
            if bytes.len() != 48 {
                continue;
            }
            let mut ulid_bytes = [0u8; 16];
            ulid_bytes.copy_from_slice(&bytes[32..]);
            let record_bytes = match self.inner.records.get(ulid_bytes) {
                Ok(Some(record_bytes)) => record_bytes,
                Ok(None) => continue,
                Err(error) => {
                    tracing::warn!(%error, "record read hit an unreadable entry");
                    continue;
                }
            };
            match postcard::from_bytes::<DecisionRecord>(record_bytes.as_ref()) {
                Ok(record) => {
                    if record.template_id == *template_id {
                        out.push(record);
                    }
                }
                Err(error) => {
                    tracing::warn!(%error, "decision record would not decode; skipping");
                }
            }
        }
        if indexed {
            out.sort_by(|a, b| {
                a.recorded_at_unix
                    .cmp(&b.recorded_at_unix)
                    .then_with(|| a.id.to_string().cmp(&b.id.to_string()))
            });
            return out;
        }
        // No index entries: a database written before the secondary index.
        // One legacy full scan keeps those installs readable.
        for guard in self.inner.records.range([0u8; 16]..=[0xFF; 16]) {
            let bytes = match guard.value() {
                Ok(bytes) => bytes,
                Err(error) => {
                    tracing::warn!(%error, "record scan hit an unreadable entry");
                    continue;
                }
            };
            match postcard::from_bytes::<DecisionRecord>(bytes.as_ref()) {
                Ok(record) => {
                    if record.template_id == *template_id {
                        out.push(record);
                    }
                }
                Err(error) => {
                    tracing::warn!(%error, "decision record would not decode; skipping");
                }
            }
        }
        out.sort_by(|a, b| {
            a.recorded_at_unix
                .cmp(&b.recorded_at_unix)
                .then_with(|| a.id.to_string().cmp(&b.id.to_string()))
        });
        out
    }

    /// Save one open alarm. The engine calls this on raise and on every
    /// dedupe bump, so what the board reads and what a restart reloads are
    /// the same record. A write failure surfaces: unlike a verdict read,
    /// losing an open alarm is a silent failure, never a safe lane.
    pub fn save_alarm(&self, alarm: &Alarm) -> Result<(), fjall::Error> {
        let bytes = encode_alarm(alarm).expect("an Alarm in memory always encodes");
        self.inner.alarms.insert(alarm_key(&alarm.id), bytes)?;
        Ok(())
    }

    /// Delete one open alarm. Called on clear, paired with every save.
    pub fn remove_alarm(&self, id: &AlarmId) -> Result<(), fjall::Error> {
        self.inner.alarms.remove(alarm_key(id))?;
        Ok(())
    }

    /// Every open alarm, oldest first. The engine reloads these on boot and
    /// re-arms their timers; unreadable entries are skipped with a warning
    /// rather than failing the boot.
    pub fn load_open_alarms(&self) -> Vec<Alarm> {
        let mut out = Vec::new();
        for guard in self.inner.alarms.range([0u8; 16]..=[0xFF; 16]) {
            let bytes = match guard.value() {
                Ok(bytes) => bytes,
                Err(error) => {
                    tracing::warn!(%error, "alarm scan hit an unreadable entry");
                    continue;
                }
            };
            match decode_alarm(bytes.as_ref()) {
                Ok(alarm) => out.push(alarm),
                Err(error) => {
                    tracing::warn!(%error, "stored alarm would not decode; skipping");
                }
            }
        }
        out.sort_by_key(|alarm| alarm.opened_at);
        out
    }

    /// Flush the journal so a reopen sees everything written so far. The
    /// database also persists on drop; this is the explicit form for tests
    /// and orderly shutdown.
    pub fn persist(&self) -> Result<(), fjall::Error> {
        self.inner.db.persist(fjall::PersistMode::SyncAll)
    }

    /// Shut the judge down and flush. Consumes the store; the database
    /// persists on drop.
    pub async fn close(self) -> Result<(), Error> {
        drop(self.sender);
        if let Some(handle) = self.judge_handle.lock().await.take() {
            handle.await?;
        }
        Ok(())
    }

    fn enqueue(&self, template_id: TemplateId, template: &str) {
        if let Err(error) = self.sender.try_send(JudgeJob {
            template_id,
            template: template.to_owned(),
        }) {
            // Full or closed: harmless. The next line carrying this template
            // misses again and re-enqueues — the log stream is the retry.
            tracing::debug!(
                template_id = %template_id,
                %error,
                "judge queue unavailable; the log stream will retry"
            );
        }
    }
}

/// The secondary-index key for one decision record: `template_id (32B) ++
/// record ULID (16B)`. A prefix scan on the template id lists that
/// template's records in chronological order, because ULID bytes sort by
/// time.
fn record_by_template_key(template_id: &TemplateId, id: &Ulid) -> [u8; 48] {
    let mut key = [0u8; 48];
    key[..32].copy_from_slice(template_id.as_bytes());
    key[32..].copy_from_slice(&id.to_bytes());
    key
}

/// The newest verdict for one template under this instance's question set, if
/// any. Scoped to the 48-byte prefix so question revisions never alias.
fn read_latest(inner: &Inner, template_id: &TemplateId) -> Result<Option<Verdict>, fjall::Error> {
    let prefix = verdict_questions_prefix(template_id, &inner.questions_hash);
    let mut best: Option<Verdict> = None;
    for guard in inner.verdicts.prefix(prefix) {
        let bytes = guard.value()?;
        let verdict: Verdict = match postcard::from_bytes(bytes.as_ref()) {
            Ok(verdict) => verdict,
            Err(error) => {
                tracing::warn!(
                    template_id = %template_id,
                    %error,
                    "cached verdict would not decode; treating as absent"
                );
                continue;
            }
        };
        if verdict.template_id != *template_id || verdict.questions_hash != inner.questions_hash {
            continue;
        }
        let newer = match &best {
            Some(current) => verdict.judged_at > current.judged_at,
            None => true,
        };
        if newer {
            best = Some(verdict);
        }
    }
    Ok(best)
}

/// The off-path task. Owns the client and the in-flight set; the queue
/// carries `(TemplateId, template_text)` because the call needs the state.
async fn judge_loop(
    inner: Arc<Inner>,
    client: Client,
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
        if let Some(cached) = inner.cache.get(&job.template_id) {
            let _ = cached;
            in_flight.lock().await.remove(&job.template_id);
            continue;
        }
        match read_latest(&inner, &job.template_id) {
            Ok(Some(verdict)) => {
                inner.cache.insert(job.template_id, verdict);
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
        let inner = Arc::clone(&inner);
        let client = client.clone();
        let in_flight = Arc::clone(&in_flight);
        let template_id = job.template_id;
        tokio::spawn(async move {
            let _permit = permit;
            judge_one(&inner, &client, job).await;
            in_flight.lock().await.remove(&template_id);
        });
    }
}

/// One judgment: call, then record, then verdict, then moka — in that order.
/// A crash between writes leaves an orphan record, which is harmless and
/// replayable, rather than a cached verdict with no provenance. No retries:
/// a failed call drops its key and the log stream re-enqueues.
async fn judge_one(inner: &Inner, client: &Client, job: JudgeJob) {
    let state = serde_json::json!({
        "template": job.template,
        "template_id": job.template_id.to_string(),
    });
    let decision = match client.decide(&state, &inner.questions).await {
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
    let questions_json = match serde_json::to_string(&inner.questions) {
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
        questions_hash: inner.questions_hash,
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
    let verdict = Verdict {
        template_id: job.template_id,
        questions_hash: inner.questions_hash,
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
    if let Err(error) = inner.records.insert(record_key(&record.id), record_bytes) {
        tracing::warn!(
            template_id = %job.template_id,
            %error,
            "decision record write failed; skipping the verdict so nothing is cached without provenance"
        );
        return;
    }
    if let Err(error) = inner
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
        &verdict.model,
    );
    if let Err(error) = inner.verdicts.insert(key, verdict_bytes) {
        tracing::warn!(
            template_id = %job.template_id,
            %error,
            "verdict write failed; leaving the record orphaned"
        );
        return;
    }
    inner.cache.insert(job.template_id, verdict);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn config_defaults_match_the_documented_constants() {
        let config = VerdictsConfig::default();
        assert_eq!(config.cache_capacity, DEFAULT_CACHE_CAPACITY);
        assert_eq!(config.queue_capacity, DEFAULT_QUEUE_CAPACITY);
        assert_eq!(config.judge_concurrency, DEFAULT_JUDGE_CONCURRENCY);
    }
}
