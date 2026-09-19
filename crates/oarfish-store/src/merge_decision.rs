//! One merge decision: whether two templates name the same event.
//!
//! Cached like a verdict — including the `no`s, which are as expensive to
//! re-derive as the `yes`es and far more common. Storing only the merges
//! would re-ask every rejected pair on every restart, turning a per-pair cost
//! into a per-restart-per-pair cost. Values are `postcard`, like verdicts.

use oarfish_core::{QuestionsHash, TemplateId};
use serde::{Deserialize, Serialize};

/// Whether a close pair describes the same event type, as judged once.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MergeDecision {
    /// Canonical pair, lesser id first. `(A, B)` and `(B, A)` share one row.
    pub id_lo: TemplateId,
    pub id_hi: TemplateId,
    /// The masked template texts that were judged, lo first. Carried so the
    /// alias table can serve the survivor's text for the verdict lookup after
    /// resolution, and so the decision is replayable without re-reading Drain.
    pub template_lo: String,
    pub template_hi: String,
    /// The merge question set this answers, so a rewording re-judges.
    pub questions_hash: QuestionsHash,
    /// The resolved, dated model id that answered — never the requested pin.
    pub model: String,
    /// The raw `noul` value: P(these describe the same event type).
    pub noul: f64,
    /// Whether the pair merged: `noul` at or above the threshold in force
    /// when judged. Derived once at write, so readers never re-derive it.
    pub merged: bool,
    /// Unix seconds. An integer so the value stays `postcard`-safe; the same
    /// discipline [`crate::DecisionRecord`] follows.
    pub judged_at_unix: i64,
}

impl MergeDecision {
    /// When the pair was judged.
    pub fn judged_at(&self) -> time::OffsetDateTime {
        time::OffsetDateTime::from_unix_timestamp(self.judged_at_unix)
            .unwrap_or(time::OffsetDateTime::UNIX_EPOCH)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn a_decision() -> MergeDecision {
        let lo = TemplateId::of("backup done files ok");
        let hi = TemplateId::of("backup done files ok extra");
        let (lo, hi) = if lo <= hi { (lo, hi) } else { (hi, lo) };
        MergeDecision {
            id_lo: lo,
            id_hi: hi,
            template_lo: "backup done files ok".to_owned(),
            template_hi: "backup done files ok extra".to_owned(),
            questions_hash: QuestionsHash::of(b"merge"),
            model: "typesafe/jev-1.13-20260917".to_owned(),
            noul: 0.91,
            merged: true,
            judged_at_unix: 0,
        }
    }

    #[test]
    fn a_decision_round_trips_through_postcard() {
        let decision = a_decision();
        let encoded = postcard::to_stdvec(&decision).expect("encode");
        let back: MergeDecision = postcard::from_bytes(&encoded).expect("decode");
        assert_eq!(back, decision);
    }

    #[test]
    fn a_rejected_pair_round_trips_too() {
        let mut decision = a_decision();
        decision.noul = 0.12;
        decision.merged = false;
        let encoded = postcard::to_stdvec(&decision).expect("encode");
        let back: MergeDecision = postcard::from_bytes(&encoded).expect("decode");
        assert_eq!(back, decision);
    }

    #[test]
    fn a_decision_round_trips_through_json() {
        let decision = a_decision();
        let json = serde_json::to_string(&decision).expect("serialize");
        assert_eq!(
            serde_json::from_str::<MergeDecision>(&json).expect("deserialize"),
            decision
        );
    }
}
