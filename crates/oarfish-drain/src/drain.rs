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
            self.table.remove(&oldest.1);
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

#[cfg(test)]
mod tests {
    use crate::{Config, Drain};

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
}
