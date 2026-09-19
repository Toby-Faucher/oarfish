//! The live open-alarm set, shared read-only with the board.
//!
//! The engine task is the only writer; serving `GET /api/alarms` from this
//! instead of the store keeps the board on the same record the state machine
//! holds. The lock lives behind this interface so neither side names the
//! concurrency primitive: the engine cannot be broken by a board read, and
//! the board cannot be broken by the engine changing its internals.
//!
//! A poisoned lock serves the last-known state, never a silent empty list:
//! an empty list with a 200 is indistinguishable from a quiet night. Lock
//! operations never panic, so poison is a backstop, not a path.

use std::collections::HashMap;
use std::sync::{Arc, RwLock};

use crate::{Alarm, AlarmId};

/// The live open-alarm set. Clone shares the set; all methods take `&self`.
#[derive(Debug, Clone, Default)]
pub struct Snapshot(Arc<RwLock<HashMap<AlarmId, Alarm>>>);

impl Snapshot {
    /// An empty set. The engine builds one; readers clone its handle.
    pub fn new() -> Self {
        Self::default()
    }

    /// Open alarms, oldest first. What `GET /api/alarms` renders and what a
    /// lagged stream resyncs from.
    pub fn snapshot(&self) -> Vec<Alarm> {
        let alarms: Vec<Alarm> = match self.0.read() {
            Ok(map) => map.values().cloned().collect(),
            Err(poisoned) => poisoned.into_inner().values().cloned().collect(),
        };
        let mut alarms = alarms;
        alarms.sort_by_key(|alarm| alarm.opened_at);
        alarms
    }

    /// Mirror one open alarm into the set. The engine calls this on raise
    /// and on every dedupe bump.
    pub fn insert(&self, alarm: Alarm) {
        if let Ok(mut map) = self.0.write() {
            map.insert(alarm.id, alarm);
        }
    }

    /// Drop one open alarm from the set. Called on clear, paired with every
    /// insert.
    pub fn remove(&self, id: &AlarmId) {
        if let Ok(mut map) = self.0.write() {
            map.remove(id);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn alarm() -> Alarm {
        Alarm {
            id: AlarmId::generate(),
            template_id: crate::TemplateId::of("task <VAR:NUM> failed"),
            template: "task <VAR:NUM> failed".to_owned(),
            severity: crate::Severity::Minor,
            host: "web01".to_owned(),
            count: 1,
            opened_at: time::OffsetDateTime::UNIX_EPOCH,
        }
    }

    #[test]
    fn insert_snapshot_remove_round_trip() {
        let snapshot = Snapshot::new();
        assert!(snapshot.snapshot().is_empty());
        let alarm = alarm();
        let id = alarm.id;
        snapshot.insert(alarm);
        assert_eq!(snapshot.snapshot().len(), 1);
        snapshot.remove(&id);
        assert!(snapshot.snapshot().is_empty());
    }

    #[test]
    fn clones_share_the_set() {
        let snapshot = Snapshot::new();
        let reader = snapshot.clone();
        snapshot.insert(alarm());
        assert_eq!(reader.snapshot().len(), 1);
    }
}
