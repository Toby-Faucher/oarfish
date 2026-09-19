//! Open alarms, persisted. The engine's only-writer state, durable.
//!
//! The engine calls this on raise and on every dedupe bump, so what the
//! board reads and what a restart reloads are the same record. A write
//! failure surfaces: unlike a verdict read, losing an open alarm is a silent
//! failure, never a safe lane. Unreadable entries are skipped with a warning
//! rather than failing the boot.
//!
//! This module knows nothing about verdicts, questions or judging: alarm
//! persistence evolves and tests without dragging the cache or the judge.

use oarfish_core::{Alarm, AlarmId};

use crate::alarms::{alarm_key, decode_alarm, encode_alarm};

/// The persisted open-alarm set, over one `fjall` keyspace.
pub struct AlarmStore {
    alarms: fjall::Keyspace,
}

impl AlarmStore {
    /// Built by the composition root over the alarms keyspace.
    pub(crate) fn new(alarms: fjall::Keyspace) -> Self {
        Self { alarms }
    }

    /// Save one open alarm.
    pub fn save(&self, alarm: &Alarm) -> Result<(), fjall::Error> {
        let bytes = encode_alarm(alarm).expect("an Alarm in memory always encodes");
        self.alarms.insert(alarm_key(&alarm.id), bytes)?;
        Ok(())
    }

    /// Delete one open alarm. Called on clear, paired with every save.
    pub fn remove(&self, id: &AlarmId) -> Result<(), fjall::Error> {
        self.alarms.remove(alarm_key(id))?;
        Ok(())
    }

    /// Every open alarm, oldest first. The engine reloads these on boot and
    /// re-arms their timers.
    pub fn load_open(&self) -> Vec<Alarm> {
        let mut out = Vec::new();
        for guard in self.alarms.range([0u8; 16]..=[0xFF; 16]) {
            let bytes = match guard.value() {
                Ok(bytes) => bytes,
                Err(error) => {
                    tracing::warn!(%error, "alarm scan hit an unreadable entry");
                    continue;
                }
            };
            match decode_alarm(bytes.as_ref()) {
                Ok(alarm) => out.push(alarm),
                Err(error) => {
                    tracing::warn!(%error, "stored alarm would not decode; skipping");
                }
            }
        }
        out.sort_by_key(|alarm| alarm.opened_at);
        out
    }
}
