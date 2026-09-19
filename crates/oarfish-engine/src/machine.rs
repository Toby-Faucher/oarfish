//! The alarm state machine: the only writer of open-alarm state.
//!
//! One engine task owns the windows, the open alarms and the [`DelayQueue`].
//! The M3 pipeline sends it [`EngineInput`] over a channel; it publishes
//! [`AlarmChange`] on a `broadcast` channel that SSE handlers subscribe to.
//! A single owner is not a preference: the state machine must be the only
//! writer of open-alarm state, and one owning task gives that without locks
//! while keeping contention off the every-line path.
//!
//! What causes a raise, in order:
//!
//! 1. **The verdict gates.** A template may alarm only if judged actionable
//!    at or above the severity floor. No verdict yet means no raise.
//! 2. **High severity bypasses the window.** A template judged critical
//!    raises on first sight.
//! 3. **Otherwise the window triggers**, on count or rate over threshold.
//!
//! Dedupe keys on `(template_id, host)`: an open alarm for that pair takes a
//! `count` bump and a timer reset, not a second alarm. Auto-clear is one
//! `DelayQueue` entry per open alarm, reset on each new event. Flap
//! suppression is a cooldown after clear: a re-raise inside it updates the
//! alarm that just closed rather than opening a new one.

use std::collections::HashMap;
use std::future::poll_fn;
use std::sync::{Arc, RwLock};
use std::task::Poll;

use oarfish_core::{
    Alarm, AlarmChange, AlarmId, EngineInput, Event, Severity, TemplateId, Verdict, VerdictAnswer,
};
use oarfish_store::Verdicts;
use time::OffsetDateTime;
use tokio::sync::{broadcast, mpsc};
use tokio_util::sync::CancellationToken;
use tokio_util::time::DelayQueue;

use crate::{
    EngineConfig, WindowTable,
    windows::{SHORT_WINDOW_SECS, WindowStats},
};

/// Where an alarm would surface. Thresholds scale with the stakes: waking
/// someone needs more certainty than drawing a card on a dashboard.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Lane {
    /// `wake_someone >= 0.90`: page via ntfy (M6).
    Page,
    /// `0.60 - 0.90`, and everything in M5: dashboard only.
    Dashboard,
    /// `< 0.60`: record, no surface.
    Record,
}

/// Route on the contextual check's `wake_someone` value.
///
/// `None` — no contextual check ran — routes dashboard-only. That is the
/// whole of M5: `wake_someone` arrives with M5.5, ntfy delivery with M6, so
/// the page lane is computed here but inert. The alternative, a provisional
/// lane derived from the static verdict, would be a second routing rule that
/// exists only to be deleted, tuned against a signal it is not for.
pub fn route(wake_someone: Option<f64>) -> Lane {
    match wake_someone {
        Some(wake) if wake >= 0.90 => Lane::Page,
        Some(wake) if wake >= 0.60 => Lane::Dashboard,
        Some(_) => Lane::Record,
        None => Lane::Dashboard,
    }
}

/// Read the severity score answer as the X.733 severity the board renders.
/// The mapping is a rounding of the 0–3 score: `3 -> critical`, `2 ->
/// major`, `1 -> minor`, `0 -> info`. `cleared` is never reachable from a
/// score — it is a state the machine assigns, never a judgement Jev makes.
fn severity_of(verdict: &Verdict) -> Option<Severity> {
    match verdict.answers.get("severity")? {
        VerdictAnswer::Score { score, .. } => Some(match score.round() as i64 {
            3.. => Severity::Critical,
            2 => Severity::Major,
            1 => Severity::Minor,
            _ => Severity::Info,
        }),
        _ => None,
    }
}

/// Whether the `actionable` noul value clears the threshold. A missing or
/// misshapen answer closes the gate: an unjudged template never raises,
// no matter how hard it bursts.
fn is_actionable(verdict: &Verdict, threshold: f64) -> Option<bool> {
    match verdict.answers.get("actionable")? {
        VerdictAnswer::Noul { noul } => Some(*noul >= threshold),
        _ => None,
    }
}

/// The gate: actionable at or above the severity floor, else nothing raises.
fn gate(verdict: &Verdict, config: &EngineConfig) -> Option<Severity> {
    let severity = severity_of(verdict)?;
    if severity < config.gate_floor {
        return None;
    }
    if !is_actionable(verdict, config.actionable_threshold)? {
        return None;
    }
    Some(severity)
}

/// Dedupe key: one alarm per template per host. The window stays per
/// template so the fleet-wide view survives; correlation stays per host.
type AlarmKey = (TemplateId, String);

struct OpenEntry {
    alarm: Alarm,
    timer: tokio_util::time::delay_queue::Key,
}

struct Tombstone {
    alarm: Alarm,
    cleared_at: tokio::time::Instant,
}

/// The live open-alarm set, shared with the API read-only. The engine task
/// is the only writer; serving `GET /api/alarms` from this instead of the
/// store keeps the board on the same record the state machine holds.
pub type Snapshot = Arc<RwLock<HashMap<AlarmId, Alarm>>>;

/// The owning task's state: windows, open alarms, tombstones, timers.
pub struct Engine {
    verdicts: Arc<Verdicts>,
    config: EngineConfig,
    windows: WindowTable,
    open: HashMap<AlarmKey, OpenEntry>,
    by_id: HashMap<AlarmId, AlarmKey>,
    tombstones: HashMap<AlarmKey, Tombstone>,
    timers: DelayQueue<AlarmId>,
    tx: broadcast::Sender<AlarmChange>,
    snapshot: Snapshot,
}

impl Engine {
    /// Build the engine and reload open alarms from the store, re-arming
    /// their `DelayQueue` timers at the full silence interval. Windows are
    /// not reloaded — they rebuild from live traffic in minutes.
    pub fn new(verdicts: Arc<Verdicts>, config: EngineConfig) -> Self {
        let (tx, _) = broadcast::channel(config.broadcast_capacity.max(1));
        let mut engine = Self {
            verdicts,
            windows: WindowTable::new(config.max_windows),
            open: HashMap::new(),
            by_id: HashMap::new(),
            tombstones: HashMap::new(),
            timers: DelayQueue::new(),
            tx,
            snapshot: Arc::new(RwLock::new(HashMap::new())),
            config,
        };
        for alarm in engine.verdicts.load_open_alarms() {
            tracing::info!(alarm_id = %alarm.id, "reloaded open alarm after restart");
            engine.insert_open(alarm, engine.config.silence);
        }
        engine
    }

    /// Subscribe to [`AlarmChange`]. SSE handlers hold one receiver each; a
    /// lagging one resyncs from `/api/alarms`.
    pub fn subscribe(&self) -> broadcast::Receiver<AlarmChange> {
        self.tx.subscribe()
    }

    /// The broadcast sender, for SSE handlers to subscribe per connection.
    pub fn sender(&self) -> broadcast::Sender<AlarmChange> {
        self.tx.clone()
    }

    /// The live open-alarm set for `GET /api/alarms`.
    pub fn snapshot_handle(&self) -> Snapshot {
        Arc::clone(&self.snapshot)
    }

    /// Open alarms, oldest first. A test and debugging read, not a hot path.
    pub fn open_alarms(&self) -> Vec<Alarm> {
        let mut alarms: Vec<Alarm> = self
            .open
            .values()
            .map(|entry| entry.alarm.clone())
            .collect();
        alarms.sort_by_key(|alarm| alarm.opened_at);
        alarms
    }

    /// Templates currently holding a window. Eviction proof.
    pub fn window_count(&self) -> usize {
        self.windows.len()
    }

    /// Whether a template currently holds a window. Eviction proof.
    pub fn is_window_tracked(&self, template_id: &TemplateId) -> bool {
        self.windows.contains(template_id)
    }

    /// The every-line entry: look the verdict up, then classify. The lookup
    /// is synchronous and cheap — moka, then fjall, then an enqueue and
    /// `None` — and gating is a decision-layer concern, so it lives here
    /// rather than in the pipeline.
    pub fn on_event(&mut self, input: EngineInput) {
        let verdict = self
            .verdicts
            .verdict_for(&input.template_id, &input.template);
        self.on_classified(
            &input.event,
            input.template_id,
            &input.template,
            verdict.as_ref(),
        );
    }

    /// Classify one line against a verdict. The verdict is a parameter —
    /// rather than looked up — so tests drive the machine deterministically
    /// without a store or a model.
    pub fn on_classified(
        &mut self,
        event: &Event,
        template_id: TemplateId,
        template: &str,
        verdict: Option<&Verdict>,
    ) {
        let now = tokio::time::Instant::now();
        let stats = self.windows.record(&template_id, &event.host, now);
        let Some(verdict) = verdict else { return };
        let Some(severity) = gate(verdict, &self.config) else {
            return;
        };
        let key: AlarmKey = (template_id, event.host.clone());

        if let Some(entry) = self.open.get_mut(&key) {
            entry.alarm.count += 1;
            entry.alarm.severity = entry.alarm.severity.max(severity);
            self.timers.reset(&entry.timer, self.config.silence);
            let alarm = entry.alarm.clone();
            self.save_and_snapshot(&alarm);
            self.publish(AlarmChange::Updated(alarm));
            return;
        }

        if let Some(tombstone) = self.tombstones.remove(&key)
            && now.saturating_duration_since(tombstone.cleared_at) <= self.config.flap_cooldown
        {
            let mut alarm = tombstone.alarm;
            alarm.count += 1;
            alarm.severity = alarm.severity.max(severity);
            tracing::debug!(alarm_id = %alarm.id, "flap cooldown revived a closed alarm");
            self.insert_open(alarm.clone(), self.config.silence);
            self.publish(AlarmChange::Updated(alarm));
            return;
        }

        if severity < self.config.bypass_severity && !window_triggered(&stats, &self.config) {
            return;
        }

        let alarm = Alarm {
            id: AlarmId::generate(),
            template_id,
            template: template.to_owned(),
            severity,
            host: event.host.clone(),
            count: stats.short_total.max(1),
            opened_at: OffsetDateTime::now_utc(),
        };
        tracing::info!(alarm_id = %alarm.id, ?severity, "alarm raised");
        self.insert_open(alarm.clone(), self.config.silence);
        self.publish(AlarmChange::Raised(alarm));
    }

    /// Fire every silence timer already past its deadline. Never parks:
    /// awaiting the next deadline would fast-forward the paused clock past
    /// it — the idle runtime auto-advances virtual time — and clear alarms
    /// the caller asserts are still open. Tests call this after advancing
    /// the clock; the run loop parks on [`Engine::next_expiry`] instead,
    /// which is correct in production and never used under pause.
    pub fn expire_ready(&mut self) {
        let waker = futures::task::noop_waker();
        let mut cx = std::task::Context::from_waker(&waker);
        while let Poll::Ready(Some(expired)) = self.timers.poll_expired(&mut cx) {
            self.clear(expired.into_inner());
        }
    }

    async fn next_expiry(&mut self) -> Option<AlarmId> {
        poll_fn(|cx| self.timers.poll_expired(cx))
            .await
            .map(|expired| expired.into_inner())
    }

    fn clear(&mut self, id: AlarmId) {
        let Some(key) = self.by_id.remove(&id) else {
            return;
        };
        let Some(entry) = self.open.remove(&key) else {
            return;
        };
        if let Err(error) = self.verdicts.remove_alarm(&id) {
            tracing::error!(alarm_id = %id, %error, "open alarm could not be deleted; it will reload on restart");
        }
        if let Ok(mut snapshot) = self.snapshot.write() {
            snapshot.remove(&id);
        }
        self.tombstones.retain(|_, tombstone| {
            tombstone
                .cleared_at
                .elapsed()
                .le(&self.config.flap_cooldown)
        });
        self.tombstones.insert(
            key,
            Tombstone {
                alarm: entry.alarm.clone(),
                cleared_at: tokio::time::Instant::now(),
            },
        );
        tracing::info!(alarm_id = %id, "alarm auto-cleared after silence");
        self.publish(AlarmChange::Cleared(id));
    }

    /// Track one open alarm everywhere it lives: the maps, the timer, the
    /// snapshot and the store.
    fn insert_open(&mut self, alarm: Alarm, silence: std::time::Duration) {
        let key = (alarm.template_id, alarm.host.clone());
        let timer = self.timers.insert(alarm.id, silence);
        self.by_id.insert(alarm.id, key.clone());
        self.open.insert(
            key,
            OpenEntry {
                alarm: alarm.clone(),
                timer,
            },
        );
        self.save_and_snapshot(&alarm);
    }

    /// Persist one open alarm and mirror it into the snapshot, so what the
    /// board reads and what a restart reloads are the same record.
    fn save_and_snapshot(&self, alarm: &Alarm) {
        if let Err(error) = self.verdicts.save_alarm(alarm) {
            tracing::error!(alarm_id = %alarm.id, %error, "open alarm could not be saved; a restart would lose it");
        }
        if let Ok(mut snapshot) = self.snapshot.write() {
            snapshot.insert(alarm.id, alarm.clone());
        }
    }

    fn publish(&self, change: AlarmChange) {
        // Fails only with no receivers, which is the quiet-night case.
        let _ = self.tx.send(change);
    }

    /// Own the windows, the machine and the timers until shutdown: the
    /// channel closes once the pipeline drains, or the token cancels. Open
    /// alarms stay persisted either way, so a restart re-arms them.
    pub async fn run(mut self, mut rx: mpsc::Receiver<EngineInput>, cancel: CancellationToken) {
        loop {
            tokio::select! {
                _ = cancel.cancelled() => break,
                input = rx.recv() => {
                    match input {
                        Some(input) => self.on_event(input),
                        None => break,
                    }
                }
                expired = self.next_expiry(), if !self.timers.is_empty() => {
                    if let Some(id) = expired {
                        self.clear(id);
                    }
                }
            }
        }
        if let Err(error) = self.verdicts.persist() {
            tracing::error!(%error, "engine shutdown could not flush the store");
        }
    }
}

/// The window trigger: count or rate over threshold. The rate compares the
/// five-minute event rate against the baseline rate over the template's
/// actual history span — not an assumed full hour, which would price a young
/// template as an infinite multiple of nothing. The short window must be
/// full before rates compare: with less than five minutes of history the
/// count rule owns every raise.
fn window_triggered(stats: &WindowStats, config: &EngineConfig) -> bool {
    if stats.short_total >= config.window_count_threshold {
        return true;
    }
    if stats.span_secs < SHORT_WINDOW_SECS || stats.long_total == 0 {
        return false;
    }
    let short_rate = stats.short_total as f64 / SHORT_WINDOW_SECS as f64;
    let base_rate = stats.long_total as f64 / stats.span_secs as f64;
    short_rate >= config.rate_multiple * base_rate
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn severity_scores_round_to_the_x733_subset() {
        assert_eq!(
            severity_of(&verdict_with(2.6, 0.9)),
            Some(Severity::Critical)
        );
        assert_eq!(severity_of(&verdict_with(2.4, 0.9)), Some(Severity::Major));
        assert_eq!(severity_of(&verdict_with(1.4, 0.9)), Some(Severity::Minor));
        assert_eq!(
            severity_of(&verdict_with(3.0, 0.9)),
            Some(Severity::Critical)
        );
        assert_eq!(severity_of(&verdict_with(0.2, 0.9)), Some(Severity::Info));
    }

    #[test]
    fn cleared_is_never_reachable_from_a_score() {
        for score in [0.0, 1.0, 2.0, 3.0, 100.0, -100.0] {
            assert_ne!(
                severity_of(&verdict_with(score, 0.9)),
                Some(Severity::Cleared)
            );
        }
    }

    #[test]
    fn a_missing_or_misshapen_answer_closes_the_gate() {
        let mut verdict = verdict_with(3.0, 0.9);
        verdict.answers.remove("severity");
        assert_eq!(severity_of(&verdict), None);
        assert_eq!(gate(&verdict, &EngineConfig::default()), None);
    }

    /// The routing table as written: 0.93 pages, 0.61 does not, and nothing
    /// in M5 reaches the page lane.
    #[test]
    fn confidence_picks_the_lane() {
        assert_eq!(route(Some(0.93)), Lane::Page);
        assert_eq!(route(Some(0.61)), Lane::Dashboard);
        assert_eq!(route(Some(0.30)), Lane::Record);
        assert_eq!(route(None), Lane::Dashboard);
    }

    fn verdict_with(score: f64, actionable: f64) -> Verdict {
        use std::collections::BTreeMap;
        Verdict {
            template_id: TemplateId::of("task <VAR:NUM> failed"),
            questions_hash: oarfish_core::QuestionsHash::of(b"{}"),
            model: "typesafe/jev-1.13-20260917".to_owned(),
            answers: BTreeMap::from([
                (
                    "severity".to_owned(),
                    VerdictAnswer::Score {
                        score,
                        confidence: 0.61,
                        probabilities: BTreeMap::new(),
                        legend: None,
                    },
                ),
                (
                    "actionable".to_owned(),
                    VerdictAnswer::Noul { noul: actionable },
                ),
            ]),
            judged_at: OffsetDateTime::UNIX_EPOCH,
        }
    }
}
