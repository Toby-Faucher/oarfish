//! Domain types shared by every stage of the pipeline.
//!
//! Depends on nothing else in the workspace, by design: every other crate
//! depends on this one, so a cycle here would be a cycle everywhere.
//!
//! Owns `TemplateId`, `Severity`, `Slot`, `AlarmId` and `Alarm`, and exports
//! them to the board with `ts-rs` so the two never drift. Nothing here does
//! I/O.
//!
//! `Event` arrives with `oarfish-ingest`, and `Verdict` with `oarfish-jev`:
//! both are shaped by their producers, so they are defined alongside them
//! rather than guessed at here.

#![forbid(unsafe_code)]

mod severity;

pub use severity::Severity;

mod template;

pub use template::{Slot, TemplateId, TemplateIdError};

mod alarm;

pub use alarm::{Alarm, AlarmId};

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
