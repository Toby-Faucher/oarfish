//! The state the verdict cache and the judge share.
//!
//! One `fjall` database holds four keyspaces; `moka` fronts the verdict
//! reads. Both the cache-aside reads and the background judge touch the same
//! handles, so they live in one reference-counted module both are built
//! over — rather than each threading five handles through every call.

use std::collections::BTreeMap;
use std::sync::Arc;

use moka::sync::Cache;
use oarfish_core::{QuestionsHash, TemplateId, Verdict};
use oarfish_jev::Question;
use oarfish_mask::BundleHash;

/// The handles behind [`crate::VerdictCache`] and the judge: the database,
/// its keyspaces, the memory cache, and the question set (with its hash and
/// the bundle hash) every key is scoped under.
pub(crate) struct Shared {
    pub db: fjall::Database,
    pub verdicts: fjall::Keyspace,
    pub records: fjall::Keyspace,
    pub records_by_template: fjall::Keyspace,
    pub cache: Cache<(TemplateId, BundleHash), Verdict>,
    pub questions: BTreeMap<String, Question>,
    pub questions_hash: QuestionsHash,
    pub bundle_hash: BundleHash,
}

/// The verdict-side keyspaces, opened together by the composition root.
pub(crate) struct Keyspaces {
    pub verdicts: fjall::Keyspace,
    pub records: fjall::Keyspace,
    pub records_by_template: fjall::Keyspace,
}

impl Shared {
    pub fn new(
        db: fjall::Database,
        keys: Keyspaces,
        cache_capacity: u64,
        questions: BTreeMap<String, Question>,
        questions_hash: QuestionsHash,
        bundle_hash: BundleHash,
    ) -> Arc<Self> {
        Arc::new(Self {
            db,
            verdicts: keys.verdicts,
            records: keys.records,
            records_by_template: keys.records_by_template,
            cache: Cache::new(cache_capacity),
            questions,
            questions_hash,
            bundle_hash,
        })
    }
}
