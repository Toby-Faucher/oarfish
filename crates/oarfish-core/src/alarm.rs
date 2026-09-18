//! An open alarm: the thing the whole daemon exists to avoid raising.

use std::fmt;

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
}

impl fmt::Display for AlarmId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
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
}
