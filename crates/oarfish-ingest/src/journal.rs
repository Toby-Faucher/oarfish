//! systemd journal reading, behind the `journald` feature.
//!
//! The `systemd` crate is FFI to libsystemd, which breaks static builds and
//! will not compile on macOS or musl — so everything touching it lives behind
//! `journald`, on by default for Linux packages and off for portable builds.
//! What is *not* gated is the pure `record -> Event` conversion below it: FFI
//! is quarantined there, and the mapping is tested with no libsystemd.
//!
//! The reader tails the journal: no cursor is persisted (that is M4 storage
//! work, in `oarfish-store`), so a restart re-reads nothing and replays
//! nothing. And it filters its own `_SYSTEMD_UNIT` by default, because the
//! daemon's own `tracing` output goes to the journal and would otherwise be
//! read straight back in — a log line per log line.

use std::collections::BTreeMap;

use bytes::Bytes;
use oarfish_core::{Event, Source};
use time::OffsetDateTime;

#[cfg(feature = "journald")]
use crate::IngestError;

/// The unit filtered by default. The daemon ships as `oarfish.service`; its
/// own writes would otherwise loop back through the reader.
pub const DEFAULT_EXCLUDE_UNIT: &str = "oarfish.service";

/// Turn one journal record into an `Event`. Pure: no FFI, no clock, same
/// record in, same event out.
///
/// `MESSAGE` becomes the raw bytes verbatim. The host is `_HOSTNAME`, falling
/// back to `localhost` — the journal is local. The source timestamp is
/// `_SOURCE_REALTIME_TIMESTAMP`, falling back to `__REALTIME_TIMESTAMP`;
/// both are microseconds since the epoch, and a missing or unparsable one
/// means unknown, not 1970. Every other field survives in `attrs` rather than
/// being flattened away.
pub fn record_to_event(record: &BTreeMap<String, String>, received_at: OffsetDateTime) -> Event {
    let mut attrs = BTreeMap::new();
    for (key, value) in record {
        if key != "MESSAGE" {
            attrs.insert(key.clone(), value.clone());
        }
    }
    Event {
        raw: record
            .get("MESSAGE")
            .map(|message| Bytes::copy_from_slice(message.as_bytes()))
            .unwrap_or_default(),
        received_at,
        timestamp: record
            .get("_SOURCE_REALTIME_TIMESTAMP")
            .or_else(|| record.get("__REALTIME_TIMESTAMP"))
            .and_then(|micros| micros.parse::<u64>().ok())
            .and_then(|micros| micros.checked_mul(1_000))
            .and_then(|nanos| OffsetDateTime::from_unix_timestamp_nanos(nanos as i128).ok()),
        host: record
            .get("_HOSTNAME")
            .filter(|name| !name.is_empty())
            .cloned()
            .unwrap_or_else(|| "localhost".to_owned()),
        source: Source::Journal,
        attrs,
    }
}

/// The journal reader. Opened in [`JournalReader::open`], served in
/// [`JournalReader::run_blocking`]: the same two-phase lifecycle as the other
/// listeners, except the open is synchronous FFI rather than a socket bind.
///
/// The handle is `!Send`: open and read on one thread, never move it. The
/// daemon opens it on a dedicated OS thread for exactly this reason.
#[cfg(feature = "journald")]
pub struct JournalReader {
    journal: systemd::journal::Journal,
    exclude_unit: Option<String>,
}

#[cfg(feature = "journald")]
impl JournalReader {
    /// Open the journal at its tail, excluding [`DEFAULT_EXCLUDE_UNIT`].
    /// Fails with [`IngestError::JournalOpen`] when libsystemd refuses — a
    /// startup error, not a per-line one.
    pub fn open() -> Result<Self, IngestError> {
        Self::open_excluding(Some(DEFAULT_EXCLUDE_UNIT.to_owned()))
    }

    /// Open the journal at its tail, excluding `exclude_unit`. `None` reads
    /// everything, including the daemon's own writes — only useful in tests.
    pub fn open_excluding(exclude_unit: Option<String>) -> Result<Self, IngestError> {
        let mut journal = systemd::journal::OpenOptions::default()
            .open()
            .map_err(|e| IngestError::JournalOpen {
                reason: e.to_string(),
            })?;
        journal
            .seek(systemd::journal::JournalSeek::Tail)
            .map_err(|e| IngestError::JournalOpen {
                reason: e.to_string(),
            })?;
        Ok(Self {
            journal,
            exclude_unit,
        })
    }

    /// Tail the journal on the calling thread until `cancel` fires. Drops its
    /// `Sender` on return.
    ///
    /// Blocking, and `Journal` is `!Send`: call this from a dedicated OS
    /// thread (as the daemon does), never on a runtime worker and never
    /// inside a spawned future, which would poison it with `!Send`.
    pub fn run_blocking(
        self,
        tx: tokio::sync::mpsc::Sender<Event>,
        cancel: tokio_util::sync::CancellationToken,
    ) {
        let Self {
            mut journal,
            exclude_unit,
        } = self;
        loop {
            match journal.await_next_entry(Some(std::time::Duration::from_millis(250))) {
                Ok(Some(record)) => {
                    let own_unit = exclude_unit.as_deref().and_then(|exclude| {
                        record.get("_SYSTEMD_UNIT").filter(|unit| *unit == exclude)
                    });
                    if own_unit.is_some() {
                        continue;
                    }
                    let event = record_to_event(&record, OffsetDateTime::now_utc());
                    // Blocking send: this is a blocking context by
                    // construction, and there is no backpressure transport to
                    // honour — the cursor simply waits with us.
                    if tx.blocking_send(event).is_err() {
                        break;
                    }
                }
                Ok(None) => {
                    if cancel.is_cancelled() {
                        break;
                    }
                }
                Err(e) => {
                    tracing::debug!(error = %e, "journal read failed");
                    if cancel.is_cancelled() {
                        break;
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn received_at() -> OffsetDateTime {
        OffsetDateTime::from_unix_timestamp(1_789_812_000).expect("fixed test time")
    }

    fn a_record() -> BTreeMap<String, String> {
        BTreeMap::from([
            (
                "MESSAGE".to_owned(),
                "sshd[1]: Failed password for root".to_owned(),
            ),
            ("_HOSTNAME".to_owned(), "web01".to_owned()),
            ("PRIORITY".to_owned(), "3".to_owned()),
            ("SYSLOG_IDENTIFIER".to_owned(), "sshd".to_owned()),
            ("_SYSTEMD_UNIT".to_owned(), "sshd.service".to_owned()),
            (
                "_SOURCE_REALTIME_TIMESTAMP".to_owned(),
                "1789812000000000".to_owned(),
            ),
        ])
    }

    #[test]
    fn a_journal_record_becomes_an_event() {
        let event = record_to_event(&a_record(), received_at());
        insta::assert_json_snapshot!(event, @r###"
        {
          "raw": "sshd[1]: Failed password for root",
          "received_at": "2026-09-19T10:00:00Z",
          "timestamp": "2026-09-19T10:00:00Z",
          "host": "web01",
          "source": "journal",
          "attrs": {
            "PRIORITY": "3",
            "SYSLOG_IDENTIFIER": "sshd",
            "_HOSTNAME": "web01",
            "_SOURCE_REALTIME_TIMESTAMP": "1789812000000000",
            "_SYSTEMD_UNIT": "sshd.service"
          }
        }
        "###);
    }

    #[test]
    fn a_missing_hostname_falls_back_to_localhost() {
        let mut record = a_record();
        record.remove("_HOSTNAME");
        assert_eq!(record_to_event(&record, received_at()).host, "localhost");
    }

    #[test]
    fn a_missing_message_is_still_an_event_with_empty_raw() {
        let mut record = a_record();
        record.remove("MESSAGE");
        let event = record_to_event(&record, received_at());
        assert!(event.raw.is_empty());
    }

    #[test]
    fn a_bad_timestamp_means_unknown_not_1970() {
        let mut record = a_record();
        record.insert(
            "_SOURCE_REALTIME_TIMESTAMP".to_owned(),
            "not-a-time".to_owned(),
        );
        record.remove("__REALTIME_TIMESTAMP");
        assert_eq!(record_to_event(&record, received_at()).timestamp, None);
    }
}
