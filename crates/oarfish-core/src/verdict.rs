//! The static judgment on one template: what Jev said, and what it was asked.
//!
//! `Verdict` lives here — not beside its producer — because it is the contract
//! between the decision layer and everything downstream: the store caches it,
//! the engine routes on it, and the board renders it. `Decision`, the wire
//! shape the provider returned, lives in `oarfish-jev` and is mapped to this
//! on the way in, so a change to the `alpha` wire format stops at the crate
//! boundary.
//!
//! The question set is policy owned by `oarfish-engine`. This crate does not
//! know what the questions are; it only carries the answers and the
//! `questions_hash` they are valid for. A reworded question misses the cache
//! and re-judges rather than serving an answer to a question nobody asks.

use std::collections::BTreeMap;
use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Deserializer, Serialize, Serializer};
use ts_rs::TS;

use crate::{Alarm, TemplateId};

/// The blake3 hash of the canonicalised question set, truncated to 16 bytes.
///
/// Same discipline `bundle_hash` gives masking: a fixed, short identity for
/// "what was asked", carried on every verdict so a reworded question cannot
/// silently hit an answer meant for another. Displayed and serialized as
/// 32 lowercase hex characters.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, TS)]
#[ts(export, export_to = "oarfish.ts", type = "string")]
pub struct QuestionsHash([u8; 16]);

impl QuestionsHash {
    /// Hash arbitrary canonical bytes. Callers canonicalise first; this only
    /// hashes. Truncation to 16 bytes keeps the storage key short while
    /// leaving 128 bits against accidental collision.
    pub fn of(canonical: &[u8]) -> Self {
        let digest = blake3::hash(canonical);
        let mut bytes = [0u8; 16];
        bytes.copy_from_slice(&digest.as_bytes()[..16]);
        Self(bytes)
    }

    /// The raw bytes, for storage-key assembly.
    pub fn as_bytes(&self) -> &[u8; 16] {
        &self.0
    }

    /// Rebuild from raw storage bytes.
    pub fn from_bytes(bytes: [u8; 16]) -> Self {
        Self(bytes)
    }
}

impl fmt::Display for QuestionsHash {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for byte in self.0 {
            write!(f, "{byte:02x}")?;
        }
        Ok(())
    }
}

/// Why a string could not be read as a [`QuestionsHash`].
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum QuestionsHashError {
    #[error("questions hash must carry 32 hex characters, found {0}")]
    WrongLength(usize),
    #[error("questions hash contains a character that is not hex")]
    NotHex,
}

impl FromStr for QuestionsHash {
    type Err = QuestionsHashError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        if s.len() != 32 {
            return Err(QuestionsHashError::WrongLength(s.len()));
        }
        let mut bytes = [0u8; 16];
        for (i, chunk) in s.as_bytes().chunks(2).enumerate() {
            let pair = std::str::from_utf8(chunk).map_err(|_| QuestionsHashError::NotHex)?;
            bytes[i] = u8::from_str_radix(pair, 16).map_err(|_| QuestionsHashError::NotHex)?;
        }
        Ok(Self(bytes))
    }
}

impl Serialize for QuestionsHash {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.collect_str(self)
    }
}

impl<'de> Deserialize<'de> for QuestionsHash {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let raw = String::deserialize(deserializer)?;
        raw.parse().map_err(serde::de::Error::custom)
    }
}

/// One judged answer, in domain shape.
///
/// Mirrors the wire's three primitives without naming a provider field:
/// `choice` picks one option, `score` places the state on an ordered rubric,
/// `noul` is P(yes) for a yes/no question.
///
/// Externally tagged (the default) rather than matching the wire's
/// `{"type": ...}` shape, because the store writes `postcard` and postcard —
/// not being self-describing — only supports externally tagged enums. The
/// wire type in `oarfish-jev` keeps the provider's shape; this is the mapped
/// domain value.
///
/// `choice` and `score` carry the full distribution plus the provider's
/// calibrated `confidence`, both required — a payload without a confidence is
/// a parse error, never a default, because a defaulted `0.0` would route as
/// "record, no surface" and silently swallow a page. `noul` carries no
/// separate confidence on the wire; its value *is* the probability, so
/// [`VerdictAnswer::confidence`] returns `None` for it rather than inventing
/// one.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "lowercase")]
#[ts(export, export_to = "oarfish.ts")]
pub enum VerdictAnswer {
    Choice {
        choice: String,
        confidence: f64,
        probabilities: BTreeMap<String, f64>,
    },
    Score {
        score: f64,
        confidence: f64,
        probabilities: BTreeMap<String, f64>,
        /// No `skip_serializing_if`: verdicts are `postcard`-encoded for the
        /// store and postcard is not self-describing, so a skipped field
        /// shifts every field after it and corrupts the stream.
        #[serde(default)]
        legend: Option<BTreeMap<String, String>>,
    },
    Noul {
        noul: f64,
    },
}

impl VerdictAnswer {
    /// The calibrated confidence, where the wire provides one.
    ///
    /// `None` for `noul`: the wire carries no separate confidence field, and
    /// synthesizing one from the probability would be exactly the invention
    /// the type system is here to prevent.
    pub fn confidence(&self) -> Option<f64> {
        match self {
            VerdictAnswer::Choice { confidence, .. } | VerdictAnswer::Score { confidence, .. } => {
                Some(*confidence)
            }
            VerdictAnswer::Noul { .. } => None,
        }
    }
}

/// The cached judgment on one template.
///
/// Stored, cached and rendered; never re-asked. Valid only for the question
/// set in [`Verdict::questions_hash`] and the build in [`Verdict::model`]:
/// the gate proved a request for `typesafe/jev-1.13` is answered by a dated
/// build, so the resolved id is recorded rather than assumed.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "oarfish.ts")]
pub struct Verdict {
    pub template_id: TemplateId,
    pub questions_hash: QuestionsHash,
    /// The resolved, dated model id that answered — e.g.
    /// `typesafe/jev-1.13-20260917` — not the requested pin.
    pub model: String,
    /// One entry per question id asked.
    pub answers: BTreeMap<String, VerdictAnswer>,
    #[serde(with = "judged_at_serde")]
    #[ts(type = "string")]
    pub judged_at: time::OffsetDateTime,
}

/// One alarm with the judgment behind it, in the shape the board's detail
/// panel reads. The verdict and the record are optional by construction: an
/// alarm can be open while its template is still unjudged, and the board
/// renders that as "not yet judged" rather than a missing panel.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "oarfish.ts")]
pub struct AlarmDetail {
    pub alarm: Alarm,
    pub verdict: Option<Verdict>,
    pub record: Option<DecisionRecordView>,
}

/// The decision record cut down to what the board renders. The full record
/// (kept in `oarfish-store`) carries the exact state, questions and answers
/// JSON for replay tooling; the panel shows the provenance row: who answered,
/// when, at what token cost. Plain fields, so the store maps into it without
/// core depending on the store.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "oarfish.ts")]
pub struct DecisionRecordView {
    /// The resolved, dated build that answered — never the requested pin.
    pub model: String,
    #[serde(with = "time::serde::rfc3339")]
    #[ts(type = "string")]
    pub recorded_at: time::OffsetDateTime,
    #[ts(type = "number")]
    pub input_tokens: u64,
    #[ts(type = "number")]
    pub output_tokens: u64,
    pub cost: Option<f64>,
}

/// `OffsetDateTime` on both wires. Human-readable formats (JSON, and through
/// it the board) get RFC 3339; binary formats (`postcard`, for the fjall
/// store) get unix seconds. `time::serde::rfc3339` refuses the binary path,
/// so gating on `is_human_readable` is what keeps the store byte-exact.
mod judged_at_serde {
    use serde::{Deserialize, Deserializer, Serializer};
    use time::OffsetDateTime;

    pub fn serialize<S: Serializer>(at: &OffsetDateTime, serializer: S) -> Result<S::Ok, S::Error> {
        if serializer.is_human_readable() {
            time::serde::rfc3339::serialize(at, serializer)
        } else {
            serializer.serialize_i64(at.unix_timestamp())
        }
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(
        deserializer: D,
    ) -> Result<OffsetDateTime, D::Error> {
        if deserializer.is_human_readable() {
            time::serde::rfc3339::deserialize(deserializer)
        } else {
            let unix = i64::deserialize(deserializer)?;
            OffsetDateTime::from_unix_timestamp(unix).map_err(serde::de::Error::custom)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use time::OffsetDateTime;

    fn a_verdict() -> Verdict {
        Verdict {
            template_id: TemplateId::of("task <VAR:NUM> failed"),
            questions_hash: QuestionsHash::of(b"{}"),
            model: "typesafe/jev-1.13-20260917".to_owned(),
            answers: BTreeMap::from([(
                "kind".to_owned(),
                VerdictAnswer::Choice {
                    choice: "software".to_owned(),
                    confidence: 0.93,
                    probabilities: BTreeMap::from([
                        ("software".to_owned(), 0.9),
                        ("hardware".to_owned(), 0.1),
                    ]),
                },
            )]),
            judged_at: OffsetDateTime::UNIX_EPOCH,
        }
    }

    #[test]
    fn hashes_are_16_bytes_rendered_as_32_hex() {
        let hash = QuestionsHash::of(b"{}");
        let rendered = hash.to_string();
        assert_eq!(rendered.len(), 32);
        assert!(rendered.chars().all(|c| c.is_ascii_hexdigit()));
        assert_eq!(rendered.parse::<QuestionsHash>().expect("parse"), hash);
    }

    #[test]
    fn different_canonical_bytes_hash_differently() {
        assert_ne!(QuestionsHash::of(b"a"), QuestionsHash::of(b"b"));
    }

    #[test]
    fn hashes_serialize_as_a_plain_string() {
        let hash = QuestionsHash::of(b"{}");
        let json = serde_json::to_string(&hash).expect("serialize");
        assert_eq!(json, format!("\"{hash}\""));
        assert_eq!(
            serde_json::from_str::<QuestionsHash>(&json).expect("deserialize"),
            hash
        );
    }

    #[test]
    fn a_verdict_round_trips() {
        let verdict = a_verdict();
        let json = serde_json::to_string(&verdict).expect("serialize");
        assert_eq!(
            serde_json::from_str::<Verdict>(&json).expect("deserialize"),
            verdict
        );
    }

    #[test]
    fn confidence_is_present_on_choice_and_score_but_absent_on_noul() {
        let choice = VerdictAnswer::Choice {
            choice: "x".to_owned(),
            confidence: 0.93,
            probabilities: BTreeMap::new(),
        };
        let score = VerdictAnswer::Score {
            score: 1.5,
            confidence: 0.61,
            probabilities: BTreeMap::new(),
            legend: None,
        };
        let noul = VerdictAnswer::Noul { noul: 0.88 };
        assert_eq!(choice.confidence(), Some(0.93));
        assert_eq!(score.confidence(), Some(0.61));
        assert_eq!(noul.confidence(), None);
    }

    #[test]
    fn a_missing_confidence_fails_to_parse_rather_than_defaulting() {
        let json = serde_json::json!({
            "choice": {
                "choice": "software",
                "probabilities": {"software": 0.9}
            }
        });
        assert!(serde_json::from_value::<VerdictAnswer>(json).is_err());
    }

    #[test]
    fn postcard_preserves_a_verdict_exactly() {
        let verdict = a_verdict();
        let encoded = postcard::to_stdvec(&verdict).expect("encode");
        let back: Verdict = postcard::from_bytes(&encoded).expect("decode");
        assert_eq!(back, verdict);
    }

    /// `skip_serializing_if` is absent on purpose: under `postcard` a
    /// skipped `None` shifts the stream and corrupts the decode. A score
    /// without a legend — the common case — must round-trip.
    #[test]
    fn postcard_preserves_a_score_without_a_legend() {
        let mut verdict = a_verdict();
        verdict.answers.insert(
            "severity".to_owned(),
            VerdictAnswer::Score {
                score: 2.1,
                confidence: 0.61,
                probabilities: BTreeMap::from([("2".to_owned(), 0.61)]),
                legend: None,
            },
        );
        let encoded = postcard::to_stdvec(&verdict).expect("encode");
        let back: Verdict = postcard::from_bytes(&encoded).expect("decode");
        assert_eq!(back, verdict);
    }
}
