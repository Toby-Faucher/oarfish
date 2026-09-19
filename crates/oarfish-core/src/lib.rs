//! Domain types shared by every stage of the pipeline.
//!
//! Depends on nothing else in the workspace, by design: every other crate
//! depends on this one, so a cycle here would be a cycle everywhere.
//!
//! Owns `TemplateId`, `Severity`, `Slot`, `AlarmId`, `Alarm`, `Source`,
//! `Event`, `Verdict`, `VerdictAnswer` and `QuestionsHash`, and exports them
//! to the board with `ts-rs` so the two never drift. Nothing here does I/O.
//!
//! `Event` lives here — not beside its producers — because it is the contract
//! between them: syslog, OTLP and the journal all normalize into it, and the
//! pipeline reads nothing else. `Verdict` lives here for the same reason
//! `Event` does: it is the contract between the decision layer and everything
//! downstream, and the store, the engine and the board must share one
//! definition rather than each guessing at the producer's shape.

#![forbid(unsafe_code)]

mod severity;

pub use severity::Severity;

mod template;

pub use template::{Slot, TemplateId, TemplateIdError};

mod verdict;

pub use verdict::{QuestionsHash, QuestionsHashError, Verdict, VerdictAnswer};

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
