//! The verdict cache: judged once, served from memory, explained by records.
//!
//! [`Verdicts`] is the composition root over three modules, each with one
//! job:
//!
//! - [`crate::VerdictCache`] serves the cache-aside reads: moka, then fjall,
//!   then `None`.
//! - [`crate::AlarmStore`] persists the engine's open alarms.
//! - [`crate::Judge`] spends the 1–2s per new template off the every-line
//!   path, judging through the [`Decide`] adapter.
//!
//! The cost is a dependency edge from `oarfish-store` to `oarfish-jev`: a
//! chain where the rest of the workspace is a star, accepted deliberately.
//! The alternative is opening `oarfish-engine` a milestone early, where the
//! boundary of "just the caching part" would blur under the first thing that
//! needed it.

use std::collections::BTreeMap;
use std::path::Path;
use std::sync::Arc;

use oarfish_core::{Alarm, AlarmId, QuestionsHash, TemplateId, Verdict};
use oarfish_jev::{Client, Question};
use oarfish_mask::BundleHash;

use crate::alarm_store::AlarmStore;
use crate::alarms::ALARMS_KEYSPACE;
use crate::judge::Judge;
use crate::merges::{MERGES_KEYSPACE, Merges};
use crate::record::{DecisionRecord, record_key};
use crate::shared::{Keyspaces, Shared};
use crate::verdict_cache::VerdictCache;

/// The `verdicts` keyspace: [`verdict_key`](crate::verdict_key) →
/// `postcard` [`Verdict`].
pub const VERDICTS_KEYSPACE: &str = "verdicts";
/// The `records` keyspace: [`record_key`](crate::record_key) →
/// `postcard` [`DecisionRecord`].
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

/// The cache-aside verdict store: the composition root.
///
/// Call [`Verdicts::open`] from within a Tokio runtime: it spawns the
/// background judge task. The question set is fixed per instance — it is
/// policy owned by `oarfish-engine`, taken here as a parameter and hashed
/// into every key — as is the mask bundle's identity: a bundle edit must
/// miss the cache and re-judge rather than serve a verdict meant for text
/// another bundle produced. The merge question set rides along the same way:
/// this opens the `merges` keyspace on the same database and builds the
/// [`Merges`] sibling the pipeline refers pairs to and the engine resolves
/// through.
pub struct Verdicts {
    shared: Arc<Shared>,
    cache: VerdictCache,
    alarms: AlarmStore,
    judge: Judge,
    merges: Arc<Merges>,
}

impl Verdicts {
    /// Open (or create) the store at `path` with default tuning.
    pub fn open(
        path: impl AsRef<Path>,
        client: Client,
        questions: BTreeMap<String, Question>,
        merge_questions: BTreeMap<String, Question>,
        bundle_hash: BundleHash,
    ) -> Result<Self, Error> {
        Self::open_with(
            path,
            client,
            questions,
            merge_questions,
            bundle_hash,
            VerdictsConfig::default(),
        )
    }

    /// Open with explicit tuning. Tests use this to shrink the queue.
    pub fn open_with(
        path: impl AsRef<Path>,
        client: Client,
        questions: BTreeMap<String, Question>,
        merge_questions: BTreeMap<String, Question>,
        bundle_hash: BundleHash,
        config: VerdictsConfig,
    ) -> Result<Self, Error> {
        let path = path.as_ref();
        let db = fjall::Database::builder(path)
            .open()
            .map_err(|e| Error::Open(path.display().to_string(), e))?;
        let keyspace = |name: &str| {
            db.keyspace(name, fjall::KeyspaceCreateOptions::default)
                .map_err(|e| Error::Open(path.display().to_string(), e))
        };
        let verdicts = keyspace(VERDICTS_KEYSPACE)?;
        let records = keyspace(RECORDS_KEYSPACE)?;
        let records_by_template = keyspace(RECORDS_BY_TEMPLATE_KEYSPACE)?;
        let alarms = keyspace(ALARMS_KEYSPACE)?;
        let merges_space = keyspace(MERGES_KEYSPACE)?;

        let questions_hash = oarfish_jev::questions_hash(&questions);
        let keys = Keyspaces {
            verdicts,
            records: records.clone(),
            records_by_template: records_by_template.clone(),
        };
        let shared = Shared::new(
            db,
            keys,
            config.cache_capacity,
            questions,
            questions_hash,
            bundle_hash,
        );
        let judge = Judge::spawn(
            Arc::clone(&shared),
            client.clone(),
            config.queue_capacity,
            config.judge_concurrency,
        );
        let merges = Arc::new(Merges::open(
            merges_space,
            records,
            records_by_template,
            client,
            merge_questions,
            bundle_hash,
        ));

        Ok(Self {
            cache: VerdictCache::new(Arc::clone(&shared)),
            alarms: AlarmStore::new(alarms),
            judge,
            merges,
            shared,
        })
    }

    /// The merge store: the pipeline refers close pairs here, and the engine
    /// resolves through its alias table before touching windows.
    pub fn merges(&self) -> &Merges {
        &self.merges
    }

    /// A shared handle to the merge store, for the pipeline: it refers close
    /// pairs from its own task while the engine resolves through the same
    /// table.
    pub fn merges_handle(&self) -> Arc<Merges> {
        Arc::clone(&self.merges)
    }

    /// Wire the engine's notification channel in, once it exists. Call this
    /// after both `Verdicts` and the engine are built — the engine can't be
    /// constructed before the store it depends on is, so this is a
    /// post-construction step rather than a constructor parameter. A second
    /// call is a no-op: the channel is set once, for the life of the store.
    pub fn set_judged_notifier(&self, notify: tokio::sync::mpsc::Sender<(TemplateId, String)>) {
        let _ = self.shared.judged_notify.set(notify);
    }

    /// The question-set hash this instance judges under. A reworded question
    /// changes this, which moves every key.
    pub fn questions_hash(&self) -> QuestionsHash {
        self.shared.questions_hash
    }

    /// The every-line read: moka, then fjall, then an enqueue and `None`.
    ///
    /// Takes the template text as well as the id because the judge needs
    /// the state, not just the key. Synchronous and infallible by design — a
    /// storage fault logs and degrades to the same unjudged lane as a miss.
    pub fn verdict_for(&self, template_id: &TemplateId, template: &str) -> Option<Verdict> {
        if let Some(hit) = self.cache.lookup(template_id) {
            return Some(hit);
        }
        self.judge.enqueue(*template_id, template);
        None
    }

    /// The exact-key read, for replay and debugging. A changed question set,
    /// a changed bundle, or a changed resolved model misses here even when
    /// other verdicts for the template exist — that is the key doing its job.
    pub fn verdict_by_key(
        &self,
        template_id: &TemplateId,
        questions_hash: &QuestionsHash,
        bundle_hash: &BundleHash,
        model: &str,
    ) -> Option<Verdict> {
        self.cache
            .by_key(template_id, questions_hash, bundle_hash, model)
    }

    /// Every verdict one template has ever received, oldest first. The
    /// answer to "why did this wake me three weeks ago" at the template
    /// level. A replay and debugging helper, not a hot-path read.
    pub fn verdicts_for_template(&self, template_id: &TemplateId) -> Vec<Verdict> {
        self.cache.for_template(template_id)
    }

    /// Every decision record for one template, oldest first. A prefix scan
    /// over the `records_by_template` secondary index (`template_id ++
    /// record_ulid`), so the cost stays flat for the life of the install.
    /// Pre-index databases fall back to the legacy full scan once.
    pub fn records_for_template(&self, template_id: &TemplateId) -> Vec<DecisionRecord> {
        self.cache.records_for_template(template_id)
    }

    /// Persist one decision record built elsewhere — the contextual check's,
    /// which the engine assembles rather than the judge. Writes the record
    /// and its secondary-index entry; a failure surfaces so the caller can
    /// degrade to the failure lane instead of trusting an unrecorded answer.
    pub fn save_record(&self, record: &DecisionRecord) -> Result<(), fjall::Error> {
        let bytes = postcard::to_stdvec(record).expect("a DecisionRecord in memory always encodes");
        self.shared.records.insert(record_key(&record.id), bytes)?;
        self.shared.records_by_template.insert(
            crate::verdict_cache::record_by_template_key(&record.template_id, &record.id),
            [],
        )?;
        Ok(())
    }

    /// Save one open alarm. The engine calls this on raise and on every
    /// dedupe bump, so what the board reads and what a restart reloads are
    /// the same record. A write failure surfaces: unlike a verdict read,
    /// losing an open alarm is a silent failure, never a safe lane.
    pub fn save_alarm(&self, alarm: &Alarm) -> Result<(), fjall::Error> {
        self.alarms.save(alarm)
    }

    /// Delete one open alarm. Called on clear, paired with every save.
    pub fn remove_alarm(&self, id: &AlarmId) -> Result<(), fjall::Error> {
        self.alarms.remove(id)
    }

    /// Every open alarm, oldest first. The engine reloads these on boot and
    /// re-arms their timers; unreadable entries are skipped with a warning
    /// rather than failing the boot.
    pub fn load_open_alarms(&self) -> Vec<Alarm> {
        self.alarms.load_open()
    }

    /// Flush the journal so a reopen sees everything written so far. The
    /// database also persists on drop; this is the explicit form for tests
    /// and orderly shutdown.
    pub fn persist(&self) -> Result<(), fjall::Error> {
        self.shared.db.persist(fjall::PersistMode::SyncAll)
    }

    /// Shut the judges down and flush. Consumes the store; the database
    /// persists on drop. When the pipeline still holds a merge handle the
    /// explicit merge-judge shutdown is skipped — the task ends once the
    /// last handle drops — and the database still persists on drop.
    pub async fn close(self) -> Result<(), Error> {
        self.judge.close().await?;
        match Arc::try_unwrap(self.merges) {
            Ok(merges) => merges.close().await,
            Err(_) => {
                tracing::debug!("merge handle still shared; the merge judge ends on drop");
                Ok(())
            }
        }
    }
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
