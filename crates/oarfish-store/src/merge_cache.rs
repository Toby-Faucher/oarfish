//! The merge cache: judged once per pair, resolved from memory.
//!
//! The cache-aside flow mirrors the verdict cache: moka, then fjall, then an
//! enqueue with the log stream as the retry mechanism. Reads are synchronous
//! and infallible by design — a storage fault logs and degrades to
//! "unmerged", which is the safe direction: an unmerged pair keeps two
//! independent alarms rather than risking invariant 4's silent deletion.
//!
//! The alias table is the repair made readable: every `merged` decision maps
//! the greater id to the lesser, held in memory — loaded at startup from the
//! keyspace, updated when a decision lands — and never read from fjall on the
//! hot path. One `HashMap` lookup per line.

use std::collections::{BTreeMap, HashMap};
use std::sync::{Arc, RwLock};

use moka::sync::Cache;
use oarfish_core::{QuestionsHash, TemplateId};
use oarfish_jev::Question;
use oarfish_mask::BundleHash;

use crate::merge_decision::MergeDecision;
use crate::merge_keys::{merge_key, merge_pair_prefix};

/// Resolution walks at most this many alias hops. Every alias points strictly
/// downward, so chains terminate on their own; the cap is a backstop, not the
/// mechanism.
pub const MAX_ALIAS_HOPS: usize = 16;

/// Where a merged template resolves: the surviving id and its text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AliasTarget {
    /// The canonical survivor: always the lesser id.
    pub canonical: TemplateId,
    /// The survivor's template text, for the verdict lookup after resolution.
    pub template: String,
}

/// The state the merge cache and the merge judge share: the keyspaces, the
/// memory cache, the alias table, and the question set (with its hash and the
/// bundle hash) every key is scoped under.
pub(crate) struct MergeShared {
    pub merges: fjall::Keyspace,
    pub records: fjall::Keyspace,
    pub records_by_template: fjall::Keyspace,
    pub cache: Cache<(TemplateId, TemplateId, BundleHash), MergeDecision>,
    pub aliases: RwLock<HashMap<TemplateId, AliasTarget>>,
    pub questions: BTreeMap<String, Question>,
    pub questions_hash: QuestionsHash,
    pub bundle_hash: BundleHash,
    pub merge_threshold: f64,
}

/// The cache-aside merge read side, over shared state.
pub struct MergeCache {
    shared: Arc<MergeShared>,
}

impl MergeCache {
    /// Built by the composition root over shared state.
    pub(crate) fn new(shared: Arc<MergeShared>) -> Self {
        Self { shared }
    }

    /// The referral read: moka, then fjall, then `None`. Takes the canonical
    /// pair — lesser id first — so `(A, B)` and `(B, A)` hit one entry.
    pub fn lookup(&self, id_lo: &TemplateId, id_hi: &TemplateId) -> Option<MergeDecision> {
        let key = (*id_lo, *id_hi, self.shared.bundle_hash);
        if let Some(hit) = self.shared.cache.get(&key)
            && hit.questions_hash == self.shared.questions_hash
        {
            return Some(hit);
        }
        match read_latest_pair(&self.shared, id_lo, id_hi) {
            Ok(Some(decision)) => {
                self.shared.cache.insert(key, decision.clone());
                Some(decision)
            }
            Ok(None) => None,
            Err(error) => {
                tracing::warn!(
                    id_lo = %id_lo,
                    id_hi = %id_hi,
                    %error,
                    "merge read failed; degrading to unmerged"
                );
                None
            }
        }
    }

    /// The exact-key read, for replay and debugging. A changed question, a
    /// changed bundle, or a changed resolved model misses here even when
    /// other decisions for the pair exist — that is the key doing its job.
    pub fn by_key(
        &self,
        id_lo: &TemplateId,
        id_hi: &TemplateId,
        questions_hash: &QuestionsHash,
        bundle_hash: &BundleHash,
        model: &str,
    ) -> Option<MergeDecision> {
        let key = merge_key(id_lo, id_hi, questions_hash, bundle_hash, model);
        match self.shared.merges.get(&key) {
            Ok(Some(bytes)) => match postcard::from_bytes::<MergeDecision>(bytes.as_ref()) {
                Ok(decision) => Some(decision),
                Err(error) => {
                    tracing::warn!(
                        id_lo = %id_lo,
                        id_hi = %id_hi,
                        %error,
                        "cached merge decision would not decode; treating as absent"
                    );
                    None
                }
            },
            Ok(None) => None,
            Err(error) => {
                tracing::warn!(
                    id_lo = %id_lo,
                    id_hi = %id_hi,
                    %error,
                    "merge read failed; treating as absent"
                );
                None
            }
        }
    }

    /// Resolve one template through the alias table: the survivor's id and
    /// text, or the input id with `None` when unmerged. One `HashMap` lookup
    /// per hop, strictly downward, so chains terminate; the hop cap is a
    /// backstop. A poisoned lock degrades to unmerged: a lock failure must
    /// never merge two templates.
    pub fn resolve(&self, id: &TemplateId) -> (TemplateId, Option<String>) {
        let map = match self.shared.aliases.read() {
            Ok(map) => map,
            Err(_) => return (*id, None),
        };
        let mut current = *id;
        let mut template = None;
        for _ in 0..MAX_ALIAS_HOPS {
            match map.get(&current) {
                Some(target) => {
                    current = target.canonical;
                    template = Some(target.template.clone());
                }
                None => break,
            }
        }
        (current, template)
    }

    /// Delete every decision for one pair and unhook its alias, restoring two
    /// independent templates. A bad merge is undone by deleting one row —
    /// cheap reversibility invariant 4's warning is worth more than tidiness.
    /// Storage faults surface: losing a correction silently would be the same
    /// failure as losing an open alarm.
    pub fn remove_pair(&self, id_lo: &TemplateId, id_hi: &TemplateId) -> Result<(), fjall::Error> {
        let prefix = merge_pair_prefix(id_lo, id_hi);
        let mut keys = Vec::new();
        for guard in self.shared.merges.prefix(prefix) {
            keys.push(guard.key()?.to_vec());
        }
        for key in keys {
            self.shared.merges.remove(key)?;
        }
        self.shared
            .cache
            .invalidate(&(*id_lo, *id_hi, self.shared.bundle_hash));
        if let Ok(mut map) = self.shared.aliases.write()
            && map.get(id_hi).is_some_and(|t| t.canonical == *id_lo)
        {
            map.remove(id_hi);
        }
        Ok(())
    }

    /// Load the alias table from the keyspace at startup: every `merged`
    /// decision hooks the greater id to the lesser. Unreadable entries are
    /// skipped with a warning rather than failing the boot; a missed alias
    /// degrades to two independent alarms, never to a wrong merge.
    pub fn load_aliases(&self) {
        let mut loaded = 0;
        for guard in self.shared.merges.range::<Vec<u8>, _>(..) {
            let bytes = match guard.value() {
                Ok(bytes) => bytes,
                Err(error) => {
                    tracing::warn!(%error, "merge scan hit an unreadable entry");
                    continue;
                }
            };
            match postcard::from_bytes::<MergeDecision>(bytes.as_ref()) {
                Ok(decision) => {
                    if decision.merged
                        && decision.questions_hash == self.shared.questions_hash
                        && let Ok(mut map) = self.shared.aliases.write()
                    {
                        map.insert(
                            decision.id_hi,
                            AliasTarget {
                                canonical: decision.id_lo,
                                template: decision.template_lo.clone(),
                            },
                        );
                        loaded += 1;
                    }
                }
                Err(error) => {
                    tracing::warn!(%error, "stored merge decision would not decode; skipping");
                }
            }
        }
        if loaded > 0 {
            tracing::info!(loaded, "merge aliases reloaded after restart");
        }
    }
}

/// The newest decision for one pair under this instance's question set and
/// bundle, if any. Scoped to the 64-byte pair prefix so question revisions
/// and bundle edits never alias — but note the scan is over every model
/// build, newest first: a re-judge under a new build supersedes the old.
pub(crate) fn read_latest_pair(
    shared: &MergeShared,
    id_lo: &TemplateId,
    id_hi: &TemplateId,
) -> Result<Option<MergeDecision>, fjall::Error> {
    let prefix = merge_pair_prefix(id_lo, id_hi);
    let mut best: Option<MergeDecision> = None;
    for guard in shared.merges.prefix(prefix) {
        let bytes = guard.value()?;
        let decision: MergeDecision = match postcard::from_bytes(bytes.as_ref()) {
            Ok(decision) => decision,
            Err(error) => {
                tracing::warn!(
                    id_lo = %id_lo,
                    id_hi = %id_hi,
                    %error,
                    "cached merge decision would not decode; treating as absent"
                );
                continue;
            }
        };
        if decision.id_lo != *id_lo
            || decision.id_hi != *id_hi
            || decision.questions_hash != shared.questions_hash
        {
            continue;
        }
        let newer = match &best {
            Some(current) => decision.judged_at_unix > current.judged_at_unix,
            None => true,
        };
        if newer {
            best = Some(decision);
        }
    }
    Ok(best)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn shared(dir: &std::path::Path) -> Arc<MergeShared> {
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
            cache: Cache::new(64),
            aliases: RwLock::new(HashMap::new()),
            questions: BTreeMap::new(),
            questions_hash: QuestionsHash::of(b"merge"),
            bundle_hash: BundleHash::from_bytes([7u8; 32]),
            merge_threshold: 0.5,
        })
    }

    fn pair() -> (TemplateId, TemplateId) {
        let a = TemplateId::of("backup done files ok");
        let b = TemplateId::of("backup done files ok extra");
        if a <= b { (a, b) } else { (b, a) }
    }

    fn decision(lo: TemplateId, hi: TemplateId, merged: bool) -> MergeDecision {
        MergeDecision {
            id_lo: lo,
            id_hi: hi,
            template_lo: "backup done files ok".to_owned(),
            template_hi: "backup done files ok extra".to_owned(),
            questions_hash: QuestionsHash::of(b"merge"),
            model: "fake-build-1".to_owned(),
            noul: if merged { 0.91 } else { 0.12 },
            merged,
            judged_at_unix: 0,
        }
    }

    fn write(shared: &MergeShared, decision: &MergeDecision) {
        let key = merge_key(
            &decision.id_lo,
            &decision.id_hi,
            &decision.questions_hash,
            &shared.bundle_hash,
            &decision.model,
        );
        let bytes = postcard::to_stdvec(decision).expect("encode");
        shared.merges.insert(key, bytes).expect("insert");
    }

    #[test]
    fn resolution_follows_chains_downward_and_halts() {
        let dir = std::env::temp_dir().join(format!("oarfish-alias-test-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let shared = shared(&dir);
        let cache = MergeCache::new(Arc::clone(&shared));
        let a = TemplateId::of("event a here");
        let b = TemplateId::of("event b here");
        let c = TemplateId::of("event c here");
        let mut ids = [a, b, c];
        ids.sort();
        let (lo, mid, hi) = (ids[0], ids[1], ids[2]);

        // Unmerged ids resolve to themselves with no text.
        assert_eq!(cache.resolve(&hi), (hi, None));

        {
            let mut map = shared.aliases.write().expect("lock");
            map.insert(
                mid,
                AliasTarget {
                    canonical: lo,
                    template: "lo text".to_owned(),
                },
            );
            map.insert(
                hi,
                AliasTarget {
                    canonical: mid,
                    template: "mid text".to_owned(),
                },
            );
        }
        // Chains resolve by iteration to the least id with its text.
        assert_eq!(
            cache.resolve(&hi),
            (lo, Some("lo text".to_owned())),
            "C -> B -> A must resolve to A"
        );
        assert_eq!(cache.resolve(&mid), (lo, Some("lo text".to_owned())));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn removing_the_row_restores_two_independent_templates() {
        let dir = std::env::temp_dir().join(format!("oarfish-unmerge-test-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let shared = shared(&dir);
        let cache = MergeCache::new(Arc::clone(&shared));
        let (lo, hi) = pair();
        write(&shared, &decision(lo, hi, true));
        {
            let mut map = shared.aliases.write().expect("lock");
            map.insert(
                hi,
                AliasTarget {
                    canonical: lo,
                    template: "backup done files ok".to_owned(),
                },
            );
        }
        assert_eq!(
            cache.resolve(&hi),
            (lo, Some("backup done files ok".to_owned()))
        );

        cache.remove_pair(&lo, &hi).expect("remove");
        assert_eq!(cache.resolve(&hi), (hi, None));
        assert!(cache.lookup(&lo, &hi).is_none());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn startup_loads_only_merged_decisions_for_this_question_set() {
        let dir =
            std::env::temp_dir().join(format!("oarfish-alias-load-test-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let shared = shared(&dir);
        let (lo, hi) = pair();
        write(&shared, &decision(lo, hi, true));
        let other = TemplateId::of("unrelated event over here");
        let (x, y) = if lo <= other {
            (lo, other)
        } else {
            (other, lo)
        };
        // A rejection under this question set: cached, never aliased.
        write(
            &shared,
            &MergeDecision {
                id_lo: x,
                id_hi: y,
                template_lo: "x".to_owned(),
                template_hi: "y".to_owned(),
                questions_hash: QuestionsHash::of(b"merge"),
                model: "fake-build-1".to_owned(),
                noul: 0.12,
                merged: false,
                judged_at_unix: 1,
            },
        );
        // A merge under another question set: not ours, not loaded.
        write(
            &shared,
            &MergeDecision {
                id_lo: x,
                id_hi: y,
                template_lo: "x".to_owned(),
                template_hi: "y".to_owned(),
                questions_hash: QuestionsHash::of(b"other"),
                model: "fake-build-1".to_owned(),
                noul: 0.99,
                merged: true,
                judged_at_unix: 2,
            },
        );

        let cache = MergeCache::new(Arc::clone(&shared));
        cache.load_aliases();
        assert_eq!(
            cache.resolve(&hi),
            (lo, Some("backup done files ok".to_owned()))
        );
        assert_eq!(cache.resolve(&y), (y, None));
        // And the rejection is still cached: a `no` is an answer too.
        assert!(cache.lookup(&x, &y).is_some());
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Even a corrupt table halts: the hop cap is a backstop, so a cycle the
    /// judge could never write still resolves instead of spinning.
    #[test]
    fn even_a_hand_built_cycle_halts_at_the_cap() {
        let dir =
            std::env::temp_dir().join(format!("oarfish-alias-cycle-test-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let shared = shared(&dir);
        let cache = MergeCache::new(Arc::clone(&shared));
        let a = TemplateId::of("event a here");
        let b = TemplateId::of("event b here");
        {
            let mut map = shared.aliases.write().expect("lock");
            map.insert(
                a,
                AliasTarget {
                    canonical: b,
                    template: "b".to_owned(),
                },
            );
            map.insert(
                b,
                AliasTarget {
                    canonical: a,
                    template: "a".to_owned(),
                },
            );
        }
        let (resolved, _) = cache.resolve(&a);
        assert!(resolved == a || resolved == b, "halts, got {resolved}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    proptest::proptest! {
        /// Aliases terminate: every alias points strictly downward, so over
        /// generated decision sets every resolution ends at the least id of
        /// its chain — a fixpoint with no outgoing edge — and never cycles.
        #[test]
        fn resolution_always_halts_and_never_cycles(
            edges in proptest::collection::vec((proptest::num::u64::ANY, proptest::num::u64::ANY), 0..50),
        ) {
            use std::collections::HashSet;
            let dir = std::env::temp_dir().join(format!(
                "oarfish-alias-prop-{}-{}",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .expect("clock")
                    .as_nanos()
            ));
            let _ = std::fs::create_dir_all(&dir);
            let shared = shared(&dir);
            let cache = MergeCache::new(Arc::clone(&shared));
            let id_of = |n: u64| {
                let mut bytes = [0u8; 32];
                bytes[..8].copy_from_slice(&n.to_le_bytes());
                TemplateId::from_bytes(bytes)
            };
            let mut mentioned = HashSet::new();
            {
                let mut map = shared.aliases.write().expect("lock");
                for (x, y) in &edges {
                    if x == y {
                        continue;
                    }
                    // Order by `TemplateId`, not by integer: the alias
                    // discipline points downward in id order, and LE-encoded
                    // integers do not sort the way the ids do.
                    let (id_x, id_y) = (id_of(*x), id_of(*y));
                    if id_x == id_y {
                        continue;
                    }
                    let (lo, hi) = if id_x < id_y { (id_x, id_y) } else { (id_y, id_x) };
                    map.insert(
                        hi,
                        AliasTarget {
                            canonical: lo,
                            template: format!("{lo}"),
                        },
                    );
                    mentioned.insert(lo);
                    mentioned.insert(hi);
                }
            }
            for start in mentioned {
                let (resolved, _) = cache.resolve(&start);
                // Terminates at or below the start: every hop descends.
                proptest::prop_assert!(resolved <= start, "rose from {start} to {resolved}");
                // At a fixpoint: nothing further to resolve.
                let (again, _) = cache.resolve(&resolved);
                proptest::prop_assert_eq!(again, resolved, "not a fixpoint");
            }
            let _ = std::fs::remove_dir_all(&dir);
        }
    }
}
