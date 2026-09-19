//! The merge cache: a close pair is judged once, and both answers are kept.
//!
//! [`Merges`] is the composition root over [`MergeCache`] and [`MergeJudge`],
//! built to the shape [`Verdicts`] proved: a bounded queue, a background
//! task, cache-aside over its keyspace, and the provider behind [`Decide`].
//! The pipeline refers pairs; the engine resolves through the alias table
//! before it touches windows.

use std::collections::{BTreeMap, HashMap};
use std::sync::{Arc, RwLock};

use moka::sync::Cache;
use oarfish_core::{QuestionsHash, TemplateId};
use oarfish_jev::{Client, Question};
use oarfish_mask::BundleHash;

use crate::merge_cache::{MergeCache, MergeShared};
use crate::merge_decision::MergeDecision;
use crate::merge_judge::MergeJudge;
use crate::merge_keys::canonical_pair;
use crate::verdicts::Error;

/// The `merges` keyspace: [`merge_key`](crate::merge_key) →
/// `postcard` [`MergeDecision`].
pub const MERGES_KEYSPACE: &str = "merges";

/// Bounded on purpose: the log stream is the retry mechanism. Referrals run
/// perhaps a dozen times a day, so the queue is small.
pub const DEFAULT_MERGE_QUEUE_CAPACITY: usize = 256;
/// One question, one call at a time in the common case; two lets a cold start
/// drain without serializing.
pub const DEFAULT_MERGE_JUDGE_CONCURRENCY: usize = 2;
/// A coin flip merges. A starting point to be tuned against real data, like
/// every threshold in this system — but note the asymmetry: a wrong merge
/// silently deletes the alarm, so tune up, never down, without evidence.
pub const DEFAULT_MERGE_THRESHOLD: f64 = 0.5;

/// Tuning for [`Merges::open`]. Defaults are the constants above.
#[derive(Debug, Clone)]
pub struct MergesConfig {
    pub queue_capacity: usize,
    pub judge_concurrency: usize,
    pub merge_threshold: f64,
}

impl Default for MergesConfig {
    fn default() -> Self {
        Self {
            queue_capacity: DEFAULT_MERGE_QUEUE_CAPACITY,
            judge_concurrency: DEFAULT_MERGE_JUDGE_CONCURRENCY,
            merge_threshold: DEFAULT_MERGE_THRESHOLD,
        }
    }
}

/// The cache-aside merge store: the composition root.
///
/// The question set is fixed per instance — one `noul`, policy owned by
/// `oarfish-engine` and taken here as a parameter — as is the mask bundle's
/// identity: a bundle edit must miss the cache and re-judge rather than serve
/// a merge meant for text another bundle produced.
pub struct Merges {
    shared: Arc<MergeShared>,
    cache: MergeCache,
    judge: MergeJudge,
}

impl Merges {
    /// Open over already-open keyspaces with default tuning. Call from within
    /// a Tokio runtime: it spawns the background merge-judge task. Loads the
    /// alias table from the keyspace before returning.
    pub fn open(
        merges: fjall::Keyspace,
        records: fjall::Keyspace,
        records_by_template: fjall::Keyspace,
        client: Client,
        questions: BTreeMap<String, Question>,
        bundle_hash: BundleHash,
    ) -> Self {
        Self::open_with(
            merges,
            records,
            records_by_template,
            client,
            questions,
            bundle_hash,
            MergesConfig::default(),
        )
    }

    /// Open with explicit tuning. Tests use this to shrink the queue.
    pub fn open_with(
        merges: fjall::Keyspace,
        records: fjall::Keyspace,
        records_by_template: fjall::Keyspace,
        client: Client,
        questions: BTreeMap<String, Question>,
        bundle_hash: BundleHash,
        config: MergesConfig,
    ) -> Self {
        let questions_hash = oarfish_jev::questions_hash(&questions);
        let shared = Arc::new(MergeShared {
            merges,
            records,
            records_by_template,
            cache: Cache::new(crate::verdicts::DEFAULT_CACHE_CAPACITY),
            aliases: RwLock::new(HashMap::new()),
            questions,
            questions_hash,
            bundle_hash,
            merge_threshold: config.merge_threshold,
        });
        let judge = MergeJudge::spawn(
            Arc::clone(&shared),
            client,
            config.queue_capacity,
            config.judge_concurrency,
        );
        let cache = MergeCache::new(Arc::clone(&shared));
        cache.load_aliases();
        Self {
            shared,
            cache,
            judge,
        }
    }

    /// The question-set hash this instance judges under. A reworded question
    /// changes this, which moves every key.
    pub fn questions_hash(&self) -> QuestionsHash {
        self.shared.questions_hash
    }

    /// Refer one pair for review. Canonically ordered, so `(A, B)` and
    /// `(B, A)` are one referral; a self-pair is ignored. Decided pairs —
    /// `yes` or `no` — are not re-enqueued: the second sighting fires zero
    /// further requests.
    pub fn refer(&self, id_a: TemplateId, template_a: &str, id_b: TemplateId, template_b: &str) {
        if id_a == id_b {
            return;
        }
        let (id_lo, template_lo, id_hi, template_hi) =
            canonical_pair(id_a, template_a, id_b, template_b);
        if self.cache.lookup(&id_lo, &id_hi).is_some() {
            return;
        }
        self.judge.enqueue(id_lo, &template_lo, id_hi, &template_hi);
    }

    /// Resolve one template through the alias table: the survivor's id and
    /// text, or the input id with `None` when unmerged. The every-line read:
    /// one in-memory lookup per hop, strictly downward.
    pub fn resolve(&self, id: &TemplateId) -> (TemplateId, Option<String>) {
        self.cache.resolve(id)
    }

    /// The cached decision for one pair, either answer. A replay and
    /// debugging read, not a hot-path read.
    pub fn lookup(&self, id_a: &TemplateId, id_b: &TemplateId) -> Option<MergeDecision> {
        if id_a == id_b {
            return None;
        }
        let (id_lo, _, id_hi, _) = canonical_pair(*id_a, "", *id_b, "");
        self.cache.lookup(&id_lo, &id_hi)
    }

    /// The exact-key read, for replay and debugging.
    pub fn by_key(
        &self,
        id_lo: &TemplateId,
        id_hi: &TemplateId,
        questions_hash: &QuestionsHash,
        bundle_hash: &BundleHash,
        model: &str,
    ) -> Option<MergeDecision> {
        self.cache
            .by_key(id_lo, id_hi, questions_hash, bundle_hash, model)
    }

    /// Delete every decision for one pair and unhook its alias. A bad merge
    /// is undone by deleting one row.
    pub fn remove_pair(&self, id_a: &TemplateId, id_b: &TemplateId) -> Result<(), fjall::Error> {
        if id_a == id_b {
            return Ok(());
        }
        let (id_lo, _, id_hi, _) = canonical_pair(*id_a, "", *id_b, "");
        self.cache.remove_pair(&id_lo, &id_hi)
    }

    /// The alias table, for tests and debugging: merged-away id → survivor.
    /// The engine never reads this directly; it resolves per line.
    pub fn aliases(
        &self,
    ) -> std::collections::HashMap<TemplateId, crate::merge_cache::AliasTarget> {
        self.shared
            .aliases
            .read()
            .map(|map| map.clone())
            .unwrap_or_default()
    }

    /// Shut the merge judge down and flush. Consumes the store.
    pub async fn close(self) -> Result<(), Error> {
        self.judge.close().await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn config_defaults_match_the_documented_constants() {
        let config = MergesConfig::default();
        assert_eq!(config.queue_capacity, DEFAULT_MERGE_QUEUE_CAPACITY);
        assert_eq!(config.judge_concurrency, DEFAULT_MERGE_JUDGE_CONCURRENCY);
        assert_eq!(config.merge_threshold, DEFAULT_MERGE_THRESHOLD);
    }
}
