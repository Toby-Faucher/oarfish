//! A port of the Drain log-clustering algorithm: a fixed-depth parse tree
//! that assigns each masked line a stable `TemplateId`.
//!
//! Ported from Grafana Loki's `pkg/pattern/drain` rather than from drain3,
//! because Loki's version ships the operational pieces we need - notably a
//! limiter that bounds cluster-table growth on hostile input.
//!
//! Runs at a deliberately conservative similarity threshold (0.90): it should
//! over-split rather than merge "succeeded" into "failed". Over-splits are
//! repaired by the merge review in `oarfish-engine`.

#![forbid(unsafe_code)]
