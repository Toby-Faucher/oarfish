//! An open alarm: the thing the whole daemon exists to avoid raising.

use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Serialize};
use time::OffsetDateTime;
use ts_rs::TS;
use ulid::Ulid;

use crate::{Severity, TemplateId};

/// A ULID, so the store gets chronological range scans for free.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, TS)]
#[ts(export, export_to = "oarfish.ts", type = "string")]
pub struct AlarmId(Ulid);

impl AlarmId {
    pub fn generate() -> Self {
        Self(Ulid::generate())
    }

    /// The 16-byte chronological key for the `alarms` keyspace. ULID bytes
    /// sort in time order, so a range scan is a timeline for free.
    pub fn to_bytes(&self) -> [u8; 16] {
        self.0.to_bytes()
    }

    /// Rebuild from raw storage bytes.
    pub fn from_bytes(bytes: [u8; 16]) -> Self {
        Self(Ulid::from_bytes(bytes))
    }
}

impl fmt::Display for AlarmId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

/// Why a string could not be read as an [`AlarmId`].
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum AlarmIdError {
    #[error("alarm id is not a ULID: {0}")]
    NotUlid(String),
}

impl FromStr for AlarmId {
    type Err = AlarmIdError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        s.parse::<Ulid>()
            .map(Self)
            .map_err(|_| AlarmIdError::NotUlid(s.to_owned()))
    }
}

/// Where an alarm would surface. Thresholds scale with the stakes: waking
/// someone needs more certainty than drawing a card on a dashboard.
///
/// This rides on [`Alarm`] rather than beside it so the raise publishes where
/// it routed: M5 raises land [`Lane::Dashboard`], and M5.5's contextual check
/// makes [`Lane::Page`] reachable with a real `wake_someone` behind it.
/// Nothing is on the other side of the page lane until delivery lands — the
/// signal and its delivery are separable, and landing the signal first means
/// delivery builds against a value it can observe.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "oarfish.ts")]
pub enum Lane {
    /// `wake_someone >= 0.90`: page via ntfy (delivery lands after M5.5).
    Page,
    /// `0.60 - 0.90`, and everything that never saw a contextual check.
    Dashboard,
    /// `< 0.60`: record, no surface.
    Record,
}

/// One alarm, in the shape the board reads it.
///
/// There is no `title` and no summary field, by design: oarfish classifies, it
/// does not narrate. The display line is the masked template, which is the same
/// artifact `Template.svelte` draws, so nothing here is generated prose.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "oarfish.ts")]
pub struct Alarm {
    pub id: AlarmId,
    pub template_id: TemplateId,
    /// The masked template, verbatim. The alarm's display line.
    pub template: String,
    pub severity: Severity,
    pub host: String,
    /// Where this alarm routed. M5 raises land here as `Dashboard`; the
    /// contextual check routes flagged bursts by `wake_someone`.
    pub lane: Lane,
    /// Occurrences folded into this alarm. Exported as a TypeScript `number`
    /// rather than ts-rs's default `bigint`, because it crosses the wire as a
    /// JSON number.
    #[ts(type = "number")]
    pub count: u64,
    /// `time` has no ts-rs integration, so the TypeScript type is declared by
    /// hand and the wire format is pinned to RFC 3339 rather than `time`'s
    /// default component encoding.
    #[serde(with = "time::serde::rfc3339")]
    #[ts(type = "string")]
    pub opened_at: OffsetDateTime,
}

/// What changed, in the shape the board reads: one alarm raised, updated
/// or cleared, plus the `Resync` marker.
///
/// The engine publishes these on a `broadcast` channel that SSE handlers
/// subscribe to. `Resync` carries no payload: a lagging receiver skipped an
/// unknowable set of messages — possibly a `Cleared` — so the board
/// re-fetches `/api/alarms` rather than trusting a stream it knows skipped.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "oarfish.ts")]
pub enum AlarmChange {
    Raised(Alarm),
    Updated(Alarm),
    Cleared(AlarmId),
    Resync,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn an_alarm() -> Alarm {
        Alarm {
            id: AlarmId::generate(),
            template_id: TemplateId::of("EXT4-fs error (device <VAR:DEV>)"),
            template: "EXT4-fs error (device <VAR:DEV>)".to_owned(),
            severity: Severity::Critical,
            host: "nas01".to_owned(),
            lane: Lane::Dashboard,
            count: 14,
            opened_at: OffsetDateTime::UNIX_EPOCH,
        }
    }

    /// Alarm ids are ULIDs so the store gets chronological range scans for
    /// free. A UUIDv4 would scatter them across the keyspace.
    #[test]
    fn alarm_ids_sort_chronologically() {
        let first = AlarmId::generate();
        std::thread::sleep(std::time::Duration::from_millis(2));
        let second = AlarmId::generate();

        assert!(first < second);
        assert!(first.to_string() < second.to_string());
    }

    #[test]
    fn alarm_ids_serialize_as_a_plain_string() {
        let id = AlarmId::generate();
        assert_eq!(
            serde_json::to_string(&id).expect("serialize"),
            format!("\"{id}\"")
        );
    }

    #[test]
    fn timestamps_cross_the_wire_as_rfc_3339() {
        let json = serde_json::to_value(an_alarm()).expect("serialize");
        assert_eq!(json["opened_at"], serde_json::json!("1970-01-01T00:00:00Z"));
    }

    #[test]
    fn alarm_ids_round_trip_through_their_string_form() {
        let id = AlarmId::generate();
        assert_eq!(id.to_string().parse::<AlarmId>().expect("parse"), id);
        assert!("not-a-ulid".parse::<AlarmId>().is_err());
    }

    #[test]
    fn lanes_serialize_as_plain_strings() {
        for (lane, json) in [
            (Lane::Page, r#""Page""#),
            (Lane::Dashboard, r#""Dashboard""#),
            (Lane::Record, r#""Record""#),
        ] {
            assert_eq!(serde_json::to_string(&lane).expect("serialize"), json);
            assert_eq!(
                serde_json::from_str::<Lane>(json).expect("deserialize"),
                lane
            );
        }
    }
    #[test]
    fn alarm_ids_round_trip_through_storage_bytes() {
        let id = AlarmId::generate();
        assert_eq!(AlarmId::from_bytes(id.to_bytes()), id);
    }

    #[test]
    fn an_alarm_round_trips() {
        let alarm = an_alarm();
        let json = serde_json::to_string(&alarm).expect("serialize");
        assert_eq!(
            serde_json::from_str::<Alarm>(&json).expect("deserialize"),
            alarm
        );
    }

    /// Oarfish classifies; it does not narrate. The display line is the masked
    /// template, so there is nowhere for a generated summary to live.
    #[test]
    fn the_display_line_is_the_masked_template() {
        assert_eq!(an_alarm().template, "EXT4-fs error (device <VAR:DEV>)");
    }

    #[test]
    fn changes_round_trip() {
        let alarm = an_alarm();
        for change in [
            AlarmChange::Raised(alarm.clone()),
            AlarmChange::Updated(alarm.clone()),
            AlarmChange::Cleared(alarm.id),
            AlarmChange::Resync,
        ] {
            let json = serde_json::to_string(&change).expect("serialize");
            assert_eq!(
                serde_json::from_str::<AlarmChange>(&json).expect("deserialize"),
                change
            );
        }
    }
}
