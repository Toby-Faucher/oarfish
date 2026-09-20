//! Acceptance: a first-and-only sighting raises without a repeat, through
//! the real wiring — the pipeline, the store's background judge, the
//! notification channel, and the running engine loop, not just the
//! `on_judged` unit tested in `tests/engine.rs`.

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use bytes::Bytes;
use oarfish_core::{AlarmChange, Source};
use oarfish_engine::{Engine, EngineConfig};
use oarfish_jev::Client;
use oarfish_store::Verdicts;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;
use wiremock::matchers::method;
use wiremock::{Mock, MockServer, ResponseTemplate};

static COUNTER: AtomicU64 = AtomicU64::new(0);

fn tempdir() -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "oarfish-judged-acc-{}-{}",
        std::process::id(),
        COUNTER.fetch_add(1, Ordering::SeqCst)
    ));
    std::fs::create_dir_all(&dir).expect("create tempdir");
    dir
}

fn decide_client(server: &MockServer) -> Client {
    Client::new(server.uri(), "test-key", "typesafe/jev-1.13")
}

fn verdict_body(severity: f64) -> serde_json::Value {
    serde_json::json!({
        "model": "typesafe/jev-1.13-20260917",
        "answers": {
            "kind": {
                "type": "choice",
                "choice": "hardware_fault",
                "confidence": 0.93,
                "probabilities": {"hardware_fault": 0.93, "routine": 0.07}
            },
            "severity": {
                "type": "score",
                "score": severity,
                "confidence": 0.9,
                "probabilities": {}
            },
            "actionable": {"type": "noul", "noul": 0.9},
            "transient": {"type": "noul", "noul": 0.1},
            "security": {"type": "noul", "noul": 0.05},
            "contextual": {"type": "noul", "noul": 0.05}
        },
        "usage": {"input_tokens": 100, "output_tokens": 20}
    })
}

fn raw_event(line: &'static str, host: &'static str) -> oarfish_core::Event {
    oarfish_core::Event::new(Bytes::from_static(line.as_bytes()), host, Source::Syslog)
}

async fn next_change(rx: &mut tokio::sync::broadcast::Receiver<AlarmChange>) -> AlarmChange {
    tokio::time::timeout(Duration::from_secs(15), rx.recv())
        .await
        .expect("a change arrives")
        .expect("sender lives")
}

/// One line, sent exactly once, for a template judged critical: it raises
/// without a second sighting, through the real store-to-engine wiring —
/// `Judge` completing, `Verdicts::set_judged_notifier`'s channel, and the
/// running engine loop's own select branch, not a direct method call.
#[tokio::test]
async fn a_single_sighting_raises_through_the_real_wiring() {
    let dir = tempdir();
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200).set_body_json(verdict_body(3.0)))
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
    // The one line under test: wiring this in is the entire point of the
    // acceptance test. Without it, a single sighting could never raise.
    verdicts.set_judged_notifier(engine.judged_sender());
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

    // Exactly one line, never repeated.
    ingest_tx
        .send(raw_event(
            "kernel: EXT4-fs error (device sda1): a single unrepeated failure",
            "nas01",
        ))
        .await
        .expect("send");

    match next_change(&mut changes).await {
        AlarmChange::Raised(alarm) => {
            assert_eq!(alarm.host, "nas01");
            assert_eq!(alarm.severity, oarfish_core::Severity::Critical);
            assert_eq!(alarm.count, 1, "raised from the one and only sighting");
        }
        other => panic!("expected Raised, got {other:?}"),
    }

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
