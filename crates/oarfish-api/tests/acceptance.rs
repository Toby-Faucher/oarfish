//! M5 acceptance: an alarm raises, dedupes, clears, and every transition
//! arrives over SSE.
//!
//! The full stack in one test — pipeline, engine, API — against a wiremock
//! Jev that judges the probe template critical and actionable. Real time
//! throughout with short intervals: the engine's silence is two seconds, and
//! the judge answers on localhost in milliseconds.

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use bytes::Bytes;
use futures::StreamExt;
use oarfish_core::{Source, TemplateId};
use oarfish_engine::{AlarmChange, Engine, EngineConfig};
use oarfish_jev::Client;
use oarfish_store::Verdicts;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;
use wiremock::matchers::method;
use wiremock::{Mock, MockServer, ResponseTemplate};

static COUNTER: AtomicU64 = AtomicU64::new(0);

fn tempdir() -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "oarfish-m5-acc-{}-{}",
        std::process::id(),
        COUNTER.fetch_add(1, Ordering::SeqCst)
    ));
    std::fs::create_dir_all(&dir).expect("create tempdir");
    dir
}

const RAW_LINE: &str = "acceptance probe line alpha";

/// The masked text the pipeline will assign, computed with the same bundle.
/// Identical raw lines always share one cluster, so the verdict the judge
/// returns covers every probe event.
fn probe_template() -> String {
    oarfish_mask::curated().mask(RAW_LINE).template().to_owned()
}

/// A gate-faithful verdict for the probe: critical and actionable, so the
/// first judged sighting bypasses the window and raises.
fn decision_body() -> serde_json::Value {
    serde_json::json!({
        "model": "typesafe/jev-1.13-20260917",
        "answers": {
            "kind": {
                "type": "choice",
                "choice": "software",
                "confidence": 0.93,
                "probabilities": {"hardware_fault": 0.02, "software_error": 0.93, "config": 0.02, "security": 0.02, "routine": 0.01}
            },
            "severity": {
                "type": "score",
                "score": 3.0,
                "confidence": 0.9,
                "legend": {"0": "noise", "1": "degraded", "2": "broken", "3": "down"},
                "probabilities": {"3": 0.9, "2": 0.1}
            },
            "actionable": {"type": "noul", "noul": 0.9},
            "transient": {"type": "noul", "noul": 0.1},
            "security": {"type": "noul", "noul": 0.1},
            "contextual": {"type": "noul", "noul": 0.2}
        },
        "usage": {"input_tokens": 100, "output_tokens": 20}
    })
}

fn probe_event() -> oarfish_core::Event {
    oarfish_core::Event::new(
        Bytes::from_static(b"acceptance probe line alpha"),
        "acc01",
        Source::Syslog,
    )
}

/// Wait until the background judge lands the verdict for the probe.
async fn wait_for_verdict(verdicts: &Verdicts, id: &TemplateId, template: &str) {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(15);
    loop {
        if verdicts.verdict_for(id, template).is_some() {
            return;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "timed out waiting for the probe verdict"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

async fn next_change(rx: &mut tokio::sync::broadcast::Receiver<AlarmChange>) -> AlarmChange {
    tokio::time::timeout(Duration::from_secs(10), rx.recv())
        .await
        .expect("a change arrives")
        .expect("sender lives")
}

/// Read the SSE stream until a `cleared` event arrives, returning every
/// `(event, data)` pair seen. Comment-only keep-alive frames carry no event
/// and are skipped.
async fn collect_sse(url: String) -> Vec<(String, String)> {
    let response = reqwest::get(&url).await.expect("sse connects");
    assert_eq!(response.status(), 200);
    let mut stream = response.bytes_stream();
    let mut buf = Vec::new();
    let mut out = Vec::new();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.expect("chunk");
        buf.extend_from_slice(&chunk);
        while let Some(len) = frame_len(&buf) {
            let frame: Vec<u8> = buf.drain(..len).collect();
            let text = String::from_utf8(frame).expect("sse is text");
            let mut event = None;
            let mut data = None;
            for line in text.lines() {
                if let Some(rest) = line.strip_prefix("event:") {
                    event = Some(rest.trim().to_owned());
                } else if let Some(rest) = line.strip_prefix("data:") {
                    data = Some(rest.trim().to_owned());
                }
            }
            if let (Some(event), Some(data)) = (event, data) {
                let done = event == "cleared";
                out.push((event, data));
                if done {
                    return out;
                }
            }
        }
    }
    out
}

/// Length in bytes of the first complete SSE frame in `buf`, if any. axum
/// terminates frames with a blank line.
fn frame_len(buf: &[u8]) -> Option<usize> {
    buf.windows(2)
        .position(|window| window == b"\n\n")
        .map(|pos| pos + 2)
}

#[tokio::test]
async fn an_alarm_raises_dedupes_clears_and_every_transition_reaches_sse() {
    let dir = tempdir();
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200).set_body_json(decision_body()))
        .mount(&server)
        .await;

    let verdicts = Arc::new(
        Verdicts::open(
            &dir,
            Client::new(server.uri(), "test-key", "typesafe/jev-1.13"),
            oarfish_engine::static_questions(),
        )
        .expect("open"),
    );
    let engine = Engine::new(
        Arc::clone(&verdicts),
        EngineConfig {
            silence: Duration::from_secs(2),
            flap_cooldown: Duration::from_secs(2),
            ..EngineConfig::default()
        },
    );
    let template = probe_template();
    let template_id = TemplateId::of(&template);

    let cancel = CancellationToken::new();
    let api_state = oarfish_api::ApiState::new(
        engine.snapshot_handle(),
        engine.sender(),
        None,
        cancel.child_token(),
    );
    let mut changes = engine.subscribe();

    let (ingest_tx, ingest_rx) = mpsc::channel(1024);
    let (engine_tx, engine_rx) = mpsc::channel(1024);

    let pipeline = oarfish_ingest::Pipeline::new(
        oarfish_mask::curated().clone(),
        oarfish_drain::Drain::new(oarfish_drain::Config::default()).expect("drain"),
    );
    let pipe_handle = tokio::spawn(pipeline.run_forwarding(ingest_rx, engine_tx));
    let engine_handle = tokio::spawn(engine.run(engine_rx, cancel.child_token()));

    // Bind the real listener up front and hand it to `serve`: no port-reuse
    // gap, and the port is known without re-binding.
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind");
    let port = listener.local_addr().expect("addr").port();
    let api_handle = tokio::spawn(oarfish_api::serve_on_listener(
        api_state,
        listener,
        cancel.child_token(),
    ));
    // Poll until the server accepts: readiness is a connection, not a sleep.
    let alarms_url = format!("http://127.0.0.1:{port}/api/alarms");
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    loop {
        if reqwest::get(&alarms_url).await.is_ok() {
            break;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "timed out waiting for the API to accept"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }

    let sse_handle = tokio::spawn(collect_sse(format!(
        "http://127.0.0.1:{port}/api/alarms/stream"
    )));

    // Unjudged: enqueues the judge, raises nothing.
    ingest_tx.send(probe_event()).await.expect("send");
    wait_for_verdict(&verdicts, &template_id, &template).await;

    // Judged critical: the second sighting raises on first sight.
    ingest_tx.send(probe_event()).await.expect("send");
    let raised = next_change(&mut changes).await;
    let alarm_id = match raised {
        AlarmChange::Raised(ref alarm) => {
            assert_eq!(alarm.host, "acc01");
            assert_eq!(alarm.template, template);
            alarm.id
        }
        other => panic!("expected Raised, got {other:?}"),
    };

    // The open alarm reads back as JSON.
    let open: serde_json::Value = reqwest::get(format!("http://127.0.0.1:{port}/api/alarms"))
        .await
        .expect("get alarms")
        .json()
        .await
        .expect("json");
    assert_eq!(open.as_array().expect("array").len(), 1);
    assert_eq!(open[0]["host"], serde_json::json!("acc01"));

    // Same pair: a count bump, not a second alarm. The count folds events
    // for this host: one at raise, plus this one.
    ingest_tx.send(probe_event()).await.expect("send");
    match next_change(&mut changes).await {
        AlarmChange::Updated(alarm) => {
            assert_eq!(alarm.id, alarm_id);
            assert_eq!(alarm.count, 2);
        }
        other => panic!("expected Updated, got {other:?}"),
    }

    // Silence past the two-second interval clears.
    match tokio::time::timeout(Duration::from_secs(10), changes.recv()).await {
        Ok(Ok(AlarmChange::Cleared(cleared))) => assert_eq!(cleared, alarm_id),
        other => panic!("expected Cleared, got {other:?}"),
    }

    // And every transition arrived over SSE, in order.
    let sse = tokio::time::timeout(Duration::from_secs(10), sse_handle)
        .await
        .expect("sse finishes")
        .expect("join");
    let kinds: Vec<&str> = sse.iter().map(|(event, _)| event.as_str()).collect();
    assert_eq!(kinds, vec!["raised", "updated", "cleared"], "got {sse:?}");

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
    tokio::time::timeout(Duration::from_secs(10), api_handle)
        .await
        .expect("api stops")
        .expect("join")
        .expect("serve");
    match Arc::try_unwrap(verdicts) {
        Ok(verdicts) => verdicts.close().await.expect("close"),
        Err(verdicts) => verdicts.persist().expect("persist"),
    }
    let _ = std::fs::remove_dir_all(&dir);
}
