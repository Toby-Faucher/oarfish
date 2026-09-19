//! The decision layer: the static question set, sliding windows, and the
//! alarm state machine.
//!
//! Three rates run through here, and keeping them separate is the whole
//! design:
//!
//! - **Every line** - verdict lookup, window aggregation and the alarm state
//!   machine. Pure Rust, no network.
//! - **Per new template** - the static verdict, judged once under
//!   [`static_questions`] and cached against `(template, questions, model)`.
//! - **Per burst, flagged templates only** - the contextual check (M5.5).
//!
//! One [`Engine`] task owns the windows, the state machine and the
//! [`DelayQueue`](tokio_util::time::DelayQueue). The M3 pipeline sends it
//! [`EngineInput`](oarfish_core::EngineInput) over a channel; it publishes
//! [`AlarmChange`] on a `broadcast` channel that SSE handlers subscribe to.
//! The verdict lookup belongs here, not in the pipeline: it is synchronous
//! and cheap, and gating is a decision-layer concern.
//!
//! Depends on `oarfish-core` for the domain types, on `oarfish-jev` for the
//! question shape the static set is written in, and on `oarfish-store` for
//! the verdict cache and the persisted open alarms. Holds no policy about
//! which questions to ask beyond owning the static set itself.

#![forbid(unsafe_code)]

mod config;
mod machine;
mod questions;
mod windows;

pub use config::{
    DEFAULT_BROADCAST_CAPACITY, DEFAULT_FLAP_COOLDOWN_SECS, DEFAULT_MAX_WINDOWS,
    DEFAULT_RATE_MULTIPLE, DEFAULT_SILENCE_SECS, DEFAULT_WINDOW_COUNT_THRESHOLD, EngineConfig,
};
pub use machine::{Engine, Lane, Snapshot, route};
/// Re-exported for publishers and servers: defined — and `ts-rs` exported —
/// in `oarfish-core`, the only crate that may write into `oarfish.ts`.
pub use oarfish_core::AlarmChange;
pub use questions::static_questions;
pub(crate) use windows::WindowTable;
