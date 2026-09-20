//! The decision layer: the static question set, sliding windows, and the
//! alarm state machine.
//!
//! Three rates run through here, and keeping them separate is the whole
//! design:
//!
//! - **Every line** - alias resolution, verdict lookup, window aggregation
//!   and the alarm state machine. Pure Rust, no network.
//! - **Per new template** - the static verdict, judged once under
//!   [`static_questions`] and cached against `(template, questions, model)`.
//! - **Per close pair** - the merge review, judged once under
//!   [`merge_questions`] and cached against the canonical pair. The
//!   pipeline refers pairs; the engine resolves through the alias table
//!   before it touches windows, so two halves of a split event feed one
//!   window and one alarm.
//! - **Per burst, flagged templates only** - the contextual check
//!   ([`context`]): the raise waits for the answer, confidence routes the
//!   lane, and failure raises at `Dashboard` rather than swallowing.
//!
//! One [`Engine`] task owns the windows, the state machine and the
//! [`DelayQueue`](tokio_util::time::DelayQueue). The M3 pipeline sends it
//! [`EngineInput`](oarfish_core::EngineInput) over a channel; it publishes
//! [`AlarmChange`] on a `broadcast` channel that SSE handlers subscribe to.
//! The verdict lookup belongs here, not in the pipeline: it is synchronous
//! and cheap, and gating is a decision-layer concern.
//!
//! Depends on `oarfish-core` for the domain types, on `oarfish-jev` for the
//! question shapes the three sets are written in, and on `oarfish-store` for
//! the verdict and merge caches and the persisted open alarms. Holds no
//! policy about which questions to ask beyond owning the question sets
//! themselves.

#![forbid(unsafe_code)]

mod config;
mod context;
mod gate;
mod machine;
mod questions;
mod windows;

pub use config::{
    DEFAULT_BROADCAST_CAPACITY, DEFAULT_CHECK_TIMEOUT_SECS, DEFAULT_CONTEXTUAL_THRESHOLD,
    DEFAULT_FLAP_COOLDOWN_SECS, DEFAULT_JUDGED_CAPACITY, DEFAULT_MATTERS_NOW_THRESHOLD,
    DEFAULT_MAX_CONTEXT_ALARMS, DEFAULT_MAX_WINDOWS, DEFAULT_PENDING_CAPACITY,
    DEFAULT_RATE_MULTIPLE, DEFAULT_SILENCE_SECS, DEFAULT_WINDOW_COUNT_THRESHOLD, EngineConfig,
};
pub use context::{
    BurstContext, CORRELATES_WITH_QUESTION, CheckOutcome, CheckResult, ContextAlarm,
    MATTERS_NOW_QUESTION, NONE_OPTION, OpenAlarmView, PendingAnswer, TEMPLATE_EXCERPT_LEN,
    WAKE_SOMEONE_QUESTION, capture_snapshot, context_questions, run_check,
};
pub use gate::{Gate, gate, is_contextual};
pub use machine::{Engine, route};
/// Re-exported for raisers and servers: defined — and `ts-rs` exported —
/// in `oarfish-core`, the only crate that may write into `oarfish.ts`.
pub use oarfish_core::{AlarmChange, Lane};
pub use questions::{
    ACTIONABLE_QUESTION, BLAST_RADIUS_QUESTION, CONTEXTUAL_QUESTION, PAGE_DELAY_QUESTION,
    RUNBOOK_QUESTION, SEVERITY_QUESTION, merge_questions, static_questions,
};
pub use windows::WindowStats;
pub(crate) use windows::WindowTable;
