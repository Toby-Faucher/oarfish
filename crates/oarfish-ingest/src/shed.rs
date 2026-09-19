//! Counted shedding, shared by the listeners that cannot apply backpressure.
//!
//! A datagram socket has no window to close: when the channel is full the only
//! honest options are counting the loss or hiding it. This type counts, and
//! damps the warning itself through `governor` — a 100k lines/sec flood must
//! produce one warn per interval carrying an accumulated count, not 100k
//! warns. The log about the firehose must not become the firehose.
//!
//! Shedding is never silent: every dropped datagram increments a per-source
//! counter that is logged and, from M5, is available as a signal in its own
//! right.

use std::num::NonZeroU32;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use governor::{
    Quota, RateLimiter, clock::DefaultClock, middleware::NoOpMiddleware, state::InMemoryState,
    state::direct::NotKeyed,
};

type WarnLimiter = RateLimiter<NotKeyed, InMemoryState, DefaultClock, NoOpMiddleware>;

/// Bookkeeping for one shedding source, e.g. `"syslog-udp"`. Cheap to clone:
/// the counter and the limiter are shared, so a test can hold a handle while
/// the listener runs.
#[derive(Debug, Clone)]
pub struct ShedTracker {
    source: &'static str,
    dropped: Arc<AtomicU64>,
    warn_limiter: Arc<WarnLimiter>,
}

impl ShedTracker {
    /// One warn per `warn_interval` at most, carrying the accumulated count.
    pub fn new(source: &'static str) -> Self {
        Self::with_warn_interval(source, Duration::from_secs(5))
    }

    /// A custom warn interval. The production default is [`ShedTracker::new`];
    /// this exists so tests do not wait five seconds to see a second warn.
    pub fn with_warn_interval(source: &'static str, warn_interval: Duration) -> Self {
        let quota = Quota::with_period(warn_interval).expect("a positive warn interval");
        Self {
            source,
            dropped: Arc::new(AtomicU64::new(0)),
            warn_limiter: Arc::new(WarnLimiter::direct(quota)),
        }
    }

    /// Record one shed event: the channel was full and this transport has no
    /// backpressure to apply. Increments the counter always; emits the warn
    /// at most once per interval, with the total so far.
    pub fn note_dropped(&self) {
        self.note("the channel is full and this transport has no backpressure");
    }

    /// Record one shed event: the datagram arrived over the UDP intake quota.
    /// Same counting and damped warn as [`ShedTracker::note_dropped`], but the
    /// warn names this cause — a lab flooding an idle pipeline must not be
    /// sent to debug the channel.
    pub fn note_rate_limited(&self) {
        self.note("over the UDP intake quota on a non-full channel");
    }

    fn note(&self, cause: &str) {
        let total = self.dropped.fetch_add(1, Ordering::Relaxed) + 1;
        if self.warn_limiter.check().is_ok() {
            tracing::warn!(
                source = self.source,
                dropped_total = total,
                cause = cause,
                "shedding intake"
            );
        }
    }

    /// How many events this source has shed so far.
    pub fn dropped(&self) -> u64 {
        self.dropped.load(Ordering::Relaxed)
    }

    /// Intake quota for UDP: generous enough that a healthy lab never notices
    /// it, tight enough that a runaway container cannot wedge the pipeline.
    /// 50k lines/sec with a matching burst, per listener.
    pub fn udp_quota() -> Quota {
        Quota::per_second(NonZeroU32::new(50_000).expect("non-zero"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn drops_are_counted_even_when_the_warn_is_damped() {
        let tracker = ShedTracker::with_warn_interval("test", Duration::from_secs(3600));
        for _ in 0..10 {
            tracker.note_dropped();
        }
        assert_eq!(tracker.dropped(), 10);
    }

    #[test]
    fn clones_share_the_same_counter() {
        let tracker = ShedTracker::new("test");
        let other = tracker.clone();
        other.note_dropped();
        assert_eq!(tracker.dropped(), 1);
    }

    #[test]
    fn both_causes_share_the_counter() {
        let tracker = ShedTracker::new("test");
        tracker.note_dropped();
        tracker.note_rate_limited();
        assert_eq!(tracker.dropped(), 2);
    }
}
