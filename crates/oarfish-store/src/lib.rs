//! Durable state on an embedded `fjall` LSM store.
//!
//! Holds the caches the cost model depends on: verdicts keyed by
//! [`TemplateId`] — more precisely by template, question-set hash and
//! resolved model — merge decisions keyed by template pair (M5), and the
//! decision records that let any alarm be replayed long after it fired.
//!
//! Also holds open alarms and local corrections (the "not an alarm"
//! feedback).
//!
//! [`Verdicts`] is the M4 surface: the moka cache, the fjall keyspaces and,
//! through the background judge task, the Jev client, exposing the
//! cache-aside flow that keeps every network call off the every-line path.
//! Values are `postcard`, not JSON — this fronts a hot read path and JSON in
//! an LSM store is waste — with `moka` in front so verdict lookups do not
//! touch disk per line.

#![forbid(unsafe_code)]

mod alarms;
mod keys;
mod record;
mod verdicts;

pub use alarms::{ALARMS_KEYSPACE, alarm_key, decode_alarm, encode_alarm};
pub use keys::{
    QUESTIONS_HASH_LEN, TEMPLATE_ID_LEN, parse_verdict_key, verdict_key, verdict_questions_prefix,
    verdict_template_prefix,
};
pub use record::{DecisionRecord, record_key};
pub use verdicts::{
    DEFAULT_CACHE_CAPACITY, DEFAULT_JUDGE_CONCURRENCY, DEFAULT_QUEUE_CAPACITY, Error,
    RECORDS_BY_TEMPLATE_KEYSPACE, RECORDS_KEYSPACE, VERDICTS_KEYSPACE, Verdicts, VerdictsConfig,
};
