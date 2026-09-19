//! M5.5 acceptance: a close pair is judged once through the whole stack, and
//! a flagged burst resolves to `wake_someone` and lands in `Lane::Page`.
//!
//! The full pipeline in both tests — ingest channel, pipeline with merge
//! referral, engine run loop — against a wiremock Jev with disjoint body
//! matchers for the verdict, merge and check passes. Real time throughout
//! with short intervals, the M5 acceptance pattern.

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use bytes::Bytes;
use oarfish_core::{Lane, Source, TemplateId};
use oarfish_engine::{AlarmChange, Engine, EngineConfig};
use oarfish_jev::Client;
use oarfish_store::Verdicts;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;
use wiremock::matchers::{body_string_contains, method};
use wiremock::{Mock, MockServer, ResponseTemplate};

static COUNTER: AtomicU64 = AtomicU64::new(0);

fn tempdir() -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "oarfish-m55-acc-{}-{}",
        std::process::id(),
        COUNTER.fetch_add(1, Ordering::SeqCst)
    ));
    std::fs::create_dir_all(&dir).expect("create tempdir");
    dir
}

fn decide_client(server: &MockServer) -> Client {
    Client::new(server.uri(), "test-key", "typesafe/jev-1.13")
}

/// A gate-faithful verdict body with a chosen `contextual` flag and severity.
fn verdict_body(severity: f64, contextual: f64) -> serde_json::Value {
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
                "score": severity,
                "confidence": 0.9,
                "probabilities": {}
            },
            "actionable": {"type": "noul", "noul": 0.9},
            "transient": {"type": "noul", "noul": 0.1},
            "security": {"type": "noul", "noul": 0.1},
            "contextual": {"type": "noul", "noul": contextual}
        },
        "usage": {"input_tokens": 100, "output_tokens": 20}
    })
}

fn merge_body(noul: f64) -> serde_json::Value {
    serde_json::json!({
        "model": "typesafe/jev-1.13-20260917",
        "answers": {"same_event": {"type": "noul", "noul": noul}},
        "usage": {"input_tokens": 50, "output_tokens": 5}
    })
}

fn check_body(matters_now: f64, wake_someone: f64) -> serde_json::Value {
    serde_json::json!({
        "model": "typesafe/jev-1.13-20260917",
        "answers": {
            "matters_now": {"type": "noul", "noul": matters_now},
            "correlates_with": {
                "type": "choice",
                "choice": "none",
                "confidence": 0.9,
                "probabilities": {}
            },
            "wake_someone": {"type": "noul", "noul": wake_someone}
        },
        "usage": {"input_tokens": 200, "output_tokens": 10}
    })
}

fn raw_event(line: &'static str, host: &'static str) -> oarfish_core::Event {
    oarfish_core::Event::new(Bytes::from_static(line.as_bytes()), host, Source::Syslog)
}

async fn wait_for_merge(verdicts: &Verdicts, a: &TemplateId, b: &TemplateId) {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(15);
    loop {
        if verdicts.merges().lookup(a, b).is_some() {
            return;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "timed out waiting for the merge"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

async fn wait_for_verdict(verdicts: &Verdicts, id: &TemplateId, template: &str) {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(15);
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

async fn next_change(rx: &mut tokio::sync::broadcast::Receiver<AlarmChange>) -> AlarmChange {
    tokio::time::timeout(Duration::from_secs(15), rx.recv())
        .await
        .expect("a change arrives")
        .expect("sender lives")
}

/// A close pair is judged once: two raw lines differing by an optional field
/// cluster apart, the pipeline refers them, the merge judge asks once, and
/// the two halves then raise one alarm on the survivor.
#[tokio::test]
async fn a_close_pair_is_judged_once_and_raises_once() {
    let dir = tempdir();
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(body_string_contains("actionable"))
        .respond_with(ResponseTemplate::new(200).set_body_json(verdict_body(1.0, 0.1)))
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(body_string_contains("same_event"))
        .respond_with(ResponseTemplate::new(200).set_body_json(merge_body(0.91)))
        .mount(&server)
        .await;

    let verdicts = Arc::new(
        Verdicts::open(
            &dir,
            decide_client(&server),
            oarfish_engine::static_questions(),
            oarfish_engine::merge_questions(),
            oarfish_mask::curated().hash(),
        )
        .expect("open"),
    );
    let engine = Engine::new(
        Arc::clone(&verdicts),
        decide_client(&server),
        EngineConfig {
            silence: Duration::from_secs(300),
            flap_cooldown: Duration::from_secs(600),
            ..EngineConfig::default()
        },
    );
    let mut changes = engine.subscribe();

    let cancel = CancellationToken::new();
    let (ingest_tx, ingest_rx) = mpsc::channel(1024);
    let (engine_tx, engine_rx) = mpsc::channel(1024);
    let pipeline = oarfish_ingest::Pipeline::new(
        oarfish_mask::curated().clone(),
        oarfish_drain::Drain::new(oarfish_drain::Config::default()).expect("drain"),
    )
    .with_merges(verdicts.merges_handle());
    let pipe_handle = tokio::spawn(pipeline.run_forwarding(ingest_rx, engine_tx));
    let engine_handle = tokio::spawn(engine.run(engine_rx, cancel.child_token()));

    // One real event split in two by an optional field.
    const BASE: &str = "backup done files ok size mb";
    const EXTENDED: &str = "backup done files ok size mb extra";
    let masked = |line: &str| oarfish_mask::curated().mask(line).template().to_owned();
    let (ta, tb) = (masked(BASE), masked(EXTENDED));
    assert_ne!(ta, tb);
    let (a, b) = (TemplateId::of(&ta), TemplateId::of(&tb));
    let (lo, lo_text) = if a <= b {
        (a, ta.clone())
    } else {
        (b, tb.clone())
    };

    ingest_tx
        .send(raw_event(BASE, "nas01"))
        .await
        .expect("send");
    ingest_tx
        .send(raw_event(EXTENDED, "nas01"))
        .await
        .expect("send");

    // Judged once: the pair, and each half's verdict.
    wait_for_merge(&verdicts, &a, &b).await;
    wait_for_verdict(&verdicts, &a, &ta).await;
    wait_for_verdict(&verdicts, &b, &tb).await;
    // The survivor is decided too — it is one of the halves, already judged.
    assert!(verdicts.verdict_for(&lo, &lo_text).is_some());

    // Six alternating events: two halves at half rate each, one window, one
    // alarm on the survivor.
    for i in 0..6 {
        let line = if i % 2 == 0 { BASE } else { EXTENDED };
        ingest_tx
            .send(raw_event(line, "nas01"))
            .await
            .expect("send");
    }
    let mut raised = 0;
    let mut updated = 0;
    for _ in 0..3 {
        match next_change(&mut changes).await {
            AlarmChange::Raised(alarm) => {
                raised += 1;
                assert_eq!(alarm.template_id, lo, "the survivor carries the alarm");
                assert_eq!(alarm.host, "nas01");
            }
            AlarmChange::Updated(_) => updated += 1,
            other => panic!("expected Raised/Updated, got {other:?}"),
        }
    }
    assert_eq!((raised, updated), (1, 2), "two halves, one alarm");

    cancel.cancel();
    drop(ingest_tx);
    tokio::time::timeout(Duration::from_secs(10), pipe_handle)
        .await
        .expect("pipeline drains")
        .expect("join");
    tokio::time::timeout(Duration::from_secs(10), engine_handle)
        .await
        .expect("engine drains")
        .expect("join");
    match Arc::try_unwrap(verdicts) {
        Ok(verdicts) => verdicts.close().await.expect("close"),
        Err(verdicts) => verdicts.persist().expect("persist"),
    }
    let _ = std::fs::remove_dir_all(&dir);
}

/// A flagged burst resolves to `wake_someone` and lands in `Lane::Page`:
/// the verdict flags the template, the window trips, the check answers
/// 0.94, and the raise carries the page lane with a persisted record behind
/// it.
#[tokio::test]
async fn a_flagged_burst_resolves_to_wake_someone_and_lands_in_page() {
    let dir = tempdir();
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(body_string_contains("actionable"))
        .respond_with(ResponseTemplate::new(200).set_body_json(verdict_body(2.0, 0.9)))
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(body_string_contains("matters_now"))
        .respond_with(ResponseTemplate::new(200).set_body_json(check_body(0.9, 0.94)))
        .mount(&server)
        .await;

    let verdicts = Arc::new(
        Verdicts::open(
            &dir,
            decide_client(&server),
            oarfish_engine::static_questions(),
            oarfish_engine::merge_questions(),
            oarfish_mask::curated().hash(),
        )
        .expect("open"),
    );
    let engine = Engine::new(
        Arc::clone(&verdicts),
        decide_client(&server),
        EngineConfig {
            silence: Duration::from_secs(300),
            flap_cooldown: Duration::from_secs(600),
            ..EngineConfig::default()
        },
    );
    let mut changes = engine.subscribe();

    let cancel = CancellationToken::new();
    let (ingest_tx, ingest_rx) = mpsc::channel(1024);
    let (engine_tx, engine_rx) = mpsc::channel(1024);
    let pipeline = oarfish_ingest::Pipeline::new(
        oarfish_mask::curated().clone(),
        oarfish_drain::Drain::new(oarfish_drain::Config::default()).expect("drain"),
    );
    let pipe_handle = tokio::spawn(pipeline.run_forwarding(ingest_rx, engine_tx));
    let engine_handle = tokio::spawn(engine.run(engine_rx, cancel.child_token()));

    const LINE: &str = "acceptance flagged burst line alpha";
    let template = oarfish_mask::curated().mask(LINE).template().to_owned();
    let template_id = TemplateId::of(&template);

    ingest_tx
        .send(raw_event(LINE, "acc01"))
        .await
        .expect("send");
    wait_for_verdict(&verdicts, &template_id, &template).await;

    // A major template bursts: five in five minutes trips the window, the
    // template is flagged, and the raise waits for the check.
    for _ in 0..5 {
        ingest_tx
            .send(raw_event(LINE, "acc01"))
            .await
            .expect("send");
    }
    match next_change(&mut changes).await {
        AlarmChange::Raised(alarm) => {
            assert_eq!(alarm.lane, Lane::Page);
            assert_eq!(alarm.host, "acc01");
            assert_eq!(alarm.template, template);
        }
        other => panic!("expected the checked raise, got {other:?}"),
    }
    // Recorded before trusted: the check's decision record persists.
    assert!(!verdicts.records_for_template(&template_id).is_empty());

    cancel.cancel();
    drop(ingest_tx);
    tokio::time::timeout(Duration::from_secs(10), pipe_handle)
        .await
        .expect("pipeline drains")
        .expect("join");
    tokio::time::timeout(Duration::from_secs(10), engine_handle)
        .await
        .expect("engine drains")
        .expect("join");
    match Arc::try_unwrap(verdicts) {
        Ok(verdicts) => verdicts.close().await.expect("close"),
        Err(verdicts) => verdicts.persist().expect("persist"),
    }
    let _ = std::fs::remove_dir_all(&dir);
}
