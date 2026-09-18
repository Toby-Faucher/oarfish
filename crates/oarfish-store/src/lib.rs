//! Durable state on an embedded `fjall` LSM store.
//!
//! Holds the three caches the cost model depends on: verdicts keyed by
//! `TemplateId`, merge decisions keyed by template pair, and the decision
//! records that let any alarm be replayed long after it fired.
//!
//! Also holds open alarms and local corrections (the "not an alarm" feedback).

#![forbid(unsafe_code)]
