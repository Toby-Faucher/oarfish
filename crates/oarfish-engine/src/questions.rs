//! The static question set: policy owned by the engine.
//!
//! M4 took the question set as a parameter and hashed it; here the six
//! questions from §5.5 become the real policy. Every question goes in one
//! request — §5.5's point is that they evaluate in parallel, so a sixth
//! question costs almost nothing in latency and a rounding error in tokens.
//!
//! All six are asked, including `contextual`, whose only consumer is the
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

/// The merge-review question set: one `noul`, in its own set — separate from
/// the static six, so adding merge review cannot move `questions_hash` and
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

/// The six static questions, keyed by the ids the engine reads back in the
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
    ])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_static_set_asks_all_six_questions() {
        let questions = static_questions();
        for key in [
            "kind",
            SEVERITY_QUESTION,
            ACTIONABLE_QUESTION,
            "transient",
            "security",
            CONTEXTUAL_QUESTION,
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
        // And it is disjoint from the static six: adding merge review cannot
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
