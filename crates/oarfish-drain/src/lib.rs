//! A port of the Drain log-clustering algorithm: a fixed-depth parse tree
//! that assigns each masked line a stable `TemplateId`.
//!
//! ```
//! let mut drain = oarfish_drain::Drain::new(oarfish_drain::Config::default())?;
//! let assignment = drain.train("sshd<VAR:PID>: Failed password for root from <VAR:IP4>");
//! # Ok::<(), oarfish_drain::DrainError>(())
//! ```
//!
//! Ported from Grafana Loki's `pkg/pattern/drain` (MIT) rather than from
//! drain3, because Loki's version ships the operational piece that matters: a
//! bounded cluster table (LRU past `max_clusters`), so hostile input cannot
//! grow memory without limit. Not ported: Loki's punctuation tokenizer and its
//! per-format state (masking already separated variables from constants),
//! numeric pre-parameterization (kept, it would shred `<VAR:IP4>`), and
//! minimum-length guards (every line clusters).
//!
//! Runs at a deliberately conservative similarity threshold (0.90): it should
//! over-split rather than merge "succeeded" into "failed". Over-splits are
//! repaired by the merge review in `oarfish-engine`. Single-token differences
//! in lines of ten or more tokens still merge — that is what 0.90 means — and
//! the reviewed instances live next to the comparison harness.
//!
//! [`Drain::neighbours`] is an oarfish addition outside the port: a read-only
//! token-count index and Jaccard query that refers structurally close pairs
//! for merge review. It touches no clustering decision, so the snapshot
//! equivalence against drain3 is unaffected.
//!
//! In: masked text as `&str` (never raw lines, never `Event`). Out: an
//! `Assignment` carrying the internal sequence, the `TemplateId` of the
//! cluster's template, and the cluster size. Depends on `oarfish-core` for
//! `TemplateId`, on `serde`/`thiserror` for persistence and errors, and on
//! nothing else in the workspace.

#![forbid(unsafe_code)]

mod config;
mod drain;
mod tree;

pub use config::Config;
pub use drain::{Assignment, Candidate, Cluster, Drain, DrainError, NeighbourQuery};
