//! Open alarms, persisted so a restart cannot lose one.
//!
//! The engine is the only writer of open-alarm state: it saves on raise and
//! on every dedupe bump, deletes on clear, and reloads on boot to re-arm its
//! timers. Windows are deliberately not persisted — they rebuild from live
//! traffic in minutes — so the asymmetry is explicit: a restart may delay an
//! auto-clear by up to the silence interval, but it never drops an open
//! alarm. A delayed clear is visible and self-correcting; a lost alarm is a
//! silent failure.
//!
//! Values are `postcard` [`Alarm`]s keyed by the 16-byte [`AlarmId`] form,
//! which sorts chronologically for free.

use oarfish_core::{Alarm, AlarmId};

/// The `alarms` keyspace: [`alarm_key`] → `postcard` [`Alarm`].
pub const ALARMS_KEYSPACE: &str = "alarms";

/// The 16-byte chronological key for one open alarm.
pub fn alarm_key(id: &AlarmId) -> [u8; 16] {
    id.to_bytes()
}

/// Encode one open alarm for the store.
pub fn encode_alarm(alarm: &Alarm) -> Result<Vec<u8>, postcard::Error> {
    postcard::to_stdvec(alarm)
}

/// Decode one open alarm from the store.
pub fn decode_alarm(bytes: &[u8]) -> Result<Alarm, postcard::Error> {
    postcard::from_bytes(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;
    use oarfish_core::{Severity, TemplateId};
    use time::OffsetDateTime;

    fn an_alarm() -> Alarm {
        Alarm {
            id: AlarmId::generate(),
            template_id: TemplateId::of("EXT4-fs error (device <VAR:DEV>)"),
            template: "EXT4-fs error (device <VAR:DEV>)".to_owned(),
            exemplar: "EXT4-fs error (device sda)".to_owned(),
            severity: Severity::Critical,
            host: "nas01".to_owned(),
            lane: oarfish_core::Lane::Dashboard,
            count: 14,
            opened_at: OffsetDateTime::UNIX_EPOCH,
        }
    }

    #[test]
    fn an_alarm_round_trips_through_postcard() {
        let alarm = an_alarm();
        let back = decode_alarm(&encode_alarm(&alarm).expect("encode")).expect("decode");
        assert_eq!(back, alarm);
    }

    #[test]
    fn alarm_keys_sort_chronologically() {
        let first = AlarmId::generate();
        std::thread::sleep(std::time::Duration::from_millis(2));
        let second = AlarmId::generate();
        assert!(alarm_key(&first) < alarm_key(&second));
    }
}
