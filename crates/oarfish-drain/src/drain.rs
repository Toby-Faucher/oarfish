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
    /// Pairs scoring below this are not referred. Must sit below the table's
    /// similarity: the band is `[floor, similarity)`.
    pub floor: f64,
}

/// One referred pair: a live cluster structurally close to the queried one,
/// in the referral band and across token counts on purpose.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Candidate {
    /// The candidate cluster's internal sequence. Never persisted.
    pub seq: u64,
    /// The candidate cluster's current template id.
    pub template_id: TemplateId,
    /// Jaccard over token multisets, in `[floor, similarity)`.
    pub score: f64,
}

/// Why a clusterer could not be built. Validation runs at construction, not
/// per line: the every-line path never fails.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum DrainError {
    #[error("depth must be at least 3 (a count level and a token level), got {0}")]
    DepthBelowMinimum(usize),
}

/// The clusterer. Single-threaded by construction: `train` takes `&mut self`.
///
/// The table is bounded: past `max_clusters` entries the least-recently-used
/// cluster is forgotten (a clock-based LRU — a tick per train, stamps ordered
/// in a `BTreeMap`, so touch and eviction are O(log n)). An evicted id stays
/// in tree nodes as a stale entry and is filtered on read; seqs are never
/// reused, so a stale entry can never resurrect. `max_clusters == 0` means
/// unbounded, mirroring Loki.
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
}

impl Drain {
    /// Build a clusterer. The only failure is a depth that cannot hold a tree.
    pub fn new(config: Config) -> Result<Self, DrainError> {
        if config.depth < 3 {
            return Err(DrainError::DepthBelowMinimum(config.depth));
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
                    },
                );
                self.stamps.insert(tick, seq);
                tree::insert(&mut self.root, seq, &tokens, &self.config);
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
            let oldest = self.stamps.pop_first().expect("stamps track the table");
            if let Some(entry) = self.table.remove(&oldest.1) {
                let len = entry.cluster.tokens.len();
                if let Some(bucket) = self.counts.get_mut(&len) {
                    if let Some(pos) = bucket.iter().position(|seq| *seq == oldest.1) {
                        bucket.swap_remove(pos);
                    }
                    if bucket.is_empty() {
                        self.counts.remove(&len);
                    }
                }
            }
        }
    }

    /// Best candidate at or above threshold among the leaf's live clusters.
    /// Compares the stored token vectors in place: no re-parsing.
    fn search(&self, tokens: &[&str]) -> Option<u64> {
        let leaf = tree::search(&self.root, tokens, &self.config)?;
        if tokens.len() < 2 {
            return leaf
                .cluster_ids
                .iter()
                .find(|id| self.table.contains_key(id))
                .copied();
        }
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
    /// scored by Jaccard over token multisets. Only the referral band is
    /// returned: at or above [`Config::similarity`] the two lines would
    /// already be one cluster, so such a pair cannot exist as two clusters,
    /// and below the floor the pair is different events rather than close
    /// ones. Results arrive highest score first.
    pub fn neighbours(&self, seq: u64, query: &NeighbourQuery) -> Vec<Candidate> {
        let entry = match self.table.get(&seq) {
            Some(entry) => entry,
            None => return Vec::new(),
        };
        let len = entry.cluster.tokens.len();
        let floor = query.floor;
        if floor >= self.config.similarity {
            return Vec::new();
        }
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
                if score >= floor && score < self.config.similarity {
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

    /// The ceiling is Drain's own threshold: at or above it the pair cannot
    /// exist as two clusters at equal counts — and across counts it is still
    /// not referred. Near-identical lines sharing every token but one extra
    /// on a long line score past the ceiling and stay out.
    #[test]
    fn no_pair_scoring_at_or_above_threshold_is_ever_referred() {
        let mut drain = drain();
        // Ten tokens plus one optional eleventh: 10/11 ≈ 0.909, past the
        // ceiling — two clusters Drain never compares, and no referral.
        let base = drain.train("t1 t2 t3 t4 t5 t6 t7 t8 t9 t10");
        let extended = drain.train("t1 t2 t3 t4 t5 t6 t7 t8 t9 t10 extra");
        assert_ne!(base.seq, extended.seq);
        assert!(neighbours_of(&drain, base.seq).is_empty());
        assert!(neighbours_of(&drain, extended.seq).is_empty());

        // And below the floor: different events stay unreferred.
        let mut other = Drain::new(Config::default()).expect("default config is valid");
        let a = other.train("alpha bravo charlie delta echo foxtrot");
        let b = other.train("zulu yankee xray whiskey victor tango");
        assert_ne!(a.seq, b.seq);
        assert!(neighbours_of(&other, a.seq).is_empty());
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
}
