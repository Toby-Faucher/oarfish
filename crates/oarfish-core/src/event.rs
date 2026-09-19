//! One normalized log line, in the shape every listener produces.
//!
//! `Event` lives here rather than beside its producers because it is the
//! contract between them: syslog, OTLP and the journal all normalize into
//! this, and the pipeline reads nothing else. Source-specific fields survive
//! in `attrs` rather than being flattened away.
//!
//! Two timestamps, and windows use ours. `received_at` is oarfish's clock;
//! `timestamp` is what the source claimed. Cheap hardware has bad clocks, and
//! a switch reporting 1970 must not be able to distort a five-minute window.

use std::borrow::Cow;
use std::collections::BTreeMap;

use bytes::Bytes;
use serde::{Deserialize, Serialize};
use time::OffsetDateTime;
use ts_rs::TS;

/// Where a line came from. Three listeners, no registry: these are not
/// pluggable, so this is a closed enum and not a trait.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, TS)]
#[serde(rename_all = "lowercase")]
#[ts(export, export_to = "oarfish.ts")]
pub enum Source {
    Syslog,
    Journal,
    Otlp,
}

/// One line, normalized. The raw bytes are kept verbatim: at 3am the operator
/// wants the bytes that arrived, not our interpretation of them.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "oarfish.ts")]
pub struct Event {
    /// The bytes that arrived, unchanged. [`Event::raw_lossy`] converts at the
    /// mask boundary; the board cannot render invalid UTF-8, so the `ts-rs`
    /// export declares `string` through that same lossy conversion.
    #[serde(with = "raw_serde")]
    #[ts(type = "string")]
    pub raw: Bytes,
    /// Oarfish's clock, at intake. What windows order on.
    #[serde(with = "time::serde::rfc3339")]
    #[ts(type = "string")]
    pub received_at: OffsetDateTime,
    /// What the source claimed. `None` when it claimed nothing.
    #[serde(with = "time::serde::rfc3339::option")]
    #[ts(type = "string | null")]
    pub timestamp: Option<OffsetDateTime>,
    /// The sending host, or the socket peer when the frame names none.
    pub host: String,
    pub source: Source,
    /// Everything source-specific: syslog severity and facility, the journal's
    /// fields, OTLP attributes. `BTreeMap` for deterministic snapshots.
    pub attrs: BTreeMap<String, String>,
}

impl Event {
    /// A new event, received now, with no source timestamp and no attributes.
    /// Listeners fill in what their transport actually knows.
    pub fn new(raw: Bytes, host: impl Into<String>, source: Source) -> Self {
        Self {
            raw,
            received_at: OffsetDateTime::now_utc(),
            timestamp: None,
            host: host.into(),
            source,
            attrs: BTreeMap::new(),
        }
    }

    /// The raw line as text, converting invalid UTF-8 to the replacement
    /// character. Called at the mask boundary: masking works on `&str`, and
    /// this is the one place the conversion is unavoidable.
    pub fn raw_lossy(&self) -> Cow<'_, str> {
        String::from_utf8_lossy(&self.raw)
    }
}

/// `Bytes` on the wire. Human-readable formats (JSON, and through it the
/// board, which cannot render invalid UTF-8) get a lossy string; binary
/// formats (`postcard`, for M4's fjall store) get the exact bytes. Gating on
/// `is_human_readable` is what keeps invariant 3 through the store: without
/// it every persisted raw line would carry `U+FFFD` instead of the bytes that
/// arrived.
mod raw_serde {
    use bytes::Bytes;
    use serde::{Deserialize, Deserializer, Serialize, Serializer};

    pub fn serialize<S: Serializer>(raw: &Bytes, serializer: S) -> Result<S::Ok, S::Error> {
        if serializer.is_human_readable() {
            serializer.serialize_str(&String::from_utf8_lossy(raw))
        } else {
            raw.to_vec().serialize(serializer)
        }
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Bytes, D::Error> {
        if deserializer.is_human_readable() {
            String::deserialize(deserializer).map(|s| Bytes::from(s.into_bytes()))
        } else {
            Vec::<u8>::deserialize(deserializer).map(Bytes::from)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn an_event() -> Event {
        Event {
            raw: Bytes::from_static(b"sshd[1]: Failed password for root"),
            received_at: OffsetDateTime::UNIX_EPOCH,
            timestamp: Some(OffsetDateTime::UNIX_EPOCH),
            host: "web01".to_owned(),
            source: Source::Syslog,
            attrs: BTreeMap::from([("severity".to_owned(), "err".to_owned())]),
        }
    }

    #[test]
    fn an_event_round_trips() {
        let event = an_event();
        let json = serde_json::to_string(&event).expect("serialize");
        assert_eq!(
            serde_json::from_str::<Event>(&json).expect("deserialize"),
            event
        );
    }

    #[test]
    fn timestamps_cross_the_wire_as_rfc_3339() {
        let json = serde_json::to_value(an_event()).expect("serialize");
        assert_eq!(
            json["received_at"],
            serde_json::json!("1970-01-01T00:00:00Z")
        );
        assert_eq!(json["timestamp"], serde_json::json!("1970-01-01T00:00:00Z"));
    }

    #[test]
    fn a_missing_source_timestamp_is_null() {
        let mut event = an_event();
        event.timestamp = None;
        let json = serde_json::to_value(&event).expect("serialize");
        assert_eq!(json["timestamp"], serde_json::Value::Null);
    }

    /// The board cannot render invalid UTF-8, so the JSON boundary converts
    /// lossily. The in-process `raw` is untouched; only the serialized form
    /// carries the replacement character.
    #[test]
    fn invalid_utf8_leaves_as_lossy_but_stays_byte_exact_in_process() {
        let raw = Bytes::from(vec![b's', b's', b'h', 0xff, b'd']);
        let event = Event::new(raw.clone(), "web01", Source::Syslog);
        assert_eq!(&event.raw, &raw);
        assert_eq!(event.raw_lossy(), "ssh\u{fffd}d");

        let json = serde_json::to_value(&event).expect("serialize");
        assert_eq!(json["raw"], serde_json::json!("ssh\u{fffd}d"));
    }

    /// The store boundary must not share JSON's lossiness: under `postcard`
    /// the adapter carries the exact bytes. Pinned at the adapter level with
    /// bytes JSON cannot represent — full-Event postcard coverage is M4's to
    /// define, since it does not persist Events today.
    #[test]
    fn postcard_preserves_non_utf8_bytes_exactly() {
        use serde::{Deserialize, Serialize};

        #[derive(Debug, PartialEq, Serialize, Deserialize)]
        struct Raw(#[serde(with = "super::raw_serde")] Bytes);

        let raw = Raw(Bytes::from(vec![b's', b's', b'h', 0xff, b'd', 0x00, b'\n']));
        let encoded = postcard::to_stdvec(&raw).expect("encode");
        let back: Raw = postcard::from_bytes(&encoded).expect("decode");
        assert_eq!(back, raw);
    }

    #[test]
    fn sources_serialize_as_lowercase_strings() {
        for (source, json) in [
            (Source::Syslog, r#""syslog""#),
            (Source::Journal, r#""journal""#),
            (Source::Otlp, r#""otlp""#),
        ] {
            assert_eq!(serde_json::to_string(&source).expect("serialize"), json);
            assert_eq!(
                serde_json::from_str::<Source>(json).expect("deserialize"),
                source
            );
        }
    }
}
