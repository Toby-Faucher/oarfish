//! The decision layer: sliding windows, the two Jev passes, and the alarm
//! state machine.
//!
//! Three rates run through here, and keeping them separate is the whole
//! design:
//!
//! - **Every line** - window aggregation and the alarm state machine. Pure
//!   Rust, no network.
//! - **Per new template** - the static verdict and merge review. Cached
//!   against `(template, questions, model)` — never re-asked for the same
//!   triple.
//! - **Per burst, flagged templates only** - the contextual check.
//!
//! Confidence picks the lane, and the threshold scales with the stakes: waking
//! someone needs more certainty than drawing a card on a dashboard.

#![forbid(unsafe_code)]
