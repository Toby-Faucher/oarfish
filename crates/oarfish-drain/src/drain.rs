//! Assigning masked lines to clusters: the every-line path.
//!
//! The tree mechanics live in `tree`; this file owns the table, the template
//! text, and the ids. Ported from `Drain::train` in Loki's
//! `pkg/pattern/drain/drain.go`, minus tokenization (whitespace: masking did
//! the separation), numeric pre-parameterization (kept, it would shred
//! `<VAR:IP4>`), and minimum-length guards (every line clusters).

use std::collections::{BTreeMap, HashMap};

use oarfish_core::TemplateId;
use serde::{Deserialize, Serialize};

use crate::{Config, tree};

/// One cluster: the template text lines generalize into, and how many lines
/// have joined it. `template` holds tokens joined with single spaces;
/// `tokens` holds the same positions split, so the every-line path compares
/// in place instead of re-parsing `template` per candidate. `template_id` is
/// the hash of `template`, cached here because the steady state re-matches
/// unchanged templates. All three are updated together, exactly when
/// `generalize` reports a change.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Cluster {
    pub seq: u64,
    pub template: String,
    pub tokens: Vec<String>,
    pub template_id: TemplateId,
    pub size: u64,
}

/// What `train` hands back: all `Copy`, so the every-line path never borrows
/// the table it just wrote.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Assignment {
    /// Internal sequence. Never persisted, never leaves the crate's docs as
    /// stable: a restart renumbers everything.
    pub seq: u64,
    /// The stable identity: `TemplateId::of` over the cluster's template.
    pub template: TemplateId,
    /// Lines assigned so far, including this one.
    pub size: u64,
}

/// How [`Drain::neighbours`] is asked: the referral floor for this call.
/// Defaults to the table's [`Config::referral_floor`]; tests pin values
/// directly rather than rebuilding tables under edited configs.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct NeighbourQuery {
    /// Pairs scoring below this are not referred. There is no ceiling: see
    /// [`Drain::neighbours`].
    pub floor: f64,
}

/// One referred pair: a live cluster structurally close to the queried one,
/// at or above the referral floor and across token counts on purpose.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Candidate {
    /// The candidate cluster's internal sequence. Never persisted.
    pub seq: u64,
    /// The candidate cluster's current template id.
    pub template_id: TemplateId,
    /// Jaccard over token multisets, in `[floor, 1]`.
    pub score: f64,
}

/// Why a clusterer could not be built. Validation runs at construction, not
/// per line: the every-line path never fails.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum DrainError {
    #[error("depth must be at least 3 (a count level and a token level), got {0}")]
    DepthBelowMinimum(usize),
    #[error("max_children must be at least 1, room for the parameter node")]
    MaxChildrenZero,
}

/// The clusterer. Single-threaded by construction: `train` takes `&mut self`.
///
/// The table is bounded: past `max_clusters` entries the least-recently-used
/// cluster is forgotten (a clock-based LRU — a tick per train, stamps ordered
/// in a `BTreeMap`, so touch and eviction are O(log n)). Eviction takes the id
/// out of its tree leaf too and prunes nodes it leaves empty, so the tree holds
/// exactly the live clusters and is bounded with the table. `max_clusters == 0`
/// means unbounded, mirroring Loki.
///
/// Each leaf is bounded too, by `max_leaf_clusters`: a new cluster that
/// would overfill its leaf evicts that leaf's least recently used cluster.
/// Search scores every cluster in the line's leaf, so this is what bounds one
/// line's work (`leaf_cost_bounded` in `lean/OarfishDrain/Theorems.lean`).
pub struct Drain {
    config: Config,
    root: tree::Node,
    table: HashMap<u64, Entry>,
    stamps: BTreeMap<u64, u64>,
    tick: u64,
    next_seq: u64,
    /// Token-count index for [`Drain::neighbours`]: token count → live seqs.
    /// Generalization never changes a cluster's token count, so entries move
    /// only on insert and evict. An oarfish addition; the drain3 port never
    /// had it.
    counts: BTreeMap<usize, Vec<u64>>,
}

struct Entry {
    cluster: Cluster,
    tick: u64,
    /// The edge keys from the root to the leaf holding this id. Recorded at
    /// insert because generalization changes `cluster.tokens`, and with it
    /// the path those tokens would take today.
    path: Vec<String>,
}

impl Drain {
    /// Build a clusterer. The failures are shapes that cannot hold a tree: a
    /// depth under 3, or no room for even the parameter node.
    pub fn new(config: Config) -> Result<Self, DrainError> {
        if config.depth < 3 {
            return Err(DrainError::DepthBelowMinimum(config.depth));
        }
        if config.max_children == 0 {
            return Err(DrainError::MaxChildrenZero);
        }
        Ok(Self {
            config,
            root: tree::Node::new(),
            table: HashMap::new(),
            stamps: BTreeMap::new(),
            tick: 0,
            next_seq: 1,
            counts: BTreeMap::new(),
        })
    }

    /// Assign one masked line to a cluster, creating or generalizing as
    /// needed. Total: every input, including `""`, gets an assignment.
    /// Pure apart from the table write; no network, never.
    pub fn train(&mut self, masked: &str) -> Assignment {
        let max_tokens = self.config.max_tokens;
        // Borrowed views into `masked`: the every-line path allocates one
        // `Vec` here and no per-token `String`. Owned copies are made only
        // when a new cluster is created.
        let tokens: Vec<&str> = masked.split_whitespace().take(max_tokens).collect();

        self.tick = self.tick.wrapping_add(1);
        let tick = self.tick;
        let seq = match self.search(&tokens) {
            Some(seq) => {
                let old_tick = {
                    let entry = self.table.get_mut(&seq).expect("search yields live ids");
                    if tree::generalize(&mut entry.cluster.tokens, &tokens, &self.config.param) {
                        entry.cluster.template = entry.cluster.tokens.join(" ");
                        entry.cluster.template_id = TemplateId::of(&entry.cluster.template);
                    }
                    entry.cluster.size += 1;
                    entry.tick
                };
                self.touch(seq, old_tick, tick);
                let entry = self.table.get_mut(&seq).expect("just touched");
                entry.tick = tick;
                seq
            }
            None => {
                let seq = self.next_seq;
                self.next_seq += 1;
                let template = tokens.join(" ");
                let owned: Vec<String> = tokens.iter().map(|s| (*s).to_owned()).collect();
                let template_id = TemplateId::of(&template);
                self.counts.entry(owned.len()).or_default().push(seq);
                self.table.insert(
                    seq,
                    Entry {
                        cluster: Cluster {
                            seq,
                            template,
                            tokens: owned,
                            template_id,
                            size: 1,
                        },
                        tick,
                        path: Vec::new(),
                    },
                );
                self.stamps.insert(tick, seq);
                let path = tree::insert(&mut self.root, seq, &tokens, &self.config);
                self.evict_from_leaf(&path, seq);
                self.table.get_mut(&seq).expect("just inserted").path = path;
                self.evict();
                seq
            }
        };

        let cluster = &self.table.get(&seq).expect("just assigned").cluster;
        Assignment {
            seq,
            template: cluster.template_id,
            size: cluster.size,
        }
    }

    /// Train on one masked line and hand back the live cluster with the
    /// assignment. The sequence just trained is live by construction: a join
    /// yields a live id by construction, and a new insert is the newest
    /// entry, so eviction cannot have taken it in the same call. Owning that
    /// invariant here keeps every caller from restating — and re-proving — it
    /// beside its own `expect`.
    pub fn train_get(&mut self, masked: &str) -> (Assignment, &Cluster) {
        let assignment = self.train(masked);
        let cluster = &self
            .table
            .get(&assignment.seq)
            .expect("the sequence just trained is live")
            .cluster;
        (assignment, cluster)
    }

    /// Move `seq` to the current tick.
    fn touch(&mut self, seq: u64, old_tick: u64, new_tick: u64) {
        self.stamps.remove(&old_tick);
        self.stamps.insert(new_tick, seq);
    }

    /// Forget least-recently-used clusters while over the cap.
    fn evict(&mut self) {
        let cap = if self.config.max_clusters == 0 {
            usize::MAX
        } else {
            self.config.max_clusters
        };
        while self.table.len() > cap {
            let (_, oldest) = self
                .stamps
                .first_key_value()
                .expect("stamps track the table");
            self.forget(*oldest);
        }
    }

    /// Forget the leaf's least-recently-used clusters while it is over
    /// `max_leaf_clusters`. `newest` just went in and is never the victim:
    /// its tick is the latest anyway, and a train always returns a live id.
    fn evict_from_leaf(&mut self, path: &[String], newest: u64) {
        let cap = self.config.max_leaf_clusters;
        if cap == 0 {
            return;
        }
        loop {
            let ids = tree::leaf_ids(&self.root, path);
            if ids.len() <= cap {
                return;
            }
            let oldest = ids
                .iter()
                .filter(|id| **id != newest)
                .filter_map(|id| self.table.get(id).map(|entry| (entry.tick, *id)))
                .min();
            match oldest {
                Some((_, seq)) => self.forget(seq),
                None => return,
            }
        }
    }

    /// Drop one cluster from everything that tracks it: the table, the
    /// stamps, the count index and its tree leaf.
    fn forget(&mut self, seq: u64) {
        let Some(entry) = self.table.remove(&seq) else {
            return;
        };
        self.stamps.remove(&entry.tick);
        tree::remove(&mut self.root, &entry.path, seq);
        let len = entry.cluster.tokens.len();
        if let Some(bucket) = self.counts.get_mut(&len) {
            if let Some(pos) = bucket.iter().position(|s| *s == seq) {
                bucket.swap_remove(pos);
            }
            if bucket.is_empty() {
                self.counts.remove(&len);
            }
        }
    }

    /// Best candidate at or above threshold among the leaf's live clusters.
    /// Compares the stored token vectors in place: no re-parsing.
    ///
    /// Every length faces the threshold, one-token lines included. Loki
    /// returns a short line's first cluster unchecked, which is safe there
    /// only because Loki drops lines under four tokens; oarfish clusters
    /// every line, so the shortcut merged `succeeded` into `failed`.
    fn search(&self, tokens: &[&str]) -> Option<u64> {
        let leaf = tree::search(&self.root, tokens, &self.config)?;
        let (mut best_seq, mut best_sim, mut best_params) = (0, -1.0, 0);
        let mut found = false;
        for id in &leaf.cluster_ids {
            let cluster = match self.table.get(id) {
                Some(entry) => &entry.cluster,
                None => continue, // evicted since insertion; filtered here
            };
            if cluster.tokens.len() != tokens.len() {
                continue;
            }
            let (sim, params) = tree::similarity(&cluster.tokens, tokens, &self.config.param);
            if sim > best_sim || (sim == best_sim && params > best_params) {
                best_sim = sim;
                best_params = params;
                best_seq = *id;
                found = true;
            }
        }
        (found && best_sim >= self.config.similarity).then_some(best_seq)
    }

    /// Structurally close clusters to `seq`: the merge-review referral.
    ///
    /// **An oarfish addition, not part of the drain3 port.** Read-only: it
    /// touches no clustering decision, so the snapshot equivalence against
    /// drain3 is unaffected. It deliberately crosses token counts — the
    /// motivating case is one real event split in two because an optional
    /// field changed the token count, and Drain's first tree level *is* token
    /// count, so any search built on the tree would systematically miss the
    /// exact case this exists to repair.
    ///
    /// Buckets within ±2 tokens of the cluster are read through the
    /// token-count index (a handful of bucket reads, not a table walk) and
    /// scored by Jaccard over token multisets. Everything at or above the
    /// floor is returned; below it the pair is different events rather than
    /// close ones. There is no ceiling. A high Jaccard does not mean Drain
    /// already merged the pair: Jaccard ignores position, and Drain never
    /// compares across token counts or across tree paths (a word varying at
    /// an early position splits at the tree). Those pairs score highest and
    /// are the over-splits most worth repairing. Results arrive highest
    /// score first.
    pub fn neighbours(&self, seq: u64, query: &NeighbourQuery) -> Vec<Candidate> {
        let entry = match self.table.get(&seq) {
            Some(entry) => entry,
            None => return Vec::new(),
        };
        let len = entry.cluster.tokens.len();
        let floor = query.floor;
        let mut out = Vec::new();
        let lo = len.saturating_sub(2);
        let hi = len.saturating_add(2);
        for count in lo..=hi {
            let bucket = match self.counts.get(&count) {
                Some(bucket) => bucket,
                None => continue,
            };
            for candidate_seq in bucket {
                if *candidate_seq == seq {
                    continue;
                }
                let candidate = match self.table.get(candidate_seq) {
                    Some(entry) => &entry.cluster,
                    None => continue,
                };
                let score = jaccard(&entry.cluster.tokens, &candidate.tokens);
                if score >= floor {
                    out.push(Candidate {
                        seq: *candidate_seq,
                        template_id: candidate.template_id,
                        score,
                    });
                }
            }
        }
        out.sort_by(|a, b| {
            b.score
                .partial_cmp(&a.score)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then_with(|| a.seq.cmp(&b.seq))
        });
        out
    }

    /// The merge-referral floor this table refers under: [`Config::referral_floor`].
    pub fn referral_floor(&self) -> f64 {
        self.config.referral_floor
    }

    /// Look up a cluster by sequence number. `None` means never created — or
    /// evicted, once Task 2 bounds the table.
    pub fn get(&self, seq: u64) -> Option<&Cluster> {
        self.table.get(&seq).map(|entry| &entry.cluster)
    }

    /// Every live cluster, in sequence order. What M4 enumerates to persist
    /// verdicts against, and what the snapshot harness renders.
    pub fn clusters(&self) -> impl Iterator<Item = &Cluster> {
        let mut clusters: Vec<&Cluster> = self.table.values().map(|entry| &entry.cluster).collect();
        clusters.sort_by_key(|c| c.seq);
        clusters.into_iter()
    }
}

/// Jaccard over token multisets: intersection (per-token minima) over union
/// (per-token maxima). Order-insensitive on purpose — the referral crosses
/// token counts, so positions do not align — and bounded in `[0, 1]`. Two
/// empty clusters score 0, not 1: nothing about an empty line is close to
/// anything.
fn jaccard(a: &[String], b: &[String]) -> f64 {
    if a.is_empty() && b.is_empty() {
        return 0.0;
    }
    let mut counts: HashMap<&str, (usize, usize)> = HashMap::new();
    for token in a {
        counts.entry(token.as_str()).or_default().0 += 1;
    }
    for token in b {
        counts.entry(token.as_str()).or_default().1 += 1;
    }
    let (mut inter, mut union) = (0usize, 0usize);
    for (_, (ca, cb)) in counts {
        inter += ca.min(cb);
        union += ca.max(cb);
    }
    if union == 0 {
        0.0
    } else {
        inter as f64 / union as f64
    }
}

#[cfg(test)]
mod tests {
    use crate::{Candidate, Config, Drain, NeighbourQuery};

    fn drain() -> Drain {
        Drain::new(Config::default()).expect("default config is valid")
    }

    #[test]
    fn template_ids_derive_from_the_cluster_template() {
        use oarfish_core::TemplateId;
        let mut drain = drain();
        // Ten tokens differing only at the last: same tree path, 9/10, so
        // the pair generalizes its final position.
        let assignment = drain.train("user alice logged in from here at dawn today ok");
        assert_eq!(
            assignment.template,
            TemplateId::of("user alice logged in from here at dawn today ok")
        );
        // After generalization the id follows the template, monotonically.
        drain.train("user alice logged in from here at dawn today fine");
        let template = drain.get(assignment.seq).expect("cluster").template.clone();
        assert_eq!(template, "user alice logged in from here at dawn today <*>");
        assert_eq!(
            drain
                .train("user alice logged in from here at dawn today ok")
                .template,
            TemplateId::of(&template)
        );
    }

    /// A one-token line faces the threshold like any other. `succeeded` then
    /// `failed` used to share a cluster templated `<*>`: invariant 4's own
    /// example, merged. Found by `plausible` against the Lean model
    /// (`lean/OarfishDrain`), which proves join soundness at every length.
    #[test]
    fn one_token_lines_join_only_when_equal() {
        let mut drain = drain();
        let ok = drain.train("succeeded");
        let failed = drain.train("failed");
        assert_ne!(ok.seq, failed.seq, "succeeded and failed must stay apart");
        assert_eq!(drain.get(ok.seq).expect("live").template, "succeeded");
        assert_eq!(drain.get(failed.seq).expect("live").template, "failed");
        // An equal line still joins.
        assert_eq!(drain.train("succeeded").seq, ok.seq);
    }

    #[test]
    fn the_table_evicts_least_recently_used_past_the_cap() {
        let mut drain = Drain::new(Config {
            max_clusters: 3,
            ..Config::default()
        })
        .expect("valid");
        for word in ["alpha", "bravo", "charlie", "delta"] {
            drain.train(&format!("entirely different line about {word} things here"));
        }
        // Four distinct clusters under a cap of three: the oldest is gone.
        assert_eq!(drain.clusters().count(), 3);
        assert!(drain.get(1).is_none(), "seq 1 should have been evicted");
        // And the table still works: a repeat of a live line joins it.
        let again = drain.train("entirely different line about delta things here");
        assert_eq!(again.seq, 4);
        assert_eq!(again.size, 2);
    }

    #[test]
    fn evicted_ids_never_come_back_as_ghosts() {
        let mut drain = Drain::new(Config {
            max_clusters: 1,
            ..Config::default()
        })
        .expect("valid");
        drain.train("first distinct line with enough tokens here");
        drain.train("second line completely unlike the first one");
        // seq 1 was evicted; a line shaped like it must open a NEW cluster,
        // not resurrect seq 1 through a stale tree entry.
        let third = drain.train("first distinct line with enough tokens here");
        assert_ne!(third.seq, 1);
        assert_eq!(third.size, 1);
    }

    #[test]
    fn overlong_lines_cluster_on_their_prefix() {
        let mut drain = Drain::new(Config {
            max_tokens: 4,
            ..Config::default()
        })
        .expect("valid");
        let assignment = drain.train("a b c d e f g h");
        let cluster = drain.get(assignment.seq).expect("cluster");
        assert_eq!(cluster.template, "a b c d");
    }

    fn neighbours_of(drain: &Drain, seq: u64) -> Vec<Candidate> {
        drain.neighbours(
            seq,
            &NeighbourQuery {
                floor: Config::default().referral_floor,
            },
        )
    }

    /// The §5.6 motivating case: one real event split in two because an
    /// optional field changed the token count. Drain never compares across
    /// counts, so both clusters exist — and the referral crosses counts on
    /// purpose to find them. Six tokens plus an optional seventh: 6/7 ≈
    /// 0.857, inside the band.
    #[test]
    fn an_optional_field_across_token_counts_is_referred() {
        let mut drain = drain();
        let base = drain.train("backup done files ok size mb took secs");
        let extended = drain.train("backup done files ok size mb took secs extra");
        assert_ne!(base.seq, extended.seq);

        let referred = neighbours_of(&drain, base.seq);
        assert_eq!(referred.len(), 1, "got {referred:?}");
        assert_eq!(referred[0].seq, extended.seq);
        assert_eq!(referred[0].template_id, extended.template);
        assert!(
            (0.65..0.90).contains(&referred[0].score),
            "score {} outside the band",
            referred[0].score
        );
        // Symmetric: the extended cluster refers back to the base.
        let back = neighbours_of(&drain, extended.seq);
        assert_eq!(back.len(), 1);
        assert_eq!(back[0].seq, base.seq);
    }

    /// There is no ceiling: a high score does not mean Drain already merged
    /// the pair. Jaccard ignores position and Drain never compares across
    /// token counts or across tree paths, so pairs scoring 0.90 and above
    /// exist as two clusters, and they are the closest over-splits of all.
    #[test]
    fn high_scoring_pairs_that_drain_split_are_referred() {
        // An optional eleventh field: 10/11 ≈ 0.909, two counts, never compared.
        let mut drain = drain();
        let base = drain.train("t1 t2 t3 t4 t5 t6 t7 t8 t9 t10");
        let extended = drain.train("t1 t2 t3 t4 t5 t6 t7 t8 t9 t10 extra");
        assert_ne!(base.seq, extended.seq);
        let referred = neighbours_of(&drain, base.seq);
        assert_eq!(referred.len(), 1, "got {referred:?}");
        assert_eq!(referred[0].seq, extended.seq);
        assert!(referred[0].score >= 0.90, "score {}", referred[0].score);

        // A word varying at token 2 of 20: the tree splits the pair at an
        // early level, so they never meet. 19/21 ≈ 0.905.
        let words = "alpha bravo charlie delta echo foxtrot golf hotel india juliet \
                     kilo lima mike november oscar papa quebec romeo sierra tango";
        let mut drain = self::drain();
        let a = drain.train(words);
        let b = drain.train(&words.replacen("bravo", "zulu", 1));
        assert_ne!(a.seq, b.seq);
        assert_eq!(
            neighbours_of(&drain, a.seq).first().map(|c| c.seq),
            Some(b.seq)
        );

        // Same tokens, different order: Jaccard 1.0, two clusters.
        let mut drain = self::drain();
        let a = drain.train("a b c d");
        let b = drain.train("d c b a");
        assert_ne!(a.seq, b.seq);
        assert_eq!(
            neighbours_of(&drain, a.seq).first().map(|c| c.seq),
            Some(b.seq)
        );
    }

    /// Below the floor, different events stay unreferred.
    #[test]
    fn pairs_below_the_floor_are_not_referred() {
        let mut drain = drain();
        let a = drain.train("alpha bravo charlie delta echo foxtrot");
        let b = drain.train("zulu yankee xray whiskey victor tango");
        assert_ne!(a.seq, b.seq);
        assert!(neighbours_of(&drain, a.seq).is_empty());
    }

    /// Every tree leaf id is a live cluster, each live cluster sits in the
    /// tree exactly once, and no node is left empty. Eviction used to leave
    /// ids behind (spec §3 said "cleaned on insert"; nothing cleaned them),
    /// so leaves grew without bound under exactly the hostile input
    /// `max_clusters` exists to bound.
    fn assert_tree_matches_table(drain: &Drain) {
        /// Collect every leaf id, checking on the way that no node is left
        /// empty and no leaf holds more than `cap` ids (0: uncapped).
        fn walk(node: &crate::tree::Node, ids: &mut Vec<u64>, depth: usize, cap: usize) {
            assert!(
                cap == 0 || node.cluster_ids.len() <= cap,
                "leaf at depth {depth} holds {} ids, over the cap of {cap}",
                node.cluster_ids.len()
            );
            ids.extend(&node.cluster_ids);
            for (key, child) in node.children() {
                assert!(
                    !child.cluster_ids.is_empty() || child.children().next().is_some(),
                    "empty node {key:?} left at depth {depth}"
                );
                walk(child, ids, depth + 1, cap);
            }
        }
        let mut ids = Vec::new();
        walk(&drain.root, &mut ids, 0, drain.config.max_leaf_clusters);
        ids.sort_unstable();
        let mut live: Vec<u64> = drain.table.keys().copied().collect();
        live.sort_unstable();
        assert_eq!(ids, live, "tree ids and live clusters differ");

        let mut stamped: Vec<u64> = drain.stamps.values().copied().collect();
        stamped.sort_unstable();
        assert_eq!(stamped, live, "stamps and live clusters differ");
        let mut counted: Vec<u64> = drain.counts.values().flatten().copied().collect();
        counted.sort_unstable();
        assert_eq!(counted, live, "the count index and live clusters differ");
        if drain.config.max_clusters > 0 {
            assert!(drain.table.len() <= drain.config.max_clusters);
        }
    }

    #[test]
    fn eviction_removes_the_cluster_from_the_tree() {
        let mut drain = Drain::new(Config {
            max_clusters: 4,
            ..Config::default()
        })
        .expect("valid");
        for i in 0..200 {
            let word = ["alpha", "bravo", "charlie", "delta", "echo"][i % 5];
            drain.train(&format!("{word} event number {i} happened here now"));
            assert_tree_matches_table(&drain);
        }
    }

    /// A leaf never holds more than `max_leaf_clusters`, however many lines
    /// share its path. Every line here takes one path (the same six leading
    /// tokens) and none merge, which is how one hostile source makes every
    /// search scan the whole table: `line_cost_bounded` in the Lean model is
    /// `max_clusters × max_tokens` without this cap.
    #[test]
    fn a_full_leaf_evicts_its_own_least_recently_used() {
        let mut drain = Drain::new(Config {
            max_leaf_clusters: 4,
            ..Config::default()
        })
        .expect("valid");
        let line =
            |i: usize| format!("alpha bravo charlie delta echo foxtrot v{i}a v{i}b v{i}c v{i}d");
        for i in 0..50 {
            drain.train(&line(i));
            assert_tree_matches_table(&drain);
        }
        assert_eq!(drain.clusters().count(), 4, "one leaf, capped at four");
        // The survivors are the four most recent.
        let newest = drain.train(&line(49));
        assert_eq!(newest.size, 2, "the newest line is still live");
        assert_eq!(drain.train(&line(0)).size, 1, "the oldest was evicted");
    }

    /// `max_children: 0` has no room for even the parameter node; it used to
    /// panic on the first three-token line.
    #[test]
    fn max_children_zero_is_rejected() {
        assert_eq!(
            Drain::new(Config {
                max_children: 0,
                ..Config::default()
            })
            .err(),
            Some(crate::DrainError::MaxChildrenZero)
        );
        let mut one = Drain::new(Config {
            max_children: 1,
            ..Config::default()
        })
        .expect("one child is room for the parameter node");
        one.train("a b c");
        one.train("x y z");
    }

    /// Referrals arrive highest score first, and a cluster never refers to
    /// itself. An unknown or evicted seq refers nothing.
    #[test]
    fn referrals_arrive_highest_score_first_and_never_self() {
        let mut drain = drain();
        let base = drain.train("a b c d e f g h");
        // 8/9 ≈ 0.889: every base token plus one extra.
        let near = drain.train("a b c d e f g h extra");
        // 7/8 = 0.875: one token swapped for the extra, same count as base
        // but only 7/8 similar, so still its own cluster.
        let far = drain.train("a b c d e f g extra");
        assert_ne!(base.seq, near.seq);
        assert_ne!(base.seq, far.seq);
        assert_ne!(near.seq, far.seq);

        let referred = neighbours_of(&drain, base.seq);
        assert_eq!(referred.len(), 2, "got {referred:?}");
        assert_eq!(referred[0].seq, near.seq);
        assert_eq!(referred[1].seq, far.seq);
        assert!(
            referred[0].score > referred[1].score,
            "not sorted: {referred:?}"
        );
        assert!(
            referred.iter().all(|c| c.seq != base.seq),
            "self-referral: {referred:?}"
        );

        assert!(neighbours_of(&drain, 999_999).is_empty());
    }

    /// Evicted clusters leave the count index with them: a referral never
    /// names a dead seq, while live close pairs still refer.
    #[test]
    fn evicted_clusters_are_never_referred() {
        let mut drain = Drain::new(Config {
            max_clusters: 3,
            ..Config::default()
        })
        .expect("valid");
        let first = drain.train("backup done files ok size mb");
        let second = drain.train("backup done files ok size mb extra");
        // An unrelated third fills the cap; a fourth close to the second
        // evicts the oldest — seq 1 — instead.
        drain.train("completely other words live here now");
        let fourth = drain.train("backup done files ok size mb again");
        assert!(drain.get(first.seq).is_none(), "seq 1 should be evicted");

        // The live close pair still refers: 6 shared of 8 total = 0.75.
        let referred = neighbours_of(&drain, second.seq);
        assert_eq!(referred.len(), 1, "got {referred:?}");
        assert_eq!(referred[0].seq, fourth.seq);

        for cluster in drain.clusters() {
            let referred = neighbours_of(&drain, cluster.seq);
            assert!(
                referred.iter().all(|c| drain.get(c.seq).is_some()),
                "dead seq referred: {referred:?}"
            );
            assert!(
                !referred.iter().any(|c| c.seq == first.seq),
                "evicted seq referred: {referred:?}"
            );
        }
    }

    proptest::proptest! {
        /// The tree, the stamps and the count index all track exactly the
        /// live clusters, and no leaf passes its cap, under any sequence of
        /// lines and any small caps.
        #[test]
        fn the_tree_and_indexes_track_the_live_table(
            lines in proptest::collection::vec("([a-e0-9]{1,3} ){0,5}[a-e0-9]{1,3}", 1..60),
            cap in 1usize..6,
            leaf_cap in 0usize..4,
        ) {
            let mut drain = Drain::new(Config {
                max_clusters: cap,
                max_leaf_clusters: leaf_cap,
                ..Config::default()
            })
            .expect("valid");
            for line in &lines {
                drain.train(line);
                assert_tree_matches_table(&drain);
            }
        }
    }
}
