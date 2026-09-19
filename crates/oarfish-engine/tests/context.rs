//! M5.5 §9: flagged bursts wait for the contextual check, confidence routes,
//! failure raises, correlation re-validates, and merges repair the split.
//!
//! Real time throughout with short intervals: wiremock answers on localhost
//! in milliseconds, delays stand in for slow models, and the engine's
//! `poll_pending` applies answers the way the run loop's third branch does.
//! Verdict and check mocks share one server with disjoint body matchers —
//! the verdict set asks `actionable`, the check asks `matters_now` — so
//! request counts prove which pass ran.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use bytes::Bytes;
use oarfish_core::{EngineInput, Event, Lane, Source, TemplateId, Verdict, VerdictAnswer};
use oarfish_engine::{AlarmChange, Engine, EngineConfig, is_contextual};
use oarfish_jev::Client;
use oarfish_store::Verdicts;
use wiremock::matchers::{body_string_contains, method};
use wiremock::{Mock, MockServer, ResponseTemplate};

static COUNTER: AtomicU64 = AtomicU64::new(0);

fn tempdir() -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "oarfish-ctx-test-{}-{}",
        std::process::id(),
        COUNTER.fetch_add(1, Ordering::SeqCst)
    ));
    std::fs::create_dir_all(&dir).expect("create tempdir");
    dir
}

fn bundle() -> oarfish_mask::BundleHash {
    oarfish_mask::BundleHash::from_bytes([7u8; 32])
}

fn store_at(dir: &PathBuf, server: &MockServer) -> Arc<Verdicts> {
    Arc::new(
        Verdicts::open(
            dir,
            Client::new(server.uri(), "test-key", "typesafe/jev-1.13"),
            oarfish_engine::static_questions(),
            oarfish_engine::merge_questions(),
            bundle(),
        )
        .expect("open"),
    )
}

fn decide_at(server: &MockServer) -> Client {
    Client::new(server.uri(), "test-key", "typesafe/jev-1.13")
}

fn config() -> EngineConfig {
    EngineConfig {
        silence: Duration::from_secs(300),
        flap_cooldown: Duration::from_secs(600),
        ..EngineConfig::default()
    }
}

/// A gate-faithful verdict body with a chosen `contextual` flag: minor and
/// actionable, so bursts reach the window — and, when flagged, the check.
fn verdict_body(contextual: f64) -> serde_json::Value {
    serde_json::json!({
        "model": "typesafe/jev-1.13-20260917",
        "answers": {
            "kind": {
                "type": "choice",
                "choice": "software",
                "confidence": 0.93,
                "probabilities": {"software_error": 0.93, "routine": 0.07}
            },
            "severity": {
                "type": "score",
                "score": 1.0,
                "confidence": 0.9,
                "probabilities": {"1": 0.9, "0": 0.1}
            },
            "actionable": {"type": "noul", "noul": 0.9},
            "transient": {"type": "noul", "noul": 0.1},
            "security": {"type": "noul", "noul": 0.1},
            "contextual": {"type": "noul", "noul": contextual}
        },
        "usage": {"input_tokens": 100, "output_tokens": 20}
    })
}

/// A contextual-check body with chosen values and a chosen correlation.
fn check_body(matters_now: f64, correlates_with: &str, wake_someone: f64) -> serde_json::Value {
    serde_json::json!({
        "model": "typesafe/jev-1.13-20260917",
        "answers": {
            "matters_now": {"type": "noul", "noul": matters_now},
            "correlates_with": {
                "type": "choice",
                "choice": correlates_with,
                "confidence": 0.9,
                "probabilities": {}
            },
            "wake_someone": {"type": "noul", "noul": wake_someone}
        },
        "usage": {"input_tokens": 200, "output_tokens": 10}
    })
}

async fn mount_verdict(server: &MockServer, contextual: f64) {
    Mock::given(method("POST"))
        .and(body_string_contains("actionable"))
        .respond_with(ResponseTemplate::new(200).set_body_json(verdict_body(contextual)))
        .mount(server)
        .await;
}

async fn mount_check(server: &MockServer, body: serde_json::Value) {
    Mock::given(method("POST"))
        .and(body_string_contains("matters_now"))
        .respond_with(ResponseTemplate::new(200).set_body_json(body))
        .mount(server)
        .await;
}

fn event(host: &str) -> Event {
    Event::new(
        Bytes::from_static(b"db slow query past threshold"),
        host,
        Source::Syslog,
    )
}

fn template() -> (TemplateId, String) {
    let text = "db slow query past threshold".to_owned();
    (TemplateId::of(&text), text)
}

/// Poll the synchronous read until the background judge lands the verdict.
async fn wait_for_verdict(verdicts: &Verdicts, id: &TemplateId, template: &str) {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    loop {
        if verdicts.verdict_for(id, template).is_some() {
            return;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "timed out waiting for the verdict"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

/// Poll `poll_pending` until a raise lands, the way the run loop's third
/// branch would apply it.
async fn wait_for_raise(
    engine: &mut Engine,
    rx: &mut tokio::sync::broadcast::Receiver<AlarmChange>,
    timeout: Duration,
) -> oarfish_core::Alarm {
    let deadline = tokio::time::Instant::now() + timeout;
    loop {
        engine.poll_pending();
        while let Ok(change) = rx.try_recv() {
            if let AlarmChange::Raised(alarm) = change {
                return alarm;
            }
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "timed out waiting for the checked raise"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

fn published(rx: &mut tokio::sync::broadcast::Receiver<AlarmChange>) -> Vec<AlarmChange> {
    let mut out = Vec::new();
    while let Ok(change) = rx.try_recv() {
        out.push(change);
    }
    out
}

async fn request_count(server: &MockServer) -> usize {
    server.received_requests().await.unwrap_or_default().len()
}

/// The flag itself, read beside the question: chosen 0.9 flags, 0.1 does not.
#[tokio::test]
async fn the_contextual_flag_comes_from_the_verdict() {
    fn verdict_with(contextual: f64) -> Verdict {
        Verdict {
            template_id: TemplateId::of("db slow query past threshold"),
            questions_hash: oarfish_core::QuestionsHash::of(b"{}"),
            model: "typesafe/jev-1.13-20260917".to_owned(),
            answers: BTreeMap::from([(
                "contextual".to_owned(),
                VerdictAnswer::Noul { noul: contextual },
            )]),
            judged_at: time::OffsetDateTime::UNIX_EPOCH,
        }
    }
    assert!(is_contextual(&verdict_with(0.9), 0.5));
    assert!(!is_contextual(&verdict_with(0.1), 0.5));
    let mut missing = verdict_with(0.9);
    missing.answers.remove("contextual");
    assert!(!is_contextual(&missing, 0.5));
}

/// Flagged only: an unflagged template bursts to an immediate `Dashboard`
/// raise and the check never runs — one request total, the verdict's.
#[tokio::test]
async fn an_unflagged_template_never_reaches_the_check() {
    let dir = tempdir();
    let server = MockServer::start().await;
    mount_verdict(&server, 0.1).await;

    let store = store_at(&dir, &server);
    let mut engine = Engine::new(Arc::clone(&store), decide_at(&server), config());
    let mut rx = engine.subscribe();
    let (id, text) = template();

    engine.on_event(EngineInput {
        event: event("db01"),
        template_id: id,
        template: text.clone(),
    });
    wait_for_verdict(&store, &id, &text).await;

    for _ in 0..8 {
        engine.on_event(EngineInput {
            event: event("db01"),
            template_id: id,
            template: text.clone(),
        });
    }
    engine.poll_pending();
    let open = engine.open_alarms();
    assert_eq!(open.len(), 1);
    assert_eq!(open[0].lane, Lane::Dashboard);
    assert!(matches!(
        published(&mut rx).as_slice(),
        [AlarmChange::Raised(_), ..]
    ));
    assert_eq!(
        request_count(&server).await,
        1,
        "the verdict only: the check never ran"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// Nothing surfaces early: while the check is in flight — the model delayed
/// two seconds — no `AlarmChange` is published and nothing is open. The
/// answer then lands and the burst raises.
#[tokio::test]
async fn nothing_surfaces_while_a_check_is_in_flight() {
    let dir = tempdir();
    let server = MockServer::start().await;
    mount_verdict(&server, 0.9).await;
    Mock::given(method("POST"))
        .and(body_string_contains("matters_now"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_delay(Duration::from_secs(2))
                .set_body_json(check_body(0.9, "none", 0.7)),
        )
        .mount(&server)
        .await;

    let store = store_at(&dir, &server);
    let mut engine = Engine::new(Arc::clone(&store), decide_at(&server), config());
    let mut rx = engine.subscribe();
    let (id, text) = template();

    engine.on_event(EngineInput {
        event: event("db01"),
        template_id: id,
        template: text.clone(),
    });
    wait_for_verdict(&store, &id, &text).await;

    for _ in 0..5 {
        engine.on_event(EngineInput {
            event: event("db01"),
            template_id: id,
            template: text.clone(),
        });
    }
    engine.poll_pending();
    assert!(engine.open_alarms().is_empty());
    assert!(
        published(&mut rx).is_empty(),
        "nothing that has surfaced is ever withdrawn — so nothing surfaces early"
    );

    let raised = wait_for_raise(&mut engine, &mut rx, Duration::from_secs(10)).await;
    assert_eq!(raised.template_id, id);
    assert_eq!(raised.lane, Lane::Dashboard);
    let _ = std::fs::remove_dir_all(&dir);
}

/// Confidence routes: chosen 0.94 pages.
#[tokio::test]
async fn chosen_094_pages() {
    let dir = tempdir();
    let server = MockServer::start().await;
    mount_verdict(&server, 0.9).await;
    mount_check(&server, check_body(0.9, "none", 0.94)).await;

    let store = store_at(&dir, &server);
    let mut engine = Engine::new(Arc::clone(&store), decide_at(&server), config());
    let mut rx = engine.subscribe();
    let (id, text) = template();

    engine.on_event(EngineInput {
        event: event("db01"),
        template_id: id,
        template: text.clone(),
    });
    wait_for_verdict(&store, &id, &text).await;
    for _ in 0..5 {
        engine.on_event(EngineInput {
            event: event("db01"),
            template_id: id,
            template: text.clone(),
        });
    }

    let raised = wait_for_raise(&mut engine, &mut rx, Duration::from_secs(10)).await;
    assert_eq!(raised.lane, Lane::Page);
    // And the decision was recorded before it was trusted.
    assert!(!store.records_for_template(&id).is_empty());
    let _ = std::fs::remove_dir_all(&dir);
}

/// Confidence routes: chosen 0.61 does not page.
#[tokio::test]
async fn chosen_061_does_not_page() {
    let dir = tempdir();
    let server = MockServer::start().await;
    mount_verdict(&server, 0.9).await;
    mount_check(&server, check_body(0.9, "none", 0.61)).await;

    let store = store_at(&dir, &server);
    let mut engine = Engine::new(Arc::clone(&store), decide_at(&server), config());
    let mut rx = engine.subscribe();
    let (id, text) = template();

    engine.on_event(EngineInput {
        event: event("db01"),
        template_id: id,
        template: text.clone(),
    });
    wait_for_verdict(&store, &id, &text).await;
    for _ in 0..5 {
        engine.on_event(EngineInput {
            event: event("db01"),
            template_id: id,
            template: text.clone(),
        });
    }

    let raised = wait_for_raise(&mut engine, &mut rx, Duration::from_secs(10)).await;
    assert_eq!(raised.lane, Lane::Dashboard);
    let _ = std::fs::remove_dir_all(&dir);
}

/// Below the `matters_now` threshold the burst resolves to `Record` and never
/// surfaces — and the suppression holds: a second burst spawns no second
/// check, so the request count stays at verdict plus one.
#[tokio::test]
async fn matters_below_threshold_never_surfaces_and_suppresses() {
    let dir = tempdir();
    let server = MockServer::start().await;
    mount_verdict(&server, 0.9).await;
    mount_check(&server, check_body(0.2, "none", 0.94)).await;

    let store = store_at(&dir, &server);
    let mut engine = Engine::new(Arc::clone(&store), decide_at(&server), config());
    let mut rx = engine.subscribe();
    let (id, text) = template();

    engine.on_event(EngineInput {
        event: event("db01"),
        template_id: id,
        template: text.clone(),
    });
    wait_for_verdict(&store, &id, &text).await;
    for _ in 0..5 {
        engine.on_event(EngineInput {
            event: event("db01"),
            template_id: id,
            template: text.clone(),
        });
    }
    // Let the answer land and apply.
    tokio::time::sleep(Duration::from_secs(1)).await;
    engine.poll_pending();
    assert!(engine.open_alarms().is_empty());
    assert!(published(&mut rx).is_empty());

    // A second burst on the same key: suppressed, no second check.
    for _ in 0..5 {
        engine.on_event(EngineInput {
            event: event("db01"),
            template_id: id,
            template: text.clone(),
        });
    }
    tokio::time::sleep(Duration::from_secs(1)).await;
    engine.poll_pending();
    assert!(engine.open_alarms().is_empty());
    assert_eq!(
        request_count(&server).await,
        2,
        "one verdict, one check: the second burst spawns nothing"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// Failure raises: a 500 from the check raises at `Dashboard`.
#[tokio::test]
async fn a_500_from_the_check_raises_at_dashboard() {
    let dir = tempdir();
    let server = MockServer::start().await;
    mount_verdict(&server, 0.9).await;
    Mock::given(method("POST"))
        .and(body_string_contains("matters_now"))
        .respond_with(ResponseTemplate::new(500).set_body_string("overloaded"))
        .mount(&server)
        .await;

    let store = store_at(&dir, &server);
    let mut engine = Engine::new(Arc::clone(&store), decide_at(&server), config());
    let mut rx = engine.subscribe();
    let (id, text) = template();

    engine.on_event(EngineInput {
        event: event("db01"),
        template_id: id,
        template: text.clone(),
    });
    wait_for_verdict(&store, &id, &text).await;
    for _ in 0..5 {
        engine.on_event(EngineInput {
            event: event("db01"),
            template_id: id,
            template: text.clone(),
        });
    }

    let raised = wait_for_raise(&mut engine, &mut rx, Duration::from_secs(10)).await;
    assert_eq!(raised.lane, Lane::Dashboard);
    let _ = std::fs::remove_dir_all(&dir);
}

/// Failure raises: a check that never answers hits the timeout and raises at
/// `Dashboard` — a check that cannot answer must not suppress the alarm.
#[tokio::test]
async fn a_check_timeout_raises_at_dashboard() {
    let dir = tempdir();
    let server = MockServer::start().await;
    mount_verdict(&server, 0.9).await;
    Mock::given(method("POST"))
        .and(body_string_contains("matters_now"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_delay(Duration::from_secs(30))
                .set_body_json(check_body(0.9, "none", 0.94)),
        )
        .mount(&server)
        .await;

    let store = store_at(&dir, &server);
    let mut engine = Engine::new(
        Arc::clone(&store),
        decide_at(&server),
        EngineConfig {
            check_timeout: Duration::from_millis(300),
            silence: Duration::from_secs(300),
            flap_cooldown: Duration::from_secs(600),
            ..EngineConfig::default()
        },
    );
    let mut rx = engine.subscribe();
    let (id, text) = template();

    engine.on_event(EngineInput {
        event: event("db01"),
        template_id: id,
        template: text.clone(),
    });
    wait_for_verdict(&store, &id, &text).await;
    for _ in 0..5 {
        engine.on_event(EngineInput {
            event: event("db01"),
            template_id: id,
            template: text.clone(),
        });
    }

    let raised = wait_for_raise(&mut engine, &mut rx, Duration::from_secs(10)).await;
    assert_eq!(raised.lane, Lane::Dashboard);
    let _ = std::fs::remove_dir_all(&dir);
}

/// Correlation re-validates: the alarm named by `correlates_with` clears
/// mid-flight, and the burst still raises — a vanished correlation resolves
/// to `none`, never to silence.
#[tokio::test]
async fn an_alarm_cleared_mid_flight_resolves_to_none() {
    let dir = tempdir();
    let server = MockServer::start().await;
    mount_verdict(&server, 0.9).await;

    let store = store_at(&dir, &server);
    let mut engine = Engine::new(
        Arc::clone(&store),
        decide_at(&server),
        EngineConfig {
            silence: Duration::from_secs(1),
            flap_cooldown: Duration::from_secs(600),
            ..EngineConfig::default()
        },
    );
    let mut rx = engine.subscribe();
    let (id, text) = template();

    // Land the verdict first: the critical bypass below needs no verdict of
    // its own — it is driven directly — but the flagged burst does.
    engine.on_event(EngineInput {
        event: event("db01"),
        template_id: id,
        template: text.clone(),
    });
    wait_for_verdict(&store, &id, &text).await;

    // Open an alarm on the same host through the critical bypass, driven
    // directly so no second verdict is involved.
    let critical = {
        let mut answers = BTreeMap::new();
        answers.insert(
            "severity".to_owned(),
            VerdictAnswer::Score {
                score: 3.0,
                confidence: 0.9,
                probabilities: BTreeMap::new(),
                legend: None,
            },
        );
        answers.insert("actionable".to_owned(), VerdictAnswer::Noul { noul: 0.9 });
        answers.insert("contextual".to_owned(), VerdictAnswer::Noul { noul: 0.1 });
        Verdict {
            template_id: TemplateId::of("critical template here"),
            questions_hash: oarfish_core::QuestionsHash::of(b"{}"),
            model: "test".to_owned(),
            answers,
            judged_at: time::OffsetDateTime::UNIX_EPOCH,
        }
    };
    engine.on_classified(
        &event("db01"),
        TemplateId::of("critical template here"),
        "critical template here",
        Some(&critical),
    );
    assert_eq!(engine.open_alarms().len(), 1);
    let correlated_id = engine.open_alarms()[0].id;
    let _ = published(&mut rx);

    // The check names that alarm and answers slowly: it will clear first.
    Mock::given(method("POST"))
        .and(body_string_contains("matters_now"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_delay(Duration::from_secs(2))
                .set_body_json(check_body(0.9, &correlated_id.to_string(), 0.94)),
        )
        .mount(&server)
        .await;
    for _ in 0..5 {
        engine.on_event(EngineInput {
            event: event("db01"),
            template_id: id,
            template: text.clone(),
        });
    }
    engine.poll_pending();
    assert!(published(&mut rx).is_empty());

    // Past the one-second silence the correlated alarm clears mid-flight.
    tokio::time::sleep(Duration::from_millis(1200)).await;
    engine.expire_ready();
    assert!(engine.open_alarms().is_empty());

    // The answer lands against a cleared correlation: the burst still raises
    // as its own incident, at the lane its `wake_someone` earned.
    let raised = wait_for_raise(&mut engine, &mut rx, Duration::from_secs(10)).await;
    assert_eq!(raised.template_id, id);
    assert_eq!(raised.lane, Lane::Page);
    assert_ne!(raised.id, correlated_id);
    let _ = std::fs::remove_dir_all(&dir);
}

/// Merges repair the split: two aliased templates feed one window and raise
/// one alarm, keyed on the survivor.
#[tokio::test]
async fn two_aliased_templates_share_one_window_and_raise_one_alarm() {
    let dir = tempdir();
    let server = MockServer::start().await;
    mount_verdict(&server, 0.1).await;
    Mock::given(method("POST"))
        .and(body_string_contains("same_event"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "model": "typesafe/jev-1.13-20260917",
            "answers": {"same_event": {"type": "noul", "noul": 0.91}},
            "usage": {"input_tokens": 50, "output_tokens": 5}
        })))
        .mount(&server)
        .await;

    let store = store_at(&dir, &server);
    let mut engine = Engine::new(Arc::clone(&store), decide_at(&server), config());
    let mut rx = engine.subscribe();

    let ta = "backup done files ok size mb".to_owned();
    let tb = "backup done files ok size mb extra".to_owned();
    let a = TemplateId::of(&ta);
    let b = TemplateId::of(&tb);
    assert_ne!(a, b);
    let (lo, lo_text, hi, hi_text) = if a <= b {
        (a, ta.clone(), b, tb.clone())
    } else {
        (b, tb.clone(), a, ta.clone())
    };

    // The pair is judged once through the merge review.
    store.merges().refer(a, &ta, b, &tb);
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    loop {
        if store.merges().lookup(&a, &b).is_some() {
            break;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "timed out waiting for the merge"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert_eq!(store.merges().resolve(&hi), (lo, Some(lo_text.clone())));

    // Both halves judged (each enqueued before the alias landed), then six
    // alternating events: two halves of one event at half rate each, one
    // window, one alarm.
    for (id, text) in [(&a, &ta), (&b, &tb)] {
        engine.on_event(EngineInput {
            event: event("nas01"),
            template_id: *id,
            template: text.clone(),
        });
    }
    wait_for_verdict(&store, &a, &ta).await;
    wait_for_verdict(&store, &b, &tb).await;
    let _ = published(&mut rx);

    for i in 0..6 {
        let (id, text) = if i % 2 == 0 { (a, &ta) } else { (b, &tb) };
        engine.on_event(EngineInput {
            event: event("nas01"),
            template_id: id,
            template: text.clone(),
        });
    }
    engine.poll_pending();

    let open = engine.open_alarms();
    assert_eq!(open.len(), 1, "two halves, one alarm: got {open:?}");
    assert_eq!(open[0].template_id, lo);
    assert_eq!(open[0].template, lo_text);
    let raised = published(&mut rx)
        .into_iter()
        .filter(|change| matches!(change, AlarmChange::Raised(_)))
        .count();
    assert_eq!(raised, 1);
    assert!(engine.is_window_tracked(&lo));
    assert!(
        !engine.is_window_tracked(&hi) || hi == lo,
        "the merged-away id holds no window of its own"
    );
    let _ = hi_text;
    let _ = std::fs::remove_dir_all(&dir);
}
