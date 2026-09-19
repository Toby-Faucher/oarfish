//! Bucketed per-template windows: §5.7's count, rate, distinct hosts and
//! first-seen, in memory.
//!
//! The obvious implementation — a `VecDeque` of timestamps — is unbounded per
//! template, which fails at the 100k lines/sec case the codebase calls a
//! normal homelab failure. Bucketed counters instead: the five-minute window
//! is 30 buckets of 10 seconds, the trailing hour 60 buckets of a minute.
//! Per line the cost is one index and one increment — O(1) — with memory
//! fixed per template rather than proportional to traffic.
//!
//! Distinct hosts are a host-to-last-seen map per template, bounded by fleet
//! size rather than event count. A template seen on six machines is a
//! different signal from one seen six times on one.
//!
//! The table is bounded, evicting templates idle for over an hour. Drain's
//! table is bounded for the same reason and it applies here with more force:
//! 65_536 templates times 90 buckets is memory nothing would ever reclaim,
//! and a runaway container minting new templates is the case the bound
//! exists for.
//!
//! All clocks are `tokio::time::Instant`, so `pause()`-driven tests advance
//! windows deterministically. Wall time appears only on the alarm record,
//! where it is display, never logic.

use std::collections::HashMap;
use std::time::Duration;

use oarfish_core::TemplateId;
use tokio::time::Instant;

/// Five minutes in 10-second buckets.
const SHORT_BUCKETS: usize = 30;
const SHORT_BUCKET_SECS: i64 = 10;
/// The trailing hour in 1-minute buckets.
const LONG_BUCKETS: usize = 60;
const LONG_BUCKET_SECS: i64 = 60;
/// The short window, in seconds, for host-distinctness, rate comparison
/// and idle eviction.
pub(crate) const SHORT_WINDOW_SECS: u64 = 300;
const SHORT_WINDOW_SECS_I64: i64 = SHORT_WINDOW_SECS as i64;
const IDLE_EVICT_SECS: u64 = 3600;
/// At most one idle sweep per interval: eviction is housekeeping, not exact,
/// and the every-line path must stay O(1) even while a runaway container
/// mints a new template per line.
const IDLE_SWEEP_INTERVAL: Duration = Duration::from_secs(60);

/// One fixed ring of counters. A bucket is identified by its absolute number
/// (`t / width`); a slot holding an older number is stale and zeroes on
/// write, so advancing the clock overwrites the buckets it moved past.
struct Ring {
    counts: Vec<u64>,
    epochs: Vec<i64>,
}

impl Ring {
    fn new(buckets: usize) -> Self {
        Self {
            counts: vec![0; buckets],
            epochs: vec![i64::MIN; buckets],
        }
    }

    fn add(&mut self, bucket: i64) {
        let slot = (bucket.rem_euclid(self.counts.len() as i64)) as usize;
        if self.epochs[slot] != bucket {
            self.epochs[slot] = bucket;
            self.counts[slot] = 0;
        }
        self.counts[slot] += 1;
    }

    /// The total over buckets newer than `span` buckets back from `now`.
    /// Never-written slots carry `i64::MIN`; the saturating subtraction
    /// keeps them out without an overflow.
    fn total(&self, now: i64) -> u64 {
        let span = self.counts.len() as i64;
        self.counts
            .iter()
            .zip(&self.epochs)
            .filter(|(_, epoch)| now.saturating_sub(**epoch) < span)
            .map(|(count, _)| *count)
            .sum()
    }
}

/// One template's window: two rings, host last-seens, and first-seen.
struct TemplateWindow {
    short: Ring,
    long: Ring,
    /// Host → seconds since table start of its last event. Pruned past the
    /// trailing hour on write; bounded by fleet size, not event count.
    hosts: HashMap<String, i64>,
    first_seen: Instant,
    last_seen: Instant,
}

/// What §5.7 asks for, per template: count, rate input, distinct hosts and
/// first-seen.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WindowStats {
    /// Events in the five-minute window.
    pub short_total: u64,
    /// Events in the trailing hour, including the five-minute window.
    pub long_total: u64,
    /// Hosts seen in the five-minute window.
    pub distinct_hosts: usize,
    /// Seconds since table start of the first event. Display-adjacent; the
    /// engine does not route on it.
    pub first_seen_secs: u64,
    /// Seconds of history behind `long_total`, capped at the trailing hour.
    /// The rate rule divides by this rather than assuming a full hour, so a
    /// young template is never an infinite multiple of nothing.
    pub span_secs: u64,
}

/// The bounded window table.
pub struct WindowTable {
    start: Instant,
    windows: HashMap<TemplateId, TemplateWindow>,
    max_windows: usize,
    last_idle_sweep: Instant,
}

impl WindowTable {
    pub fn new(max_windows: usize) -> Self {
        let now = Instant::now();
        Self {
            start: now,
            windows: HashMap::new(),
            max_windows: max_windows.max(1),
            last_idle_sweep: now,
        }
    }

    fn secs(&self, at: Instant) -> i64 {
        at.saturating_duration_since(self.start).as_secs() as i64
    }

    /// Record one event and return the template's stats after it. Always
    /// records, even for unjudged templates: by the time the verdict lands
    /// the window already has history.
    pub fn record(&mut self, template: &TemplateId, host: &str, now: Instant) -> WindowStats {
        let at = self.secs(now);
        if !self.windows.contains_key(template) {
            self.evict_idle(now);
        }
        let window = self
            .windows
            .entry(*template)
            .or_insert_with(|| TemplateWindow {
                short: Ring::new(SHORT_BUCKETS),
                long: Ring::new(LONG_BUCKETS),
                hosts: HashMap::new(),
                first_seen: now,
                last_seen: now,
            });
        window.short.add(at / SHORT_BUCKET_SECS);
        window.long.add(at / LONG_BUCKET_SECS);
        window.last_seen = now;
        window
            .hosts
            .retain(|_, last| at - *last <= SHORT_WINDOW_SECS_I64 * 12);
        window.hosts.insert(host.to_owned(), at);
        stats_for(window, self.start, at)
    }

    /// Drop templates idle for over an hour, then — if a runaway minter is
    /// still over the cap — the stalest first. The idle sweep runs at most
    /// once a minute: under the cap a miss costs one hash lookup, and the
    /// full retain only fires as housekeeping. The over-cap cut still runs
    /// immediately, because the bound must hold even mid-runaway.
    fn evict_idle(&mut self, now: Instant) {
        if self.windows.len() < self.max_windows {
            if now.saturating_duration_since(self.last_idle_sweep) < IDLE_SWEEP_INTERVAL {
                return;
            }
            self.last_idle_sweep = now;
            let before = self.windows.len();
            self.windows.retain(|_, window| {
                now.saturating_duration_since(window.last_seen)
                    <= Duration::from_secs(IDLE_EVICT_SECS)
            });
            if self.windows.len() != before {
                tracing::debug!(
                    evicted = before - self.windows.len(),
                    "window table evicted idle templates"
                );
            }
            return;
        }
        let mut oldest: Vec<(TemplateId, Instant)> = self
            .windows
            .iter()
            .map(|(id, window)| (*id, window.last_seen))
            .collect();
        oldest.sort_by_key(|(_, seen)| *seen);
        let over = self.windows.len().saturating_sub(self.max_windows) + 1;
        for (id, _) in oldest.into_iter().take(over) {
            self.windows.remove(&id);
        }
    }

    /// Templates currently tracked. A test and debugging hook, not a hot read.
    pub fn len(&self) -> usize {
        self.windows.len()
    }

    /// Whether a template currently holds a window. Eviction proof.
    pub fn contains(&self, template: &TemplateId) -> bool {
        self.windows.contains_key(template)
    }
}

fn stats_for(window: &TemplateWindow, start: Instant, at: i64) -> WindowStats {
    let first_seen_secs = window.first_seen.saturating_duration_since(start).as_secs();
    WindowStats {
        short_total: window.short.total(at / SHORT_BUCKET_SECS),
        long_total: window.long.total(at / LONG_BUCKET_SECS),
        distinct_hosts: window
            .hosts
            .values()
            .filter(|last| at - **last < SHORT_WINDOW_SECS_I64)
            .count(),
        first_seen_secs,
        span_secs: (at as u64)
            .saturating_sub(first_seen_secs)
            .min(SHORT_WINDOW_SECS_I64 as u64 * 12),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn table() -> WindowTable {
        WindowTable::new(65_536)
    }

    fn id(text: &str) -> TemplateId {
        TemplateId::of(text)
    }

    #[tokio::test]
    async fn one_event_counts_once_in_both_windows() {
        tokio::time::pause();
        let mut table = table();
        let now = Instant::now();
        let stats = table.record(&id("a"), "web01", now);
        assert_eq!(stats.short_total, 1);
        assert_eq!(stats.long_total, 1);
        assert_eq!(stats.distinct_hosts, 1);
        assert_eq!(stats.first_seen_secs, 0);
    }

    #[tokio::test]
    async fn buckets_expire_as_the_clock_advances() {
        tokio::time::pause();
        let mut table = table();
        let template = id("a");
        table.record(&template, "web01", Instant::now());
        tokio::time::advance(Duration::from_secs(301)).await;
        let stats = table.record(&template, "web01", Instant::now());
        // The first event aged out of the five-minute window but is still in
        // the trailing hour.
        assert_eq!(stats.short_total, 1);
        assert_eq!(stats.long_total, 2);
    }

    #[tokio::test]
    async fn distinct_hosts_counts_machines_not_events() {
        tokio::time::pause();
        let mut table = table();
        let template = id("a");
        let now = Instant::now();
        table.record(&template, "web01", now);
        table.record(&template, "web01", now);
        let stats = table.record(&template, "db01", now);
        assert_eq!(stats.short_total, 3);
        assert_eq!(stats.distinct_hosts, 2);
    }

    #[tokio::test]
    async fn an_idle_template_loses_its_window_after_an_hour() {
        tokio::time::pause();
        let mut table = table();
        let stale = id("stale");
        table.record(&stale, "web01", Instant::now());
        tokio::time::advance(Duration::from_secs(3601)).await;
        table.record(&id("fresh"), "web01", Instant::now());
        assert!(!table.contains(&stale));
        assert!(table.contains(&id("fresh")));
    }
}
