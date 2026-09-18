//! Assigning masked lines to clusters: the every-line path.
//!
//! The tree mechanics live in `tree`; this file owns the table, the template
//! text, and the ids. Ported from `Drain::train` in Loki's
//! `pkg/pattern/drain/drain.go`, minus tokenization (whitespace: masking did
//! the separation), numeric pre-parameterization (kept, it would shred
//! `<VAR:IP4>`), and minimum-length guards (every line clusters).

use std::collections::HashMap;

use oarfish_core::TemplateId;
use serde::{Deserialize, Serialize};

use crate::{Config, tree};

/// One cluster: the template text lines generalize into, and how many lines
/// have joined it. `template` holds tokens joined with single spaces.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Cluster {
    pub seq: u64,
    pub template: String,
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

/// Split a stored template back into tokens. The empty template holds zero
/// tokens, not one empty string: `"".split(' ')` would say otherwise.
fn split_template(template: &str) -> Vec<String> {
    if template.is_empty() {
        Vec::new()
    } else {
        template.split(' ').map(str::to_owned).collect()
    }
}

/// The clusterer. Single-threaded by construction: `train` takes `&mut self`.
// TODO(Task 2): bound the table with LRU eviction. Until then this grows
// without limit; do not mistake the interim state for the design.
pub struct Drain {
    config: Config,
    root: tree::Node,
    table: HashMap<u64, Cluster>,
    next_seq: u64,
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
            next_seq: 1,
        })
    }

    /// Assign one masked line to a cluster, creating or generalizing as
    /// needed. Total: every input, including `""`, gets an assignment.
    /// Pure apart from the table write; no network, never.
    pub fn train(&mut self, masked: &str) -> Assignment {
        let tokens: Vec<String> = masked.split_whitespace().map(str::to_owned).collect();

        let seq = match self.search(&tokens) {
            Some(seq) => {
                let cluster = self.table.get_mut(&seq).expect("search yields live ids");
                let mut template_tokens = split_template(&cluster.template);
                tree::generalize(&mut template_tokens, &tokens, &self.config.param);
                cluster.template = template_tokens.join(" ");
                cluster.size += 1;
                seq
            }
            None => {
                let seq = self.next_seq;
                self.next_seq += 1;
                let template = tokens.join(" ");
                self.table.insert(
                    seq,
                    Cluster {
                        seq,
                        template,
                        size: 1,
                    },
                );
                tree::insert(&mut self.root, seq, &tokens, &self.config);
                seq
            }
        };

        let cluster = self.table.get(&seq).expect("just assigned");
        Assignment {
            seq,
            template: TemplateId::of(&cluster.template),
            size: cluster.size,
        }
    }

    /// Best candidate at or above threshold among the leaf's live clusters.
    fn search(&self, tokens: &[String]) -> Option<u64> {
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
                Some(cluster) => cluster,
                None => continue, // evicted since insertion; filtered here
            };
            let cluster_tokens = split_template(&cluster.template);
            if cluster_tokens.len() != tokens.len() {
                continue;
            }
            let (sim, params) = tree::similarity(&cluster_tokens, tokens, &self.config.param);
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
        self.table.get(&seq)
    }

    /// Every live cluster, in sequence order. What M4 enumerates to persist
    /// verdicts against, and what the snapshot harness renders.
    pub fn clusters(&self) -> impl Iterator<Item = &Cluster> {
        let mut clusters: Vec<&Cluster> = self.table.values().collect();
        clusters.sort_by_key(|c| c.seq);
        clusters.into_iter()
    }
}
