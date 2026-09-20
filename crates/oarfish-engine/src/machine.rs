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

use std::collections::{HashMap, HashSet};
use std::future::poll_fn;
use std::sync::Arc;
use std::task::Poll;

use oarfish_core::{
    Alarm, AlarmChange, AlarmId, EngineInput, Event, Lane, Snapshot, TemplateId, Verdict,
};
use oarfish_jev::Client;
use oarfish_store::Verdicts;
use time::OffsetDateTime;
use tokio::sync::{broadcast, mpsc};
use tokio_util::sync::CancellationToken;
use tokio_util::time::DelayQueue;

use crate::{
    EngineConfig, WindowTable,
    context::{BurstContext, CheckOutcome, CheckResult, OpenAlarmView, run_check},
    gate::{Gate, gate, is_contextual},
    windows::{SHORT_WINDOW_SECS, WindowStats},
};

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

/// Dedupe key: one alarm per template per host. The window stays per
/// template so the fleet-wide view survives; correlation stays per host.
type AlarmKey = (TemplateId, String);

/// The check's snapshot, frozen at trip time. The spawned task reads this —
/// never live engine state — so the snapshot the model judges is the burst's
/// own, whatever opens or clears during the round trip.
struct FrozenView(Vec<Alarm>);

impl OpenAlarmView for FrozenView {
    fn open_alarms(&self) -> Vec<Alarm> {
        self.0.clone()
    }
}

struct OpenEntry {
    alarm: Alarm,
    timer: tokio_util::time::delay_queue::Key,
}

struct Tombstone {
    alarm: Alarm,
    cleared_at: tokio::time::Instant,
}

/// The owning task's state: windows, open alarms, tombstones, timers, and —
/// M5.5 — the merge aliases it resolves through and the contextual checks it
/// has in flight.
pub struct Engine {
    verdicts: Arc<Verdicts>,
    decide: Client,
    config: EngineConfig,
    windows: WindowTable,
    open: HashMap<AlarmKey, OpenEntry>,
    by_id: HashMap<AlarmId, AlarmKey>,
    tombstones: HashMap<AlarmKey, Tombstone>,
    timers: DelayQueue<AlarmId>,
    tx: broadcast::Sender<AlarmChange>,
    snapshot: Snapshot,
    pending_tx: mpsc::Sender<CheckOutcome>,
    pending_rx: mpsc::Receiver<CheckOutcome>,
    /// Keys with a contextual check in flight. Their windows keep bumping;
    /// nothing else happens until the answer returns.
    in_flight: HashSet<AlarmKey>,
    /// Keys whose last check resolved to `Record`. No new check spawns until
    /// the window cools below the raise rule — one check per burst, never one
    /// per line.
    suppressed: HashMap<AlarmKey, tokio::time::Instant>,
    judged_tx: mpsc::Sender<(TemplateId, String)>,
    judged_rx: mpsc::Receiver<(TemplateId, String)>,
    /// Hosts that saw a template while it was still unjudged, by template.
    /// Resolved exactly once, when that template's first-ever judgment
    /// lands — a template is judged once, ever, so there is nothing to keep
    /// waiting for afterward, win or lose.
    unjudged: HashMap<TemplateId, HashSet<String>>,
}

impl Engine {
    /// Build the engine and reload open alarms from the store, re-arming
    /// their `DelayQueue` timers at the full silence interval. Windows are
    /// not reloaded — they rebuild from live traffic in minutes. Merge
    /// aliases load with the store, so resolution works from the first line.
    pub fn new(verdicts: Arc<Verdicts>, decide: Client, config: EngineConfig) -> Self {
        let (tx, _) = broadcast::channel(config.broadcast_capacity.max(1));
        let (pending_tx, pending_rx) = mpsc::channel(config.pending_capacity);
        let (judged_tx, judged_rx) = mpsc::channel(config.judged_capacity);
        let mut engine = Self {
            verdicts,
            decide,
            windows: WindowTable::new(config.max_windows),
            open: HashMap::new(),
            by_id: HashMap::new(),
            tombstones: HashMap::new(),
            timers: DelayQueue::new(),
            tx,
            snapshot: Snapshot::new(),
            pending_tx,
            pending_rx,
            in_flight: HashSet::new(),
            suppressed: HashMap::new(),
            judged_tx,
            judged_rx,
            unjudged: HashMap::new(),
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

    /// The judge-completion sender. Wire this into
    /// [`oarfish_store::Verdicts::set_judged_notifier`] once both the store
    /// and the engine exist, so a first-and-only sighting of a template
    /// judged severe enough doesn't have to wait for a repeat.
    pub fn judged_sender(&self) -> mpsc::Sender<(TemplateId, String)> {
        self.judged_tx.clone()
    }

    /// The live open-alarm set for `GET /api/alarms`.
    pub fn snapshot_handle(&self) -> Snapshot {
        self.snapshot.clone()
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

    /// The every-line entry: resolve through the alias table, look the
    /// verdict up, then classify. Resolution runs first — one in-memory
    /// lookup, pure Rust, no network, no disk — so two halves of a split
    /// event feed one window and one alarm. The lookup and the machine all
    /// key on the survivor.
    pub fn on_event(&mut self, input: EngineInput) {
        let (template_id, template) = match self.verdicts.merges().resolve(&input.template_id) {
            (resolved, Some(canonical)) => (resolved, canonical),
            (resolved, None) => (resolved, input.template),
        };
        let verdict = self.verdicts.verdict_for(&template_id, &template);
        self.on_classified(&input.event, template_id, &template, verdict.as_ref());
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
        let Some(verdict) = verdict else {
            // Unjudged: nothing to gate on yet. Remembered so a first-ever
            // judgment that lands severe enough can raise without waiting
            // for this host to see the template a second time.
            self.unjudged
                .entry(template_id)
                .or_default()
                .insert(event.host.clone());
            return;
        };
        let Some(Gate { severity }) = gate(verdict, &self.config) else {
            return;
        };
        let key: AlarmKey = (template_id, event.host.clone());

        // A check is already in flight for this key: the window above keeps
        // bumping as normal, and nothing else happens until the answer
        // returns. In particular no second check spawns.
        if self.in_flight.contains(&key) {
            return;
        }

        // The last check for this key resolved to `Record`: no new check
        // spawns until the window cools below the raise rule. One check per
        // burst, never one per line.
        if self.suppressed.contains_key(&key) {
            if severity < self.config.bypass_severity && !window_triggered(&stats, &self.config) {
                self.suppressed.remove(&key);
            } else {
                return;
            }
        }

        if let Some(entry) = self.open.get_mut(&key) {
            entry.alarm.count += 1;
            entry.alarm.severity = entry.alarm.severity.max(severity);
            self.timers.reset(&entry.timer, self.config.silence);
            let alarm = entry.alarm.clone();
            self.save_and_snapshot(&alarm);
            self.publish(AlarmChange::Updated(alarm));
            return;
        }

        // Revive is still subject to the raise rule: a single gated event
        // inside the cooldown must not re-open what took a window to open.
        if let Some(cleared_at) = self
            .tombstones
            .get(&key)
            .map(|tombstone| tombstone.cleared_at)
            && now.saturating_duration_since(cleared_at) <= self.config.flap_cooldown
            && (severity >= self.config.bypass_severity || window_triggered(&stats, &self.config))
        {
            let tombstone = self
                .tombstones
                .remove(&key)
                .expect("tombstone observed above");
            let mut alarm = tombstone.alarm;
            alarm.count += 1;
            alarm.severity = alarm.severity.max(severity);
            tracing::debug!(alarm_id = %alarm.id, "flap cooldown revived a closed alarm");
            self.insert_open(alarm.clone(), self.config.silence);
            self.publish(AlarmChange::Raised(alarm));
            return;
        }
        // A stale tombstone outside the cooldown is dead weight: drop it so
        // the map does not grow with one entry per cleared alarm forever.
        // (The periodic retain in `clear` only runs on a later clear.)
        if let Some(tombstone) = self.tombstones.get(&key)
            && now.saturating_duration_since(tombstone.cleared_at) > self.config.flap_cooldown
        {
            self.tombstones.remove(&key);
        }

        if severity < self.config.bypass_severity && !window_triggered(&stats, &self.config) {
            return;
        }

        // Flagged templates wait for the contextual check instead of raising
        // outright. The raise is what waits, not the engine: the window keeps
        // bumping on later events while the check flies, and nothing that
        // has surfaced is ever withdrawn.
        if is_contextual(verdict, self.config.contextual_threshold) {
            self.spawn_check(event, template_id, template, severity, stats);
            return;
        }

        self.raise(
            &event.host,
            template_id,
            template,
            &event.raw_lossy(),
            severity,
            Lane::Dashboard,
        );
    }

    /// Raise one alarm into a lane, publish it, and track it everywhere a
    /// raise lives. The M5 path raises here directly at `Dashboard` — no
    /// contextual check ran; that is the whole of M5 — while checked bursts
    /// arrive through [`Engine::apply`] with the lane the check routed.
    fn raise(
        &mut self,
        host: &str,
        template_id: TemplateId,
        template: &str,
        exemplar: &str,
        severity: oarfish_core::Severity,
        lane: Lane,
    ) {
        let alarm = Alarm {
            id: AlarmId::generate(),
            template_id,
            template: template.to_owned(),
            exemplar: exemplar.to_owned(),
            severity,
            host: host.to_owned(),
            lane,
            count: 1,
            opened_at: OffsetDateTime::now_utc(),
        };
        tracing::info!(alarm_id = %alarm.id, ?severity, ?lane, "alarm raised");
        self.insert_open(alarm.clone(), self.config.silence);
        self.publish(AlarmChange::Raised(alarm));
    }

    /// Wraps [`Engine::on_judged`] with the store lookup, the same
    /// relationship [`Engine::on_event`] has to [`Engine::on_classified`]:
    /// the wrapper touches the store, the inner method is a pure function of
    /// its verdict so tests drive it with a hand-built one, no store or
    /// model required.
    fn on_judged_event(&mut self, template_id: TemplateId, template: String) {
        let Some(verdict) = self.verdicts.verdict_for(&template_id, &template) else {
            return;
        };
        self.on_judged(template_id, &template, &verdict);
    }

    /// A template's first-ever judgment just landed. Any host that saw it
    /// while it was still unjudged is checked against the same
    /// `bypass_severity` bar a later sighting would otherwise need a whole
    /// window to reach — a first-and-only sighting no longer has to wait for
    /// a repeat, since the repeat was only ever standing in for "nothing was
    /// known yet to gate on," not a requirement in its own right.
    ///
    /// Resolves the template's `unjudged` entry unconditionally, whether or
    /// not anything ends up raising: a template is judged exactly once,
    /// ever, so there is nothing left to react to on a second look.
    pub fn on_judged(&mut self, template_id: TemplateId, template: &str, verdict: &Verdict) {
        let Some(hosts) = self.unjudged.remove(&template_id) else {
            return;
        };
        let Some(Gate { severity }) = gate(verdict, &self.config) else {
            return;
        };
        if severity < self.config.bypass_severity {
            return;
        }
        for host in hosts {
            let key: AlarmKey = (template_id, host.clone());
            // A normal line for this exact template may have already
            // arrived and raised through on_classified before this
            // notification was processed — the two travel on separate
            // channels with no ordering guarantee between them.
            if self.open.contains_key(&key) {
                continue;
            }
            self.raise(&host, template_id, template, "", severity, Lane::Dashboard);
        }
    }

    /// Spawn the contextual check for one flagged burst and mark the key
    /// in-flight. The snapshot is captured synchronously — the open set at
    /// trip time is what the model judges — while the call flies on a task
    /// the loop never awaits. Only this raise is deferred.
    ///
    /// Requires a runtime context: the run loop and every test caller have
    /// one. Without one the burst raises at `Dashboard` instead of vanishing
    /// — failure raises rather than swallows, and no runtime is a failure.
    fn spawn_check(
        &mut self,
        event: &Event,
        template_id: TemplateId,
        template: &str,
        severity: oarfish_core::Severity,
        stats: WindowStats,
    ) {
        let burst = BurstContext {
            template_id,
            template: template.to_owned(),
            host: event.host.clone(),
            severity,
            stats,
        };
        let key: AlarmKey = (template_id, event.host.clone());
        self.in_flight.insert(key);
        // The trip line rides beside the check, never through it: the model
        // judges the burst, while the raise that follows keeps the verbatim
        // bytes for the board's forensics panel.
        let exemplar = event.raw_lossy().into_owned();
        // Frozen at trip time: the task reads this, never live state, so the
        // snapshot the model judges is the burst's own.
        let view = FrozenView(self.open_alarms());
        let decide = self.decide.clone();
        let config = self.config.clone();
        let tx = self.pending_tx.clone();
        match tokio::runtime::Handle::try_current() {
            Ok(handle) => {
                handle.spawn(async move {
                    let result = run_check(
                        &decide,
                        &view,
                        &burst,
                        config.max_context_alarms,
                        config.check_timeout,
                    )
                    .await;
                    let outcome = match result {
                        CheckResult::Answered(answer) => CheckOutcome::Answered {
                            template_id: burst.template_id,
                            template: burst.template.clone(),
                            host: burst.host.clone(),
                            exemplar: exemplar.clone(),
                            severity: burst.severity,
                            answer,
                        },
                        CheckResult::Failed => CheckOutcome::Failed {
                            template_id: burst.template_id,
                            template: burst.template.clone(),
                            host: burst.host.clone(),
                            exemplar: exemplar.clone(),
                            severity: burst.severity,
                        },
                    };
                    let _ = tx.send(outcome).await;
                });
            }
            Err(_) => {
                let _ = tx.try_send(CheckOutcome::Failed {
                    template_id: burst.template_id,
                    template: burst.template.clone(),
                    host: burst.host.clone(),
                    exemplar,
                    severity: burst.severity,
                });
            }
        }
    }

    /// Apply one returning check answer. Data flows one way per evaluation —
    /// committed state, then check, then answer, then mutation — so feedback
    /// happens across bursts, never within one.
    ///
    /// The snapshot re-validates first: a Jev call is not instantaneous and
    /// auto-clear runs independently, so the alarm named by `correlates_with`
    /// may have cleared during the round trip. A vanished alarm is `none` —
    /// and the burst still raises: a cleared correlation must never swallow
    /// what the window already decided was worth raising.
    fn apply(&mut self, outcome: CheckOutcome) {
        let (template_id, template, host, exemplar, severity, answer) = match outcome {
            CheckOutcome::Answered {
                template_id,
                template,
                host,
                exemplar,
                severity,
                answer,
            } => (
                template_id,
                template,
                host,
                exemplar,
                severity,
                Some(answer),
            ),
            CheckOutcome::Failed {
                template_id,
                template,
                host,
                exemplar,
                severity,
            } => (template_id, template, host, exemplar, severity, None),
        };
        let key: AlarmKey = (template_id, host.clone());
        self.in_flight.remove(&key);

        // The key may have resolved while the check flew: an open alarm
        // takes a count bump, not a second alarm and not a second check.
        if self.open.contains_key(&key) {
            return;
        }

        let Some(answer) = answer else {
            // Failure raises rather than swallows: `route(None)`, exactly
            // the M5 behaviour for a burst with no check behind it.
            self.raise(
                &host,
                template_id,
                &template,
                &exemplar,
                severity,
                Lane::Dashboard,
            );
            return;
        };

        // The decision is recorded before it is trusted. A write failure
        // degrades to the failure lane: uncertainty routes toward the board,
        // never toward silence.
        if let Err(error) = self.verdicts.save_record(&answer.record) {
            tracing::error!(%error, "context record could not be saved; raising without it");
            self.raise(
                &host,
                template_id,
                &template,
                &exemplar,
                severity,
                Lane::Dashboard,
            );
            return;
        }

        // Re-validate the correlation against live state: the alarm named by
        // `correlates_with` may have cleared during the round trip, and a
        // vanished alarm is `none`. Either way the burst raises — the wire
        // answer stays in the record for replay, and consulting a cleared id
        // is what the re-check prevents.
        if let Some(named) = answer.correlates_with {
            if self.by_id.contains_key(&named) {
                tracing::debug!(alarm_id = %named, "checked burst correlates with a live alarm");
            } else {
                tracing::debug!(alarm_id = %named, "correlated alarm cleared mid-flight; treating as none");
            }
        }

        if answer.matters_now < self.config.matters_now_threshold {
            tracing::info!(
                matters_now = answer.matters_now,
                "checked burst resolved to Record; never surfaces"
            );
            self.suppressed.insert(key, tokio::time::Instant::now());
            return;
        }
        let lane = route(Some(answer.wake_someone));
        if lane == Lane::Record {
            self.suppressed.insert(key, tokio::time::Instant::now());
            return;
        }
        self.raise(&host, template_id, &template, &exemplar, severity, lane);
    }

    /// Apply every check answer already past the channel. Never parks: like
    /// [`Engine::expire_ready`], tests call this after the answer lands; the
    /// run loop takes the `pending_rx` branch instead, which is correct in
    /// production and never used under pause.
    pub fn poll_pending(&mut self) {
        while let Ok(outcome) = self.pending_rx.try_recv() {
            self.apply(outcome);
        }
    }

    /// Fire every silence timer already past its deadline. Never parks:    /// awaiting the next deadline would fast-forward the paused clock past
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

    async fn next_expiry(timers: &mut DelayQueue<AlarmId>) -> Option<AlarmId> {
        poll_fn(|cx| timers.poll_expired(cx))
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
        self.snapshot.remove(&id);
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
    /// snapshot and the store. Overwriting a live key cancels the displaced
    /// timer and drops its id mappings, so a leaked timer can never clear
    /// the alarm that replaced it.
    fn insert_open(&mut self, alarm: Alarm, silence: std::time::Duration) {
        let key = (alarm.template_id, alarm.host.clone());
        let timer = self.timers.insert(alarm.id, silence);
        self.by_id.insert(alarm.id, key.clone());
        if let Some(old) = self.open.insert(
            key,
            OpenEntry {
                alarm: alarm.clone(),
                timer,
            },
        ) {
            self.timers.remove(&old.timer);
            self.by_id.remove(&old.alarm.id);
            self.snapshot.remove(&old.alarm.id);
            if let Err(error) = self.verdicts.remove_alarm(&old.alarm.id) {
                tracing::error!(alarm_id = %old.alarm.id, %error, "displaced open alarm could not be deleted; it may reload on restart");
            }
        }
        self.save_and_snapshot(&alarm);
    }

    /// Persist one open alarm and mirror it into the snapshot, so what the
    /// board reads and what a restart reloads are the same record.
    fn save_and_snapshot(&self, alarm: &Alarm) {
        if let Err(error) = self.verdicts.save_alarm(alarm) {
            tracing::error!(alarm_id = %alarm.id, %error, "open alarm could not be saved; a restart would lose it");
        }
        self.snapshot.insert(alarm.clone());
    }

    fn publish(&self, change: AlarmChange) {
        // Fails only with no receivers, which is the quiet-night case.
        let _ = self.tx.send(change);
    }

    /// Own the windows, the machine and the timers until shutdown: the
    /// channel closes once the pipeline drains, and draining is what exits
    /// this loop. The cancellation token is deliberately not an exit path:
    /// breaking on it would drop whatever is still queued, contradicting the
    /// clean-stop property that a stop loses nothing already accepted.
    /// Open alarms stay persisted either way, so a restart re-arms them.
    pub async fn run(mut self, mut rx: mpsc::Receiver<EngineInput>, cancel: CancellationToken) {
        // Keep the token alive for graceful timer shutdown coordination
        // without letting it cut the drain short: dropping it here would be
        // equivalent, but holding it documents the intent.
        let _cancel_guard = cancel.clone();
        loop {
            tokio::select! {
                input = rx.recv() => {
                    match input {
                        Some(input) => self.on_event(input),
                        None => break,
                    }
                }
                expired = Self::next_expiry(&mut self.timers), if !self.timers.is_empty() => {
                    if let Some(id) = expired {
                        self.clear(id);
                    }
                }
                // Returning contextual checks. The loop never awaits a Jev
                // call — the spawned task does, and only the one raise was
                // deferred. `pending_tx` lives in `self`, so `None` is
                // unreachable while the loop is.
                outcome = self.pending_rx.recv() => {
                    if let Some(outcome) = outcome {
                        self.apply(outcome);
                    }
                }
                // A first-ever judgment landing. `judged_tx` lives in
                // `self`, same as `pending_tx`, so `None` is unreachable
                // while the loop is.
                judged = self.judged_rx.recv() => {
                    if let Some((template_id, template)) = judged {
                        self.on_judged_event(template_id, template);
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

    /// The routing table as written: 0.93 pages, 0.61 does not, and nothing
    /// in M5 reaches the page lane.
    #[test]
    fn confidence_picks_the_lane() {
        assert_eq!(route(Some(0.93)), Lane::Page);
        assert_eq!(route(Some(0.61)), Lane::Dashboard);
        assert_eq!(route(Some(0.30)), Lane::Record);
        assert_eq!(route(None), Lane::Dashboard);
    }
}
