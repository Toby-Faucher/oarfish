//! The decision record: the trust feature.
//!
//! Every Jev call is persisted with its exact input — the state sent, the
//! questions asked — and its exact output, plus the resolved model id that
//! answered. That is what makes "why did this wake me three weeks ago"
//! answerable, and what makes the confidence thresholds tunable against a
//! real lab instead of superstitious.
//!
//! Records are keyed by ULID, so they sort chronologically for free. The
//! state, questions and answers ride as the exact JSON sent and received,
//! not as re-encoded domain types: replay must show what the provider saw,
//! byte for byte, and JSON strings are also what stay `postcard`-safe.

use std::collections::BTreeMap;

use oarfish_core::{QuestionsHash, TemplateId};
use serde::{Deserialize, Serialize};
use time::OffsetDateTime;
use ulid::Ulid;

/// One persisted Jev call, input and output together.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DecisionRecord {
    /// Chronological key, doubling as the disambiguator when one template is
    /// judged repeatedly across question revisions.
    pub id: Ulid,
    pub template_id: TemplateId,
    pub questions_hash: QuestionsHash,
    /// The resolved, dated build that answered — never the requested pin.
    pub model: String,
    /// The masked template text that was judged.
    pub template: String,
    /// The exact state sent, as JSON.
    pub state_json: String,
    /// The exact questions sent, as JSON.
    pub questions_json: String,
    /// The exact answers received, as JSON in the wire's shape.
    pub answers_json: String,
    pub input_tokens: u64,
    pub output_tokens: u64,
    /// No `skip_serializing_if`: this value is `postcard`-encoded and
    /// postcard is not self-describing, so a skipped field shifts every
    /// field after it and corrupts the stream. `None` encodes as its
    /// discriminant, which is what keeps the decode aligned.
    #[serde(default)]
    pub cost: Option<f64>,
    /// Unix seconds. An integer rather than an RFC 3339 string so the value
    /// stays `postcard`-safe; see [`DecisionRecord::recorded_at`].
    pub recorded_at_unix: i64,
}

impl DecisionRecord {
    /// When the call returned.
    pub fn recorded_at(&self) -> OffsetDateTime {
        OffsetDateTime::from_unix_timestamp(self.recorded_at_unix)
            .unwrap_or(OffsetDateTime::UNIX_EPOCH)
    }

    /// The answers in the wire's shape, for replay tooling.
    pub fn wire_answers(&self) -> Result<BTreeMap<String, oarfish_jev::Answer>, serde_json::Error> {
        serde_json::from_str(&self.answers_json)
    }
}

/// The 16-byte chronological key for one record.
pub fn record_key(id: &Ulid) -> [u8; 16] {
    id.to_bytes()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn a_record() -> DecisionRecord {
        DecisionRecord {
            id: Ulid::generate(),
            template_id: TemplateId::of("task <VAR:NUM> failed"),
            questions_hash: QuestionsHash::of(b"{}"),
            model: "typesafe/jev-1.13-20260917".to_owned(),
            template: "task <VAR:NUM> failed".to_owned(),
            state_json: r#"{"template":"task <VAR:NUM> failed"}"#.to_owned(),
            questions_json: "{}".to_owned(),
            answers_json: r#"{"kind":{"type":"choice","choice":"software","confidence":0.93,"probabilities":{"software":0.9}}}"#.to_owned(),
            input_tokens: 489,
            output_tokens: 75,
            cost: Some(0.0000205),
            recorded_at_unix: 0,
        }
    }

    #[test]
    fn a_record_round_trips_through_postcard() {
        let record = a_record();
        let encoded = postcard::to_stdvec(&record).expect("encode");
        let back: DecisionRecord = postcard::from_bytes(&encoded).expect("decode");
        assert_eq!(back, record);
    }

    /// The judge's records usually carry no cost — the provider omits it —
    /// and a skipped `None` would corrupt the `postcard` stream. This is the
    /// shape that actually gets stored.
    #[test]
    fn a_record_without_a_cost_round_trips_through_postcard() {
        let mut record = a_record();
        record.cost = None;
        let encoded = postcard::to_stdvec(&record).expect("encode");
        let back: DecisionRecord = postcard::from_bytes(&encoded).expect("decode");
        assert_eq!(back, record);
    }

    #[test]
    fn a_record_round_trips_through_json() {
        let record = a_record();
        let json = serde_json::to_string(&record).expect("serialize");
        assert_eq!(
            serde_json::from_str::<DecisionRecord>(&json).expect("deserialize"),
            record
        );
    }

    #[test]
    fn record_keys_sort_chronologically() {
        let first = Ulid::generate();
        std::thread::sleep(std::time::Duration::from_millis(2));
        let second = Ulid::generate();
        assert!(record_key(&first) < record_key(&second));
    }

    #[test]
    fn wire_answers_replay_in_the_providers_shape() {
        let answers = a_record().wire_answers().expect("replay");
        match answers.get("kind") {
            Some(oarfish_jev::Answer::Choice {
                choice, confidence, ..
            }) => {
                assert_eq!(choice, "software");
                assert_eq!(*confidence, 0.93);
            }
            other => panic!("expected the recorded choice, got {other:?}"),
        }
    }
}
