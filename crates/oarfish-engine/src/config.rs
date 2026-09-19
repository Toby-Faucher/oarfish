//! Tuning for the engine. Every value is a starting point to be tuned
//! against real data in the spirit of §5.9, written down so the tests have
//! something concrete to assert rather than defended as principles.

use std::time::Duration;

use oarfish_core::Severity;

/// Tuning for [`crate::Engine`]. Defaults are the documented constants.
#[derive(Debug, Clone)]
pub struct EngineConfig {
    /// A template may alarm only if judged actionable at or above this
    /// severity. Below it the verdict gates and nothing raises.
    pub gate_floor: Severity,
    /// A template judged at or above this severity raises on first sight,
    /// without waiting for the window. An EXT4 error must not wait for a
    /// second occurrence to be believed.
    pub bypass_severity: Severity,
    /// The `actionable` noul value at or above which a template counts as
    /// actionable. A coin flip is actionable; below it the template is
    /// treated as noise.
    pub actionable_threshold: f64,
    /// The `contextual` noul value at or above which a template is flagged:
    /// its bursts wait for the contextual check instead of raising outright.
    /// A coin flip is flagged; below it the template settles on the static
    /// verdict alone.
    pub contextual_threshold: f64,
    /// The `matters_now` noul value at or above which a checked burst is
    /// worth surfacing. Below it the burst resolves to `Lane::Record` and
    /// never surfaces — nothing that has surfaced is ever withdrawn, so the
    /// raise is what waits.
    pub matters_now_threshold: f64,
    /// How long a contextual check may take before the engine stops waiting
    /// and raises at `Dashboard`. A check that cannot answer must not be able
    /// to suppress an alarm the window already decided was worth raising.
    pub check_timeout: Duration,
    /// Open alarms carried into one contextual check's state, host-scoped.
    /// Bounds the token budget; oldest first.
    pub max_context_alarms: usize,
    /// `mpsc` capacity for returning contextual-check answers. Checks run
    /// per burst on flagged templates only, so the queue is small; a lagging
    /// engine still drains, never replays.
    pub pending_capacity: usize,
    /// Events in the five-minute window that raise a gated, non-bypassed
    /// template on count alone.
    pub window_count_threshold: u64,
    /// The five-minute rate, as a multiple of the trailing-hour rate, that
    /// raises a gated, non-bypassed template.
    pub rate_multiple: f64,
    /// Silence after the last event before an open alarm auto-clears.
    pub silence: Duration,
    /// After a clear, a re-raise inside this cooldown updates the alarm that
    /// just closed rather than opening a new one.
    pub flap_cooldown: Duration,
    /// `broadcast` channel capacity for [`crate::AlarmChange`]. A lagging SSE
    /// receiver resyncs from `/api/alarms` rather than replaying.
    pub broadcast_capacity: usize,
    /// Window-table bound, sized with the Drain table: templates a homelab
    /// has. Idle eviction is by time; this caps a runaway minter of new
    /// templates.
    pub max_windows: usize,
}

/// Five events in the five-minute window raise on count.
pub const DEFAULT_WINDOW_COUNT_THRESHOLD: u64 = 5;
/// Three times the trailing-hour rate raises on rate.
pub const DEFAULT_RATE_MULTIPLE: f64 = 3.0;
/// Fifteen minutes of silence auto-clears.
pub const DEFAULT_SILENCE_SECS: u64 = 900;
/// Thirty minutes of flap cooldown after a clear.
pub const DEFAULT_FLAP_COOLDOWN_SECS: u64 = 1800;
/// Bounded on purpose: a lagging receiver resyncs, never replays.
pub const DEFAULT_BROADCAST_CAPACITY: usize = 256;
/// Sized with the Drain table and the verdict cache: one fewer thing to tune.
pub const DEFAULT_MAX_WINDOWS: usize = 65_536;
/// A coin flip is flagged: the template alone cannot settle whether its
/// bursts matter.
pub const DEFAULT_CONTEXTUAL_THRESHOLD: f64 = 0.5;
/// A coin flip matters: the check already narrowed the field to flagged
/// templates over threshold, so the bar here is confirmation, not proof.
pub const DEFAULT_MATTERS_NOW_THRESHOLD: f64 = 0.5;
/// Ten seconds: past several Jev round trips, before the engine raises
/// without the answer.
pub const DEFAULT_CHECK_TIMEOUT_SECS: u64 = 10;
/// Eight open alarms fit the token budget beside window stats and questions.
pub const DEFAULT_MAX_CONTEXT_ALARMS: usize = 8;
/// Pending check outcomes buffer this many answers. Checks run per burst on
/// flagged templates only — dozens a day, not hundreds a second.
pub const DEFAULT_PENDING_CAPACITY: usize = 256;

impl Default for EngineConfig {
    fn default() -> Self {
        Self {
            gate_floor: Severity::Minor,
            bypass_severity: Severity::Critical,
            actionable_threshold: 0.5,
            contextual_threshold: DEFAULT_CONTEXTUAL_THRESHOLD,
            matters_now_threshold: DEFAULT_MATTERS_NOW_THRESHOLD,
            check_timeout: Duration::from_secs(DEFAULT_CHECK_TIMEOUT_SECS),
            max_context_alarms: DEFAULT_MAX_CONTEXT_ALARMS,
            window_count_threshold: DEFAULT_WINDOW_COUNT_THRESHOLD,
            rate_multiple: DEFAULT_RATE_MULTIPLE,
            silence: Duration::from_secs(DEFAULT_SILENCE_SECS),
            flap_cooldown: Duration::from_secs(DEFAULT_FLAP_COOLDOWN_SECS),
            broadcast_capacity: DEFAULT_BROADCAST_CAPACITY,
            pending_capacity: DEFAULT_PENDING_CAPACITY,
            max_windows: DEFAULT_MAX_WINDOWS,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_match_the_documented_starting_values() {
        let config = EngineConfig::default();
        assert_eq!(config.gate_floor, Severity::Minor);
        assert_eq!(config.bypass_severity, Severity::Critical);
        assert_eq!(config.actionable_threshold, 0.5);
        assert_eq!(config.contextual_threshold, 0.5);
        assert_eq!(config.matters_now_threshold, 0.5);
        assert_eq!(config.check_timeout, Duration::from_secs(10));
        assert_eq!(config.max_context_alarms, 8);
        assert_eq!(config.window_count_threshold, 5);
        assert_eq!(config.rate_multiple, 3.0);
        assert_eq!(config.silence, Duration::from_secs(900));
        assert_eq!(config.flap_cooldown, Duration::from_secs(1800));
        assert_eq!(config.pending_capacity, 256);
    }
}
