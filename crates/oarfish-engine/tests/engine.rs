//! M5 §9: the gate holds, severity bypasses, the window triggers, dedupe,
//! flap suppression, auto-clear, eviction and restart.
//!
//! `tokio::time::pause()` throughout: windows, silence intervals and flap
//! cooldowns are untestable in real time. Verdicts are hand-built — the
//! machine takes them as a parameter precisely so these tests need no store
//! writes and no model.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use bytes::Bytes;
use oarfish_core::{EngineInput, Event, QuestionsHash, Source, TemplateId, Verdict, VerdictAnswer};
use oarfish_engine::{AlarmChange, Engine, EngineConfig};
use oarfish_jev::Client;
use oarfish_store::Verdicts;

/// A pinned bundle identity. These tests never judge — the client points
/// at a dead port — so any fixed hash stands in for the curated bundle.
fn bundle() -> oarfish_mask::BundleHash {
    oarfish_mask::BundleHash::from_bytes([7u8; 32])
}

static COUNTER: AtomicU64 = AtomicU64::new(0);

fn tempdir() -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "oarfish-m5-test-{}-{}",
        std::process::id(),
        COUNTER.fetch_add(1, Ordering::SeqCst)
    ));
    std::fs::create_dir_all(&dir).expect("create tempdir");
    dir
}

/// A verdict cache that never judges: the client points at a dead port, so
/// every lookup misses and stays missed. Hand-built verdicts drive the
/// machine through `on_classified`.
fn verdicts(dir: &PathBuf) -> Arc<Verdicts> {
    Arc::new(
        Verdicts::open(
            dir,
            Client::new(
                "http://127.0.0.1:9/unreachable",
                "test-key",
                "typesafe/jev-1.13",
            ),
            oarfish_engine::static_questions(),
            oarfish_engine::merge_questions(),
            bundle(),
        )
        .expect("open"),
    )
}

fn test_config() -> EngineConfig {
    EngineConfig {
        silence: Duration::from_secs(300),
        flap_cooldown: Duration::from_secs(600),
        ..EngineConfig::default()
    }
}

fn engine_at(dir: &PathBuf) -> (Engine, Arc<Verdicts>) {
    let store = verdicts(dir);
    let engine = Engine::new(Arc::clone(&store), decide_client(), test_config());
    (engine, store)
}

/// The contextual-check client. Sync machine tests never flag a template, so
/// this never fires; tests that flag one point it at wiremock instead.
fn decide_client() -> Client {
    Client::new(
        "http://127.0.0.1:9/unreachable",
        "test-key",
        "typesafe/jev-1.13",
    )
}

fn verdict(score: f64, actionable: f64) -> Verdict {
    Verdict {
        template_id: TemplateId::of("task <VAR:NUM> failed"),
        questions_hash: QuestionsHash::of(b"{}"),
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
        judged_at: time::OffsetDateTime::UNIX_EPOCH,
    }
}

fn event(host: &str) -> Event {
    Event::new(Bytes::from_static(b"task 12 failed"), host, Source::Syslog)
}

fn template() -> (TemplateId, String) {
    let text = "task <VAR:NUM> failed".to_owned();
    (TemplateId::of(&text), text)
}

/// Drain the broadcast queue into a vec. Published changes are synchronous,
/// so after driving the machine every change is already queued.
fn published(rx: &mut tokio::sync::broadcast::Receiver<AlarmChange>) -> Vec<AlarmChange> {
    let mut out = Vec::new();
    while let Ok(change) = rx.try_recv() {
        out.push(change);
    }
    out
}

/// The gate holds: an unjudged template never raises, however hard it bursts.
#[tokio::test]
async fn an_unjudged_template_never_raises() {
    tokio::time::pause();
    let dir = tempdir();
    let (mut engine, _store) = engine_at(&dir);
    let mut rx = engine.subscribe();
    let (id, text) = template();

    for _ in 0..10 {
        engine.on_classified(&event("web01"), id, &text, None);
    }
    assert!(engine.open_alarms().is_empty());
    assert!(published(&mut rx).is_empty());
    let _ = std::fs::remove_dir_all(&dir);
}

/// The gate holds two more ways: below the severity floor, and not
/// actionable, each stay silent.
#[tokio::test]
async fn the_gate_holds_on_severity_and_actionability() {
    tokio::time::pause();
    let dir = tempdir();
    let (mut engine, _store) = engine_at(&dir);
    let mut rx = engine.subscribe();
    let (id, text) = template();

    // Info, however actionable, is below the minor floor.
    for _ in 0..10 {
        engine.on_classified(&event("web01"), id, &text, Some(&verdict(0.0, 0.95)));
    }
    // Critical but unactionable: nothing a human can do, so no alarm.
    for _ in 0..10 {
        engine.on_classified(&event("web01"), id, &text, Some(&verdict(3.0, 0.1)));
    }
    assert!(engine.open_alarms().is_empty());
    assert!(published(&mut rx).is_empty());
    let _ = std::fs::remove_dir_all(&dir);
}

/// High severity bypasses the window: a critical template raises on first
/// sight. An EXT4 error must not wait for a second occurrence to be believed.
#[tokio::test]
async fn a_critical_template_raises_on_first_sight() {
    tokio::time::pause();
    let dir = tempdir();
    let (mut engine, _store) = engine_at(&dir);
    let mut rx = engine.subscribe();
    let (id, text) = template();

    engine.on_classified(&event("nas01"), id, &text, Some(&verdict(3.0, 0.9)));

    let open = engine.open_alarms();
    assert_eq!(open.len(), 1);
    assert_eq!(open[0].count, 1);
    assert_eq!(open[0].host, "nas01");
    match published(&mut rx).as_slice() {
        [AlarmChange::Raised(alarm)] => assert_eq!(alarm.id, open[0].id),
        other => panic!("expected one Raised, got {other:?}"),
    }
    let _ = std::fs::remove_dir_all(&dir);
}

/// A first-and-only sighting no longer has to wait for a repeat: the
/// unjudged sighting is remembered, and the judgment landing raises it
/// directly, at the exact severity bar `bypass_severity` already names.
#[tokio::test]
async fn a_first_and_only_sighting_raises_once_judgment_lands() {
    tokio::time::pause();
    let dir = tempdir();
    let (mut engine, _store) = engine_at(&dir);
    let mut rx = engine.subscribe();
    let (id, text) = template();

    engine.on_classified(&event("nas01"), id, &text, None);
    assert!(engine.open_alarms().is_empty(), "unjudged, nothing yet");

    engine.on_judged(id, &text, &verdict(3.0, 0.9));

    let open = engine.open_alarms();
    assert_eq!(open.len(), 1, "got {open:?}");
    assert_eq!(open[0].host, "nas01");
    assert_eq!(open[0].count, 1);
    match published(&mut rx).as_slice() {
        [AlarmChange::Raised(alarm)] => assert_eq!(alarm.id, open[0].id),
        other => panic!("expected one Raised, got {other:?}"),
    }
    let _ = std::fs::remove_dir_all(&dir);
}

/// A judgment below `bypass_severity` does not raise just for having
/// finally arrived — it falls back to the normal window-driven path for
/// whatever comes next, exactly as if it had been judged before the first
/// sighting instead of after.
#[tokio::test]
async fn on_judged_does_not_raise_below_the_bypass_bar() {
    tokio::time::pause();
    let dir = tempdir();
    let (mut engine, _store) = engine_at(&dir);
    let mut rx = engine.subscribe();
    let (id, text) = template();

    engine.on_classified(&event("web01"), id, &text, None);
    engine.on_judged(id, &text, &verdict(1.0, 0.9));

    assert!(engine.open_alarms().is_empty());
    assert!(published(&mut rx).is_empty());
    let _ = std::fs::remove_dir_all(&dir);
}

/// Two hosts independently saw the same never-before-seen template while it
/// was unjudged. One judgment, once it lands, raises both — dedupe is per
/// `(template, host)`, and both were waiting.
#[tokio::test]
async fn on_judged_raises_every_host_that_was_waiting() {
    tokio::time::pause();
    let dir = tempdir();
    let (mut engine, _store) = engine_at(&dir);
    let mut rx = engine.subscribe();
    let (id, text) = template();

    engine.on_classified(&event("nas01"), id, &text, None);
    engine.on_classified(&event("proxy01"), id, &text, None);
    engine.on_judged(id, &text, &verdict(3.0, 0.9));

    let mut hosts: Vec<String> = engine.open_alarms().into_iter().map(|a| a.host).collect();
    hosts.sort();
    assert_eq!(hosts, vec!["nas01".to_owned(), "proxy01".to_owned()]);
    assert_eq!(published(&mut rx).len(), 2);
    let _ = std::fs::remove_dir_all(&dir);
}

/// A template nothing was ever waiting on is a no-op: no panic, no raise,
/// nothing published. `on_judged` firing for a template the engine never
/// saw unjudged is a reachable, harmless case.
#[tokio::test]
async fn on_judged_with_nothing_waiting_is_a_noop() {
    tokio::time::pause();
    let dir = tempdir();
    let (mut engine, _store) = engine_at(&dir);
    let mut rx = engine.subscribe();
    let (id, text) = template();

    engine.on_judged(id, &text, &verdict(3.0, 0.9));

    assert!(engine.open_alarms().is_empty());
    assert!(published(&mut rx).is_empty());
    let _ = std::fs::remove_dir_all(&dir);
}

/// The two notification paths can race — a normal line for the same
/// template can raise through `on_classified` before the judge's own
/// notification is processed, since they travel on separate channels with
/// no ordering guarantee. `on_judged` must not raise a second time for a
/// host that already has one open.
#[tokio::test]
async fn on_judged_skips_a_host_that_already_raised_through_the_normal_path() {
    tokio::time::pause();
    let dir = tempdir();
    let (mut engine, _store) = engine_at(&dir);
    let mut rx = engine.subscribe();
    let (id, text) = template();
    let judged = verdict(3.0, 0.9);

    // Unjudged sighting, then a second line for the same template arrives
    // after the verdict is already known — the normal path raises it.
    engine.on_classified(&event("nas01"), id, &text, None);
    engine.on_classified(&event("nas01"), id, &text, Some(&judged));
    assert_eq!(
        engine.open_alarms().len(),
        1,
        "raised through on_classified"
    );
    let _ = published(&mut rx);

    // The judge's own notification arrives after: must not double-raise.
    engine.on_judged(id, &text, &judged);

    assert_eq!(engine.open_alarms().len(), 1, "still exactly one");
    assert!(
        published(&mut rx).is_empty(),
        "no second Raised for the same host"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// The window triggers: a gated minor template raises only once count
/// breaches — four events are silent, the fifth raises.
#[tokio::test]
async fn a_gated_template_raises_only_once_count_breaches() {
    tokio::time::pause();
    let dir = tempdir();
    let (mut engine, _store) = engine_at(&dir);
    let mut rx = engine.subscribe();
    let (id, text) = template();
    let judged = verdict(1.0, 0.9);

    for _ in 0..4 {
        engine.on_classified(&event("web01"), id, &text, Some(&judged));
    }
    assert!(engine.open_alarms().is_empty());
    assert!(published(&mut rx).is_empty());

    engine.on_classified(&event("web01"), id, &text, Some(&judged));
    assert_eq!(engine.open_alarms().len(), 1);
    assert!(matches!(
        published(&mut rx).as_slice(),
        [AlarmChange::Raised(_)]
    ));
    let _ = std::fs::remove_dir_all(&dir);
}

/// The window triggers on rate: with the count rule priced out, a burst at
/// three times the trailing-hour rate still raises.
#[tokio::test]
async fn a_burst_over_three_times_the_trailing_hour_raises_on_rate() {
    tokio::time::pause();
    let dir = tempdir();
    let store = verdicts(&dir);
    let mut engine = Engine::new(
        store,
        decide_client(),
        EngineConfig {
            window_count_threshold: 1_000,
            silence: Duration::from_secs(300),
            flap_cooldown: Duration::from_secs(600),
            ..EngineConfig::default()
        },
    );
    let mut rx = engine.subscribe();
    let (id, text) = template();
    let judged = verdict(1.0, 0.9);

    // Baseline: one event a minute for an hour. The five-minute window holds
    // ~5 throughout, far under the count rule and at the hourly rate.
    for _ in 0..60 {
        engine.on_classified(&event("web01"), id, &text, Some(&judged));
        tokio::time::advance(Duration::from_secs(60)).await;
    }
    assert!(engine.open_alarms().is_empty());

    // Twenty events at once: the five-minute rate jumps past three times
    // the baseline while the count rule stays out of reach. The burst past
    // the raise dedupes into `Updated`s — one alarm, not twenty. The count
    // folds events for this host since the raise, not the fleet-wide
    // template total.
    for _ in 0..20 {
        engine.on_classified(&event("web01"), id, &text, Some(&judged));
    }
    let open = engine.open_alarms();
    assert_eq!(open.len(), 1);
    assert!((1..=20).contains(&open[0].count), "got {}", open[0].count);
    let changes = published(&mut rx);
    assert!(matches!(changes.first(), Some(AlarmChange::Raised(_))));
    assert!(
        changes[1..]
            .iter()
            .all(|change| matches!(change, AlarmChange::Updated(_))),
        "the burst past the raise dedupes, got {changes:?}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// Dedupe: a second event on the same `(template, host)` bumps `count` and
/// opens nothing. A second host opens a second alarm.
#[tokio::test]
async fn a_second_event_on_the_pair_bumps_count_opens_nothing() {
    tokio::time::pause();
    let dir = tempdir();
    let (mut engine, _store) = engine_at(&dir);
    let mut rx = engine.subscribe();
    let (id, text) = template();
    let judged = verdict(3.0, 0.9);

    engine.on_classified(&event("web01"), id, &text, Some(&judged));
    engine.on_classified(&event("web01"), id, &text, Some(&judged));
    let open = engine.open_alarms();
    assert_eq!(open.len(), 1);
    assert_eq!(open[0].count, 2);
    assert!(matches!(
        published(&mut rx).as_slice(),
        [AlarmChange::Raised(_), AlarmChange::Updated(_)]
    ));

    engine.on_classified(&event("db01"), id, &text, Some(&judged));
    assert_eq!(engine.open_alarms().len(), 2);
    let _ = std::fs::remove_dir_all(&dir);
}

/// Dedupe resets the silence timer: an event near the deadline postpones the
/// clear by the full interval.
#[tokio::test]
async fn a_late_event_resets_the_silence_timer() {
    tokio::time::pause();
    let dir = tempdir();
    let (mut engine, _store) = engine_at(&dir);
    let mut rx = engine.subscribe();
    let (id, text) = template();
    let judged = verdict(3.0, 0.9);

    engine.on_classified(&event("web01"), id, &text, Some(&judged));
    tokio::time::advance(Duration::from_secs(200)).await;
    engine.on_classified(&event("web01"), id, &text, Some(&judged));
    tokio::time::advance(Duration::from_secs(200)).await;
    engine.expire_ready();
    // 400 seconds after the raise but only 200 after the reset: still open.
    assert_eq!(engine.open_alarms().len(), 1);

    tokio::time::advance(Duration::from_secs(150)).await;
    engine.expire_ready();
    assert!(engine.open_alarms().is_empty());
    assert!(
        published(&mut rx)
            .iter()
            .any(|change| matches!(change, AlarmChange::Cleared(_)))
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// Auto-clear: silence past the interval clears, and the change is published.
#[tokio::test]
async fn silence_past_the_interval_clears_and_publishes() {
    tokio::time::pause();
    let dir = tempdir();
    let (mut engine, store) = engine_at(&dir);
    let mut rx = engine.subscribe();
    let (id, text) = template();
    let judged = verdict(3.0, 0.9);

    engine.on_classified(&event("web01"), id, &text, Some(&judged));
    let alarm_id = engine.open_alarms()[0].id;
    tokio::time::advance(Duration::from_secs(301)).await;
    engine.expire_ready();

    assert!(engine.open_alarms().is_empty());
    assert!(store.load_open_alarms().is_empty());
    assert!(engine.snapshot_handle().snapshot().is_empty());
    match published(&mut rx).as_slice() {
        [AlarmChange::Raised(_), AlarmChange::Cleared(cleared)] => {
            assert_eq!(*cleared, alarm_id);
        }
        other => panic!("expected Raised then Cleared, got {other:?}"),
    }
    let _ = std::fs::remove_dir_all(&dir);
}

/// Flap suppression: a re-raise inside the cooldown reuses the alarm that
/// just closed rather than opening a new one — same id, published as
/// `Raised` so consumers that removed it on `Cleared` re-add it.
#[tokio::test]
async fn a_re_raise_inside_the_cooldown_updates_rather_than_re_raises() {
    tokio::time::pause();
    let dir = tempdir();
    let (mut engine, _store) = engine_at(&dir);
    let mut rx = engine.subscribe();
    let (id, text) = template();
    let judged = verdict(3.0, 0.9);

    engine.on_classified(&event("web01"), id, &text, Some(&judged));
    let first_id = engine.open_alarms()[0].id;
    tokio::time::advance(Duration::from_secs(301)).await;
    engine.expire_ready();
    assert!(engine.open_alarms().is_empty());

    tokio::time::advance(Duration::from_secs(100)).await;
    engine.on_classified(&event("web01"), id, &text, Some(&judged));
    let open = engine.open_alarms();
    assert_eq!(open.len(), 1);
    assert_eq!(open[0].id, first_id);
    assert!(matches!(
        published(&mut rx).as_slice(),
        [
            AlarmChange::Raised(_),
            AlarmChange::Cleared(_),
            AlarmChange::Raised(_)
        ]
    ));

    // Past the silence and the cooldown the next burst is a new alarm.
    tokio::time::advance(Duration::from_secs(301)).await;
    engine.expire_ready();
    tokio::time::advance(Duration::from_secs(601)).await;
    engine.on_classified(&event("web01"), id, &text, Some(&judged));
    let open = engine.open_alarms();
    assert_eq!(open.len(), 1);
    assert_ne!(open[0].id, first_id);
    assert!(matches!(
        published(&mut rx).as_slice(),
        [AlarmChange::Cleared(_), AlarmChange::Raised(_)]
    ));
    let _ = std::fs::remove_dir_all(&dir);
}

/// A lone gated event inside the cooldown does not revive: re-raising still
/// needs the window (or a bypass), or one line every twenty minutes would
/// pin the alarm flapping forever.
#[tokio::test]
async fn a_lone_gated_event_inside_the_cooldown_does_not_revive() {
    tokio::time::pause();
    let dir = tempdir();
    let (mut engine, _store) = engine_at(&dir);
    let mut rx = engine.subscribe();
    let (id, text) = template();
    let judged = verdict(1.0, 0.9);

    for _ in 0..5 {
        engine.on_classified(&event("web01"), id, &text, Some(&judged));
    }
    assert_eq!(engine.open_alarms().len(), 1);
    tokio::time::advance(Duration::from_secs(301)).await;
    engine.expire_ready();
    assert!(engine.open_alarms().is_empty());
    let _ = published(&mut rx);

    tokio::time::advance(Duration::from_secs(100)).await;
    engine.on_classified(&event("web01"), id, &text, Some(&judged));
    assert!(
        engine.open_alarms().is_empty(),
        "one gated line must not re-open what took five to raise"
    );
    assert!(published(&mut rx).is_empty());
    let _ = std::fs::remove_dir_all(&dir);
}

/// Eviction: a template idle over an hour loses its window when a new one
/// arrives.
#[tokio::test]
async fn an_idle_template_loses_its_window() {
    tokio::time::pause();
    let dir = tempdir();
    let (mut engine, _store) = engine_at(&dir);
    let judged = verdict(0.0, 0.1);

    let stale = TemplateId::of("stale template here");
    engine.on_classified(&event("web01"), stale, "stale template here", Some(&judged));
    assert!(engine.is_window_tracked(&stale));
    tokio::time::advance(Duration::from_secs(3601)).await;
    let fresh = TemplateId::of("fresh template here");
    engine.on_classified(&event("web01"), fresh, "fresh template here", Some(&judged));
    assert!(!engine.is_window_tracked(&stale));
    assert!(engine.is_window_tracked(&fresh));
    let _ = std::fs::remove_dir_all(&dir);
}

/// Restart: reloaded alarms have armed timers and still auto-clear. The
/// first engine is dropped with its timers; the second reloads from the
/// store and re-arms.
#[tokio::test]
async fn reloaded_alarms_have_armed_timers_and_still_auto_clear() {
    tokio::time::pause();
    let dir = tempdir();
    let judged = verdict(3.0, 0.9);
    let (id, text) = template();

    let alarm_id = {
        let store = verdicts(&dir);
        let mut first = Engine::new(Arc::clone(&store), decide_client(), test_config());
        first.on_classified(&event("web01"), id, &text, Some(&judged));
        assert_eq!(first.open_alarms().len(), 1);
        let alarm_id = first.open_alarms()[0].id;
        // Shut the store cleanly: the judge task holds the database open,
        // and a second open of the same path fails while it lives.
        drop(first);
        Arc::try_unwrap(store)
            .map_err(|_| "the engine still holds the store")
            .expect("sole owner")
            .close()
            .await
            .expect("close");
        alarm_id
    };

    let store = verdicts(&dir);
    let mut second = Engine::new(store, decide_client(), test_config());
    let open = second.open_alarms();
    assert_eq!(open.len(), 1);
    assert_eq!(open[0].id, alarm_id);
    let mut rx = second.subscribe();

    // Re-armed at the full silence interval: no early clear.
    tokio::time::advance(Duration::from_secs(299)).await;
    second.expire_ready();
    assert_eq!(second.open_alarms().len(), 1);

    tokio::time::advance(Duration::from_secs(2)).await;
    second.expire_ready();
    assert!(second.open_alarms().is_empty());
    assert!(matches!(
        published(&mut rx).as_slice(),
        [AlarmChange::Cleared(cleared)] if *cleared == alarm_id
    ));
    let _ = std::fs::remove_dir_all(&dir);
}

/// The every-line entry: an unjudged line through `on_event` misses the
/// cache, enqueues the judge, and raises nothing.
#[tokio::test]
async fn the_every_line_entry_misses_quietly_when_unjudged() {
    tokio::time::pause();
    let dir = tempdir();
    let (mut engine, _store) = engine_at(&dir);
    let mut rx = engine.subscribe();
    let (id, text) = template();

    engine.on_event(EngineInput {
        event: event("web01"),
        template_id: id,
        template: text,
    });
    assert!(engine.open_alarms().is_empty());
    assert!(published(&mut rx).is_empty());
    let _ = std::fs::remove_dir_all(&dir);
}
