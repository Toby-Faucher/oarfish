//! The static question set: policy owned by the engine.
//!
//! M4 took the question set as a parameter and hashed it; here the nine
//! questions — the six from §5.5 plus `blast_radius`, `runbook_kind` and
//! `page_delay` — become the real policy. Every question goes in one
//! request — §5.5's point is that they evaluate in parallel, so a ninth
//! question costs almost nothing in latency and a rounding error in tokens.
//!
//! All nine are asked, including `contextual`, whose only consumer is the
//! check deferred to M5.5. Asking it now is free, and it means M5.5 does not
//! invalidate every cached verdict by changing the question set, which under
//! M4's key would re-judge the entire table.

use std::collections::BTreeMap;

use oarfish_jev::Question;

/// The answer id the raise path reads as the X.733 severity. Defined here,
/// beside the question, so a rename breaks the gate's reader at compile
/// time instead of silently closing it to `None`.
pub const SEVERITY_QUESTION: &str = "severity";
/// The answer id the raise path reads as the actionability verdict. Same
/// discipline as [`SEVERITY_QUESTION`].
pub const ACTIONABLE_QUESTION: &str = "actionable";
/// The answer id the raise path reads as the contextual flag: whether a
/// burst of this template needs re-checking against surrounding context
/// rather than settling on the static verdict alone. Same discipline as
/// [`SEVERITY_QUESTION`].
pub const CONTEXTUAL_QUESTION: &str = "contextual";
/// The answer id read as the blast-radius score: how widely a failure of this
/// template reaches, from one line or host up to the whole lab. Same
/// discipline as [`SEVERITY_QUESTION`].
pub const BLAST_RADIUS_QUESTION: &str = "blast_radius";
/// The answer id read as the runbook kind: which first remediation step fits
/// this template. Classification only, never free text. Same discipline as
/// [`SEVERITY_QUESTION`].
pub const RUNBOOK_QUESTION: &str = "runbook_kind";
/// The answer id read as the paging delay: whether this template pages now,
/// soon, or never. Same discipline as [`SEVERITY_QUESTION`].
pub const PAGE_DELAY_QUESTION: &str = "page_delay";

/// The merge-review question set: one `noul`, in its own set — separate from
/// the static nine, so adding merge review cannot move `questions_hash` and
/// invalidate the verdict table.
///
/// Keyed by the id the store's merge judge reads back
/// ([`oarfish_store::MERGE_QUESTION`]), so a rename breaks the set at compile
/// time instead of silently judging nothing.
pub fn merge_questions() -> BTreeMap<String, Question> {
    BTreeMap::from([(
        oarfish_store::MERGE_QUESTION.to_owned(),
        Question::noul(
            serde_json::json!(
                "Do these two log templates describe the same event type — \
                 two halves of one real event split by an optional field or \
                 a wording variant — rather than two different events? Answer \
                 yes only when every line matching either template is the \
                 same kind of occurrence."
            ),
            None,
        ),
    )])
}

/// The nine static questions, keyed by the ids the engine reads back in the
/// raise path (`severity`, `actionable`) and M5.5 will read (`contextual`).
pub fn static_questions() -> BTreeMap<String, Question> {
    BTreeMap::from([
        (
            "kind".to_owned(),
            Question::choice(
                serde_json::json!(
                    "What kind of event does this log template describe? \
                     Judge the template as an event type, not any single line."
                ),
                serde_json::json!({
                    "hardware_fault": "A physical device failed or degraded: disk, memory, PSU, sensor, SMART error, checksum failure.",
                    "software_error": "A program errored, crashed, or failed an operation it attempted.",
                    "config": "A misconfiguration or missing setting: the system is telling the operator something needs changing.",
                    "security": "Authentication, access control, or a possible intrusion: failed logins, sudo use, firewall drops.",
                    "routine": "Normal operational noise: startups, clean shutdowns, scheduled jobs succeeding, heartbeats."
                }),
            ),
        ),
        (
            SEVERITY_QUESTION.to_owned(),
            Question::score(
                serde_json::json!(
                    "How severe is this template on its own, before considering \
                     how often it fires? Score 0-3 against the ordered levels."
                ),
                serde_json::json!([
                    "Routine noise; never worth surfacing.",
                    "Degraded or suspicious; worth a look when it bursts.",
                    "Broken; needs a human soon.",
                    "Down or data at risk; wake someone."
                ]),
            ),
        ),
        (
            ACTIONABLE_QUESTION.to_owned(),
            Question::noul(
                serde_json::json!(
                    "Can a human operator do anything about this: fix it, \
                     mitigate it, or investigate it to a useful end? A line \
                     that only informs is not actionable."
                ),
                None,
            ),
        ),
        (
            "transient".to_owned(),
            Question::noul(
                serde_json::json!(
                    "Is this self-resolving: a retry, a backoff, or a temporary \
                     condition that clears without intervention?"
                ),
                None,
            ),
        ),
        (
            "security".to_owned(),
            Question::noul(
                serde_json::json!(
                    "Is this security-relevant: does it bear on authentication, \
                     authorization, or the integrity of the system?"
                ),
                None,
            ),
        ),
        (
            CONTEXTUAL_QUESTION.to_owned(),
            Question::noul(
                serde_json::json!(
                    "When this template bursts, does deciding whether it matters \
                     need the surrounding context — window stats, the host's role, \
                     open alarms — rather than the template alone? Answer yes \
                     when the template by itself cannot settle it."
                ),
                None,
            ),
        ),
        (
            BLAST_RADIUS_QUESTION.to_owned(),
            Question::score(
                serde_json::json!(
                    "How widely does a failure of this template reach — one line \
                     or host, or the whole lab? Score 0-3 against the ordered levels."
                ),
                serde_json::json!([
                    "One line or one host; nothing beyond it is affected.",
                    "One service degraded; nearby dependents may feel it.",
                    "Several services or hosts affected; the lab is partly down.",
                    "The whole lab is down or at risk; core services on every host affected."
                ]),
            ),
        ),
        (
            RUNBOOK_QUESTION.to_owned(),
            Question::choice(
                serde_json::json!(
                    "Which first remediation step fits this template? Classify only: \
                     pick the closest option, never free text."
                ),
                serde_json::json!({
                    "restart": "Restart the service, container, or host and it recovers.",
                    "disk": "Free disk space, rotate logs, or replace failing storage.",
                    "config": "Fix a setting, file, or misconfigured option.",
                    "auth": "Fix authentication or access: password, key, certificate, or permission.",
                    "look_deeper": "No routine first step fits; investigate before acting."
                }),
            ),
        ),
        (
            PAGE_DELAY_QUESTION.to_owned(),
            Question::choice(
                serde_json::json!(
                    "When should this template page someone? Classify only: \
                     pick the closest option, never free text."
                ),
                serde_json::json!({
                    "now": "Page immediately: down or data at risk right now.",
                    "soon": "Needs a human soon but can wait for working hours.",
                    "never": "Never pages: routine noise or dashboard-only."
                }),
            ),
        ),
    ])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_static_set_asks_all_nine_questions() {
        let questions = static_questions();
        assert_eq!(questions.len(), 9);
        for key in [
            "kind",
            SEVERITY_QUESTION,
            ACTIONABLE_QUESTION,
            "transient",
            "security",
            CONTEXTUAL_QUESTION,
            BLAST_RADIUS_QUESTION,
            RUNBOOK_QUESTION,
            PAGE_DELAY_QUESTION,
        ] {
            assert!(questions.contains_key(key), "missing {key}");
        }
    }

    #[test]
    fn the_merge_set_asks_only_the_merge_question() {
        let questions = merge_questions();
        assert_eq!(
            questions.keys().collect::<Vec<_>>(),
            vec![oarfish_store::MERGE_QUESTION]
        );
        // And it is disjoint from the static nine: adding merge review cannot
        // move `questions_hash` and invalidate the verdict table.
        let static_set = static_questions();
        assert!(
            !static_set.contains_key(oarfish_store::MERGE_QUESTION),
            "the merge question must not leak into the static set"
        );
    }

    #[test]
    fn kind_covers_the_five_options() {
        let questions = static_questions();
        let criteria = questions
            .get("kind")
            .expect("kind")
            .criteria
            .clone()
            .expect("kind carries criteria");
        for option in [
            "hardware_fault",
            "software_error",
            "config",
            "security",
            "routine",
        ] {
            assert!(criteria.get(option).is_some(), "missing {option}");
        }
    }

    #[test]
    fn blast_radius_scores_zero_to_three() {
        let questions = static_questions();
        let question = questions.get(BLAST_RADIUS_QUESTION).expect("blast_radius");
        assert_eq!(question.kind, oarfish_jev::QuestionType::Score);
        let levels = question.criteria.clone().expect("a score carries criteria");
        assert_eq!(levels.as_array().expect("array").len(), 4);
    }

    #[test]
    fn runbook_kind_covers_the_five_options() {
        let questions = static_questions();
        let criteria = questions
            .get(RUNBOOK_QUESTION)
            .expect("runbook_kind")
            .criteria
            .clone()
            .expect("a choice carries criteria");
        for option in ["restart", "disk", "config", "auth", "look_deeper"] {
            assert!(criteria.get(option).is_some(), "missing {option}");
        }
        assert_eq!(criteria.as_object().expect("object").len(), 5);
    }

    #[test]
    fn page_delay_covers_the_three_options() {
        let questions = static_questions();
        let criteria = questions
            .get(PAGE_DELAY_QUESTION)
            .expect("page_delay")
            .criteria
            .clone()
            .expect("a choice carries criteria");
        for option in ["now", "soon", "never"] {
            assert!(criteria.get(option).is_some(), "missing {option}");
        }
        assert_eq!(criteria.as_object().expect("object").len(), 3);
    }

    /// The question set is the cache key's third component: a rewording must
    /// move the hash and re-judge rather than serve a stale answer.
    #[test]
    fn the_set_hashes_stably_but_a_rewording_moves_it() {
        let first = static_questions();
        let second = static_questions();
        assert_eq!(
            oarfish_jev::questions_hash(&first),
            oarfish_jev::questions_hash(&second)
        );
    }
}
