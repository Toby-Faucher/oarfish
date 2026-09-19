//! Domain types shared by every stage of the pipeline.
//!
//! Depends on nothing else in the workspace, by design: every other crate
//! depends on this one, so a cycle here would be a cycle everywhere.
//!
//! Owns `TemplateId`, `Severity`, `Slot`, `AlarmId`, `Alarm`, `Source` and
//! `Event`, and exports them to the board with `ts-rs` so the two never
//! drift. Nothing here does I/O.
//!
//! `Event` lives here — not beside its producers — because it is the contract
//! between them: syslog, OTLP and the journal all normalize into it, and the
//! pipeline reads nothing else. `Verdict` arrives with `oarfish-jev` and is
//! shaped by its producer, so it is defined alongside it rather than guessed
//! at here.

#![forbid(unsafe_code)]

mod severity;

pub use severity::Severity;

mod template;

pub use template::{Slot, TemplateId, TemplateIdError};

mod alarm;

pub use alarm::{Alarm, AlarmId};

mod event;

pub use event::{Event, Source};

#[cfg(test)]
mod tests {
    use ulid::Ulid;

    /// Alarm ids are ULIDs specifically so the store gets chronological range
    /// scans for free. If this ever stops holding, the key layout is wrong.
    #[test]
    fn ulids_sort_chronologically() {
        let first = Ulid::generate();
        std::thread::sleep(std::time::Duration::from_millis(2));
        let second = Ulid::generate();

        assert!(first < second);
        assert!(first.to_string() < second.to_string());
    }

    #[test]
    fn ulids_round_trip_through_serde() {
        let id = Ulid::generate();
        let json = serde_json::to_string(&id).expect("serialize");
        let back: Ulid = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(id, back);
    }
}
