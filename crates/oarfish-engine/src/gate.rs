//! The raise gate: the typed reading of a verdict the machine raises on.
//!
//! The question set is policy owned by [`crate::static_questions`]; this
//! module is its reader. It looks up exactly the two answer ids the raise
//! path needs — [`SEVERITY_QUESTION`] and [`ACTIONABLE_QUESTION`] — through
//! the shared constants, so renaming a question breaks the build next to the
//! definition instead of silently closing the gate to `None` at 3am. A
//! missing or misshapen answer still closes the gate: an unjudged template
//! never raises, no matter how hard it bursts.

use oarfish_core::{Severity, Verdict, VerdictAnswer};

use crate::{ACTIONABLE_QUESTION, EngineConfig, SEVERITY_QUESTION};

/// What a verdict lets through the gate: the severity the board renders.
/// Actionability is consumed by the check itself — a `false` never becomes a
/// value — so it is not carried out.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Gate {
    pub severity: Severity,
}

/// Read the severity score answer as the X.733 severity the board renders.
/// The mapping is a rounding of the 0–3 score: `3 -> critical`, `2 ->
/// major`, `1 -> minor`, `0 -> info`. `cleared` is never reachable from a
/// score — it is a state the machine assigns, never a judgement Jev makes.
fn severity_of(verdict: &Verdict) -> Option<Severity> {
    match verdict.answers.get(SEVERITY_QUESTION)? {
        VerdictAnswer::Score { score, .. } => Some(match score.round() as i64 {
            3.. => Severity::Critical,
            2 => Severity::Major,
            1 => Severity::Minor,
            _ => Severity::Info,
        }),
        _ => None,
    }
}

/// Whether the `actionable` noul value clears the threshold. A missing or
/// misshapen answer closes the gate: an unjudged template never raises,
/// no matter how hard it bursts.
fn is_actionable(verdict: &Verdict, threshold: f64) -> Option<bool> {
    match verdict.answers.get(ACTIONABLE_QUESTION)? {
        VerdictAnswer::Noul { noul } => Some(*noul >= threshold),
        _ => None,
    }
}

/// The gate: actionable at or above the severity floor, else nothing raises.
pub fn gate(verdict: &Verdict, config: &EngineConfig) -> Option<Gate> {
    let severity = severity_of(verdict)?;
    if severity < config.gate_floor {
        return None;
    }
    if !is_actionable(verdict, config.actionable_threshold)? {
        return None;
    }
    Some(Gate { severity })
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use oarfish_core::{QuestionsHash, TemplateId};
    use time::OffsetDateTime;

    use super::*;

    #[test]
    fn severity_scores_round_to_the_x733_subset() {
        // The floor at `Info` so this tests the mapping, not the floor.
        let mut config = EngineConfig::default();
        config.gate_floor = Severity::Info;
        assert_eq!(
            gate(&verdict_with(2.6, 0.9), &config).map(|gate| gate.severity),
            Some(Severity::Critical)
        );
        assert_eq!(
            gate(&verdict_with(2.4, 0.9), &config).map(|gate| gate.severity),
            Some(Severity::Major)
        );
        assert_eq!(
            gate(&verdict_with(1.4, 0.9), &config).map(|gate| gate.severity),
            Some(Severity::Minor)
        );
        assert_eq!(
            gate(&verdict_with(0.2, 0.9), &config).map(|gate| gate.severity),
            Some(Severity::Info)
        );
    }

    #[test]
    fn cleared_is_never_reachable_from_a_score() {
        let mut config = EngineConfig::default();
        config.gate_floor = Severity::Info;
        for score in [0.0, 1.0, 2.0, 3.0, 100.0, -100.0] {
            assert_ne!(
                gate(&verdict_with(score, 0.9), &config).map(|gate| gate.severity),
                Some(Severity::Cleared)
            );
        }
    }

    #[test]
    fn a_missing_or_misshapen_answer_closes_the_gate() {
        let mut verdict = verdict_with(3.0, 0.9);
        verdict.answers.remove(SEVERITY_QUESTION);
        assert_eq!(gate(&verdict, &EngineConfig::default()), None);
        let mut verdict = verdict_with(3.0, 0.9);
        verdict.answers.remove(ACTIONABLE_QUESTION);
        assert_eq!(gate(&verdict, &EngineConfig::default()), None);
    }

    #[test]
    fn below_the_floor_or_threshold_nothing_passes() {
        let mut config = EngineConfig::default();
        config.gate_floor = Severity::Critical;
        assert_eq!(gate(&verdict_with(2.0, 0.9), &config), None);
        assert_eq!(
            gate(&verdict_with(3.0, 0.1), &EngineConfig::default()),
            None
        );
    }

    fn verdict_with(score: f64, actionable: f64) -> Verdict {
        Verdict {
            template_id: TemplateId::of("task <VAR:NUM> failed"),
            questions_hash: QuestionsHash::of(b"{}"),
            model: "typesafe/jev-1.13-20260917".to_owned(),
            answers: BTreeMap::from([
                (
                    SEVERITY_QUESTION.to_owned(),
                    VerdictAnswer::Score {
                        score,
                        confidence: 0.61,
                        probabilities: BTreeMap::new(),
                        legend: None,
                    },
                ),
                (
                    ACTIONABLE_QUESTION.to_owned(),
                    VerdictAnswer::Noul { noul: actionable },
                ),
            ]),
            judged_at: OffsetDateTime::UNIX_EPOCH,
        }
    }
}
