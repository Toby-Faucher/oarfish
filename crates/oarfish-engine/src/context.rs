//! The contextual check: pending raises for flagged templates.
//!
//! Only templates the static verdict marked `contextual` reach here — the
//! template alone cannot settle whether their bursts matter, so the engine
//! asks Jev with the burst's window stats and its host's open alarms as
//! state. Three questions, §5.8's own set, separate from the static six so
//! adding them cannot move `questions_hash` and invalidate the verdict table.
//!
//! The engine never blocks on the check: the raise is what waits. When a
//! flagged template trips, the engine captures a host-scoped, bounded
//! snapshot from its open map, spawns [`run_check`], and marks the key
//! in-flight — the loop continues and nothing surfaces. The answer returns on
//! a channel; the engine re-validates and raises, or records, on apply.
//! Failure raises rather than swallows: an error or a timeout is a
//! [`CheckResult::Failed`], and the engine routes those at `Dashboard`.
//!
//! The check depends on the read-only [`OpenAlarmView`] trait rather than on
//! the state machine — which is also what lets the routing tests hand it
//! chosen snapshots instead of driving a whole engine to produce one.

use std::collections::BTreeMap;
use std::time::Duration;

use oarfish_core::{Alarm, AlarmId, Severity, TemplateId};
use oarfish_store::{Decide, DecisionRecord};
use time::OffsetDateTime;
use ulid::Ulid;

/// The answer id read as "is this burst worth surfacing at all".
pub const MATTERS_NOW_QUESTION: &str = "matters_now";
/// The answer id read as the correlated open alarm, or `none`.
pub const CORRELATES_WITH_QUESTION: &str = "correlates_with";
/// The answer id the alarm engine routes on.
pub const WAKE_SOMEONE_QUESTION: &str = "wake_someone";
/// The `correlates_with` option meaning a second, unrelated incident.
pub const NONE_OPTION: &str = "none";

/// Open alarms carried into one check's state, at most
/// [`crate::DEFAULT_MAX_CONTEXT_ALARMS`], host-scoped, oldest first. The
/// template rides truncated: an excerpt identifies the alarm, and the token
/// budget is shared with window stats and questions.
pub const TEMPLATE_EXCERPT_LEN: usize = 160;

/// One open alarm as the check sees it: what §5.8 lists — id, template id,
/// severity, host, opened-at, one template excerpt.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ContextAlarm {
    pub id: AlarmId,
    pub template_id: TemplateId,
    pub severity: Severity,
    pub host: String,
    pub opened_at_unix: i64,
    pub template_excerpt: String,
}

/// What the check needs beyond the snapshot: the burst itself.
#[derive(Debug, Clone)]
pub struct BurstContext {
    pub template_id: TemplateId,
    pub template: String,
    pub host: String,
    pub severity: Severity,
    pub stats: crate::windows::WindowStats,
}

/// A read-only view of open alarms. The state machine implements this for
/// itself; tests hand the check chosen snapshots through it.
pub trait OpenAlarmView {
    /// Open alarms, oldest first.
    fn open_alarms(&self) -> Vec<Alarm>;
}

/// Capture the check's snapshot: open alarms on the burst's host, oldest
/// first, bounded to fit the token budget. Carries what §5.8 lists — id,
/// template id, severity, host, opened-at, one template excerpt — and nothing
/// else.
pub fn capture_snapshot(
    view: &impl OpenAlarmView,
    host: &str,
    max_alarms: usize,
) -> Vec<ContextAlarm> {
    view.open_alarms()
        .into_iter()
        .filter(|alarm| alarm.host == host)
        .take(max_alarms)
        .map(|alarm| ContextAlarm {
            id: alarm.id,
            template_id: alarm.template_id,
            severity: alarm.severity,
            host: alarm.host,
            opened_at_unix: alarm.opened_at.unix_timestamp(),
            template_excerpt: truncate(&alarm.template, TEMPLATE_EXCERPT_LEN),
        })
        .collect()
}

fn truncate(template: &str, max: usize) -> String {
    if template.len() <= max {
        template.to_owned()
    } else {
        template
            .char_indices()
            .take_while(|(i, _)| *i < max)
            .map(|(_, c)| c)
            .collect()
    }
}

/// The three contextual questions over one snapshot. `correlates_with` is a
/// `choice` over the ids in that snapshot plus `none`, so the question cannot
/// name an alarm the model was never shown.
pub fn context_questions(snapshot: &[ContextAlarm]) -> BTreeMap<String, oarfish_jev::Question> {
    let mut correlates: serde_json::Map<String, serde_json::Value> = serde_json::Map::new();
    for alarm in snapshot {
        correlates.insert(
            alarm.id.to_string(),
            serde_json::Value::String(alarm.template_excerpt.clone()),
        );
    }
    correlates.insert(
        NONE_OPTION.to_owned(),
        serde_json::Value::String(
            "A second, unrelated incident: not the same incident as any alarm above.".to_owned(),
        ),
    );
    BTreeMap::from([
        (
            MATTERS_NOW_QUESTION.to_owned(),
            oarfish_jev::Question::noul(
                serde_json::json!(
                    "Is this burst worth surfacing at all — a real incident a \
                     human should see — rather than noise that should stay \
                     recorded but silent?"
                ),
                None,
            ),
        ),
        (
            CORRELATES_WITH_QUESTION.to_owned(),
            oarfish_jev::Question::choice(
                serde_json::json!(
                    "Is this burst the same incident as one of the open alarms \
                     above on this host, or a second one? Pick the matching \
                     alarm id, or none."
                ),
                serde_json::Value::Object(correlates),
            ),
        ),
        (
            WAKE_SOMEONE_QUESTION.to_owned(),
            oarfish_jev::Question::noul(
                serde_json::json!(
                    "Does this burst justify a push notification that wakes \
                     someone — down or data at risk right now — rather than a \
                     card on the dashboard?"
                ),
                None,
            ),
        ),
    ])
}

/// What a finished check hands back: the interpreted answers plus the
/// decision record, built but not yet saved. The engine persists the record
/// before trusting the answers — a crash between the two leaves an orphan
/// record, which is harmless and replayable, rather than a trusted answer
/// with no provenance.
#[derive(Debug, Clone)]
pub struct PendingAnswer {
    pub matters_now: f64,
    pub correlates_with: Option<AlarmId>,
    pub wake_someone: f64,
    pub record: DecisionRecord,
}

/// What one check resolves to. `Failed` covers the transport error, the
/// timeout, and the unreadable answer alike: a check that cannot answer must
/// not be able to suppress an alarm the window already decided was worth
/// raising, so the engine routes every failure at `Dashboard`.
#[derive(Debug)]
pub enum CheckResult {
    Answered(Box<PendingAnswer>),
    Failed,
}

/// What one check hands the engine to apply on return: the interpreted
/// answer — or the failure — with the burst's key material carried through
/// the round trip so the engine can raise without re-reading anything.
#[derive(Debug)]
pub enum CheckOutcome {
    Answered {
        template_id: TemplateId,
        template: String,
        host: String,
        severity: Severity,
        answer: Box<PendingAnswer>,
    },
    Failed {
        template_id: TemplateId,
        template: String,
        host: String,
        severity: Severity,
    },
}

/// Ask everything in one request: the three questions evaluate in parallel.
/// The snapshot in the state is persisted verbatim in the decision record, so
/// the correlation is replayable.
pub async fn run_check<D: Decide>(
    decide: &D,
    view: &impl OpenAlarmView,
    burst: &BurstContext,
    max_alarms: usize,
    timeout: Duration,
) -> CheckResult {
    let snapshot = capture_snapshot(view, &burst.host, max_alarms);
    let state = serde_json::json!({
        "template": burst.template,
        "template_id": burst.template_id.to_string(),
        "host": burst.host,
        "window": {
            "short_total": burst.stats.short_total,
            "long_total": burst.stats.long_total,
            "distinct_hosts": burst.stats.distinct_hosts,
            "first_seen_secs": burst.stats.first_seen_secs,
            "span_secs": burst.stats.span_secs,
        },
        "open_alarms": snapshot,
    });
    let questions = context_questions(&snapshot);
    let questions_hash = oarfish_jev::questions_hash(&questions);

    let decision = match tokio::time::timeout(timeout, decide.decide(&state, &questions)).await {
        Ok(Ok(decision)) => decision,
        Ok(Err(error)) => {
            tracing::warn!(%error, "contextual check failed; the burst raises without it");
            return CheckResult::Failed;
        }
        Err(_) => {
            tracing::warn!("contextual check timed out; the burst raises without it");
            return CheckResult::Failed;
        }
    };

    let matters_now = match decision.answers.get(MATTERS_NOW_QUESTION) {
        Some(oarfish_jev::Answer::Noul { noul }) => *noul,
        other => {
            tracing::warn!(
                ?other,
                "contextual check returned no matters_now; the burst raises without it"
            );
            return CheckResult::Failed;
        }
    };
    let wake_someone = match decision.answers.get(WAKE_SOMEONE_QUESTION) {
        Some(oarfish_jev::Answer::Noul { noul }) => *noul,
        other => {
            tracing::warn!(
                ?other,
                "contextual check returned no wake_someone; the burst raises without it"
            );
            return CheckResult::Failed;
        }
    };
    let correlates_with = match decision.answers.get(CORRELATES_WITH_QUESTION) {
        Some(oarfish_jev::Answer::Choice { choice, .. }) if choice == NONE_OPTION => None,
        Some(oarfish_jev::Answer::Choice { choice, .. }) => match choice.parse::<AlarmId>() {
            Ok(id) => Some(id),
            Err(_) => {
                tracing::warn!(
                    choice = %choice,
                    "contextual check named an alarm it was never shown; treating as none"
                );
                None
            }
        },
        other => {
            tracing::warn!(
                ?other,
                "contextual check returned no correlates_with; the burst raises without it"
            );
            return CheckResult::Failed;
        }
    };

    let judged_at = OffsetDateTime::now_utc();
    let state_json = state.to_string();
    let questions_json = match serde_json::to_string(&questions) {
        Ok(json) => json,
        Err(error) => {
            tracing::warn!(%error, "context questions would not serialize; the burst raises without it");
            return CheckResult::Failed;
        }
    };
    let answers_json = match serde_json::to_string(&decision.answers) {
        Ok(json) => json,
        Err(error) => {
            tracing::warn!(%error, "context answers would not serialize; the burst raises without it");
            return CheckResult::Failed;
        }
    };
    let (input_tokens, output_tokens, cost) = match &decision.usage {
        Some(usage) => (usage.input_tokens, usage.output_tokens, usage.cost),
        None => (0, 0, None),
    };
    CheckResult::Answered(Box::new(PendingAnswer {
        matters_now,
        correlates_with,
        wake_someone,
        record: DecisionRecord {
            id: Ulid::generate(),
            template_id: burst.template_id,
            questions_hash,
            model: decision.model,
            template: burst.template.clone(),
            state_json,
            questions_json,
            answers_json,
            input_tokens,
            output_tokens,
            cost,
            recorded_at_unix: judged_at.unix_timestamp(),
        },
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    struct FakeView(Vec<Alarm>);

    impl OpenAlarmView for FakeView {
        fn open_alarms(&self) -> Vec<Alarm> {
            let mut alarms = self.0.clone();
            alarms.sort_by_key(|alarm| alarm.opened_at);
            alarms
        }
    }

    /// The second adapter again: a canned provider with chosen answers — the
    /// confidence values the routing table keys off.
    #[derive(Debug, Clone)]
    struct FakeDecide {
        matters_now: f64,
        correlates_with: Option<String>,
        wake_someone: f64,
    }

    impl Decide for FakeDecide {
        fn decide<'a>(
            &'a self,
            _state: &'a serde_json::Value,
            _questions: &'a BTreeMap<String, oarfish_jev::Question>,
        ) -> impl std::future::Future<Output = Result<oarfish_jev::Decision, oarfish_jev::Error>>
        + Send
        + 'a {
            let this = self.clone();
            async move {
                Ok(oarfish_jev::Decision {
                    answers: BTreeMap::from([
                        (
                            MATTERS_NOW_QUESTION.to_owned(),
                            oarfish_jev::Answer::Noul {
                                noul: this.matters_now,
                            },
                        ),
                        (
                            CORRELATES_WITH_QUESTION.to_owned(),
                            oarfish_jev::Answer::Choice {
                                choice: this
                                    .correlates_with
                                    .clone()
                                    .unwrap_or_else(|| NONE_OPTION.to_owned()),
                                confidence: 0.9,
                                probabilities: BTreeMap::new(),
                            },
                        ),
                        (
                            WAKE_SOMEONE_QUESTION.to_owned(),
                            oarfish_jev::Answer::Noul {
                                noul: this.wake_someone,
                            },
                        ),
                    ]),
                    model: "fake-build-1".to_owned(),
                    usage: None,
                })
            }
        }
    }

    fn burst(host: &str) -> BurstContext {
        BurstContext {
            template_id: TemplateId::of("db slow query past threshold"),
            template: "db slow query past threshold".to_owned(),
            host: host.to_owned(),
            severity: Severity::Major,
            stats: crate::windows::WindowStats {
                short_total: 9,
                long_total: 12,
                distinct_hosts: 1,
                first_seen_secs: 0,
                span_secs: 400,
            },
        }
    }

    fn alarm_on(host: &str, template: &str) -> Alarm {
        Alarm {
            id: AlarmId::generate(),
            template_id: TemplateId::of(template),
            template: template.to_owned(),
            severity: Severity::Major,
            host: host.to_owned(),
            lane: oarfish_core::Lane::Dashboard,
            count: 3,
            opened_at: OffsetDateTime::UNIX_EPOCH,
        }
    }

    #[test]
    fn the_snapshot_is_host_scoped_bounded_and_excerpted() {
        let other_host = alarm_on("db02", "unrelated alarm on another host");
        let mut local = alarm_on("db01", &"x".repeat(400));
        local.opened_at = OffsetDateTime::UNIX_EPOCH;
        let view = FakeView(vec![other_host, local]);
        let snapshot = capture_snapshot(&view, "db01", 8);
        assert_eq!(snapshot.len(), 1);
        assert_eq!(snapshot[0].host, "db01");
        assert_eq!(snapshot[0].template_excerpt.len(), TEMPLATE_EXCERPT_LEN);

        // Bounded: two alarms in, one slot out.
        let view = FakeView(vec![alarm_on("db01", "first"), alarm_on("db01", "second")]);
        assert_eq!(capture_snapshot(&view, "db01", 1).len(), 1);
    }

    #[test]
    fn correlates_is_a_choice_over_exactly_the_shown_ids_plus_none() {
        let first = alarm_on("db01", "first");
        let second = alarm_on("db01", "second");
        let view = FakeView(vec![first.clone(), second.clone()]);
        let snapshot = capture_snapshot(&view, "db01", 8);
        let questions = context_questions(&snapshot);
        let criteria = questions
            .get(CORRELATES_WITH_QUESTION)
            .expect("correlates_with")
            .criteria
            .clone()
            .expect("a choice carries criteria");
        assert!(criteria.get(first.id.to_string()).is_some());
        assert!(criteria.get(second.id.to_string()).is_some());
        assert!(criteria.get(NONE_OPTION).is_some());
        assert_eq!(criteria.as_object().expect("object").len(), 3);

        // An empty snapshot asks none alone: the question cannot name an
        // alarm the model was never shown.
        let empty = context_questions(&[]);
        let criteria = empty
            .get(CORRELATES_WITH_QUESTION)
            .expect("correlates_with")
            .criteria
            .clone()
            .expect("criteria");
        assert_eq!(criteria.as_object().expect("object").len(), 1);
    }

    /// Chosen confidences route: 0.94 pages, 0.61 does not. The routing table
    /// itself lives in `machine`; here the check hands it the provider's
    /// numbers unchanged.
    #[tokio::test]
    async fn chosen_confidences_come_back_unchanged() {
        let view = FakeView(vec![]);
        for (wake, lane) in [
            (0.94, oarfish_core::Lane::Page),
            (0.61, oarfish_core::Lane::Dashboard),
        ] {
            let decide = FakeDecide {
                matters_now: 0.9,
                correlates_with: None,
                wake_someone: wake,
            };
            match run_check(&decide, &view, &burst("db01"), 8, Duration::from_secs(5)).await {
                CheckResult::Answered(answer) => {
                    assert_eq!(answer.wake_someone, wake);
                    assert_eq!(crate::machine::route(Some(answer.wake_someone)), lane);
                    assert_eq!(answer.correlates_with, None);
                }
                CheckResult::Failed => panic!("a full answer must not fail"),
            }
        }
    }

    /// The wire correlation arrives as the named id; re-validation against
    /// live state is the engine's job on apply, not the check's.
    #[tokio::test]
    async fn the_wire_correlation_arrives_as_the_named_id() {
        let open = alarm_on("db01", "the correlated alarm");
        let view = FakeView(vec![open.clone()]);
        let decide = FakeDecide {
            matters_now: 0.9,
            correlates_with: Some(open.id.to_string()),
            wake_someone: 0.7,
        };
        match run_check(&decide, &view, &burst("db01"), 8, Duration::from_secs(5)).await {
            CheckResult::Answered(answer) => {
                assert_eq!(answer.correlates_with, Some(open.id));
                // And the snapshot was persisted verbatim in the record, so
                // the correlation is replayable.
                let state: serde_json::Value =
                    serde_json::from_str(&answer.record.state_json).expect("state is JSON");
                assert_eq!(state["open_alarms"].as_array().expect("array").len(), 1);
                assert_eq!(
                    state["open_alarms"][0]["id"],
                    serde_json::json!(open.id.to_string())
                );
            }
            CheckResult::Failed => panic!("a full answer must not fail"),
        }
    }

    /// A choice the model was never shown resolves to none rather than
    /// inventing a correlation.
    #[tokio::test]
    async fn an_unnamed_correlation_resolves_to_none() {
        let view = FakeView(vec![]);
        let decide = FakeDecide {
            matters_now: 0.9,
            correlates_with: Some("not-a-ulid".to_owned()),
            wake_someone: 0.7,
        };
        match run_check(&decide, &view, &burst("db01"), 8, Duration::from_secs(5)).await {
            CheckResult::Answered(answer) => assert_eq!(answer.correlates_with, None),
            CheckResult::Failed => panic!("a garbage choice degrades to none, not failure"),
        }
    }
}
