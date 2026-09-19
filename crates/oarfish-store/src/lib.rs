//! Durable state on an embedded `fjall` LSM store.
//!
//! Holds the caches the cost model depends on: verdicts keyed by
//! [`TemplateId`] — more precisely by template, question-set hash, bundle
//! hash and resolved model — merge decisions keyed by template pair (M5.5),
//! and the decision records that let any alarm be replayed long after it
//! fired.
//!
//! Also holds open alarms and local corrections (the "not an alarm"
//! feedback).
//!
//! [`Verdicts`] is the M4 surface: the composition root over the
//! [`VerdictCache`], the [`AlarmStore`] and the background [`Judge`],
//! exposing the cache-aside flow that keeps every network call off the
//! every-line path. [`Merges`] is its M5.5 sibling over the same database:
//! the [`MergeCache`], the background [`MergeJudge`], and the alias table the
//! engine resolves through before it touches windows. Values are `postcard`,
//! not JSON — this fronts a hot read path and JSON in an LSM store is waste —
//! with `moka` in front so verdict lookups do not touch disk per line.

#![forbid(unsafe_code)]

mod alarm_store;
mod alarms;
mod judge;
mod keys;
mod merge_cache;
mod merge_decision;
mod merge_judge;
mod merge_keys;
mod merges;
mod record;
mod shared;
mod verdict_cache;
mod verdicts;

pub use alarm_store::AlarmStore;

pub use alarms::{ALARMS_KEYSPACE, alarm_key, decode_alarm, encode_alarm};
pub use judge::Decide;
pub use keys::{
    BUNDLE_HASH_LEN, QUESTIONS_HASH_LEN, TEMPLATE_ID_LEN, parse_verdict_key, verdict_key,
    verdict_questions_prefix, verdict_template_prefix,
};
pub use merge_cache::{AliasTarget, MAX_ALIAS_HOPS, MergeCache};
pub use merge_decision::MergeDecision;
pub use merge_judge::MERGE_QUESTION;
pub use merge_keys::{
    ID_HI_LEN, ID_LO_LEN, PAIR_PREFIX_LEN, canonical_pair, merge_key, merge_pair_prefix,
    parse_merge_key,
};
pub use merges::{
    DEFAULT_MERGE_JUDGE_CONCURRENCY, DEFAULT_MERGE_QUEUE_CAPACITY, DEFAULT_MERGE_THRESHOLD,
    MERGES_KEYSPACE, Merges, MergesConfig,
};
pub use record::{DecisionRecord, record_key};
pub use verdict_cache::VerdictCache;
pub use verdicts::{
    DEFAULT_CACHE_CAPACITY, DEFAULT_JUDGE_CONCURRENCY, DEFAULT_QUEUE_CAPACITY, Error,
    RECORDS_BY_TEMPLATE_KEYSPACE, RECORDS_KEYSPACE, VERDICTS_KEYSPACE, Verdicts, VerdictsConfig,
};
