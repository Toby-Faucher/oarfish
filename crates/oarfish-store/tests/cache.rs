//! M4 acceptance: a template is judged once and cached, proven against
//! wiremock. Tests never reach the real API — it bills, and the routing needs
//! *chosen* confidence values rather than whatever the model happens to say.
//!
//! The fixtures are transcribed from the payload the gate actually captured,
//! including the dated `model` field, so they are a record of the wire rather
//! than a guess at it.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use oarfish_core::{TemplateId, Verdict, VerdictAnswer};
use oarfish_jev::{Client, Question};
use oarfish_mask::BundleHash;
use oarfish_store::Verdicts;
use wiremock::matchers::method;
use wiremock::{Mock, MockServer, ResponseTemplate};

static COUNTER: AtomicU64 = AtomicU64::new(0);

/// A fresh fjall home per test. `fjall` holds a lock per path, so sharing
/// would serialize — or fail — unrelated tests.
fn tempdir() -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "oarfish-m4-test-{}-{}",
        std::process::id(),
        COUNTER.fetch_add(1, Ordering::SeqCst)
    ));
    std::fs::create_dir_all(&dir).expect("create tempdir");
    dir
}

fn cleanup(dir: &PathBuf) {
    let _ = std::fs::remove_dir_all(dir);
}

/// A pinned bundle identity. Every test judges under the same bundle, so
/// the cache hits; the rebundle test pins a second one.
fn bundle() -> BundleHash {
    BundleHash::from_bytes([7u8; 32])
}

/// The standing question set. Two questions so the confidence test can pin a
/// *chosen* value on each primitive that carries one.
fn questions() -> BTreeMap<String, Question> {
    BTreeMap::from([
        (
            "kind".to_owned(),
            Question::choice(
                serde_json::json!("Which kind of event is this template?"),
                serde_json::json!({
                    "hardware": "A physical device failed",
                    "software": "A program erred",
                    "security": "An authentication or access event",
                    "routine": "Normal operational noise"
                }),
            ),
        ),
        (
            "severity".to_owned(),
            Question::score(
                serde_json::json!("How severe is this template on its own?"),
                serde_json::json!([
                    "Routine noise; never page",
                    "Degraded but working around it",
                    "Broken; needs a human soon",
                    "Down or data at risk; wake someone"
                ]),
            ),
        ),
        (
            "actionable".to_owned(),
            Question::noul(
                serde_json::json!("Can a human do anything about this?"),
                None,
            ),
        ),
    ])
}

/// The standing merge question set: the single `noul` the merge judge reads
/// back, keyed by [`oarfish_store::MERGE_QUESTION`].
fn merge_questions() -> BTreeMap<String, Question> {
    BTreeMap::from([(
        oarfish_store::MERGE_QUESTION.to_owned(),
        Question::noul(
            serde_json::json!("Do these two log templates describe the same event type?"),
            None,
        ),
    )])
}

/// A gate-faithful success payload: dated model, chosen confidences,
/// token usage.
fn ok_body() -> serde_json::Value {
    serde_json::json!({
        "model": "typesafe/jev-1.13-20260917",
        "answers": {
            "kind": {
                "type": "choice",
                "choice": "software",
                "confidence": 0.93,
                "probabilities": {
                    "hardware": 0.02,
                    "software": 0.93,
                    "security": 0.03,
                    "routine": 0.02
                }
            },
            "severity": {
                "type": "score",
                "score": 2.1,
                "confidence": 0.61,
                "legend": {
                    "0": "Routine noise; never page",
                    "1": "Degraded but working around it",
                    "2": "Broken; needs a human soon",
                    "3": "Down or data at risk; wake someone"
                },
                "probabilities": {"0": 0.05, "1": 0.2, "2": 0.61, "3": 0.14}
            },
            "actionable": {"type": "noul", "noul": 0.88}
        },
        "usage": {"input_tokens": 489, "output_tokens": 75}
    })
}

async fn mount_ok(server: &MockServer, body: serde_json::Value) {
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200).set_body_json(body))
        .mount(server)
        .await;
}

/// Poll the synchronous read until the background judge lands the verdict.
async fn wait_for_verdict(
    verdicts: &Verdicts,
    template_id: &TemplateId,
    template: &str,
) -> Verdict {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    loop {
        if let Some(verdict) = verdicts.verdict_for(template_id, template) {
            return verdict;
        }
        if tokio::time::Instant::now() > deadline {
            panic!("timed out waiting for {template_id} to be judged");
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

async fn request_count(server: &MockServer) -> usize {
    server.received_requests().await.unwrap_or_default().len()
}

/// The milestone's acceptance: a second identical template fires zero
/// further requests, asserted on wiremock's request count.
#[tokio::test]
async fn judged_once_and_cached() {
    let dir = tempdir();
    let server = MockServer::start().await;
    mount_ok(&server, ok_body()).await;

    let verdicts = Verdicts::open(
        &dir,
        Client::new(server.uri(), "test-key", "typesafe/jev-1.13"),
        questions(),
        merge_questions(),
        bundle(),
    )
    .expect("open");

    let template = "EXT4-fs error (device <VAR:DEV>): inode #<VAR:NUM>";
    let id = TemplateId::of(template);
    assert!(verdicts.verdict_for(&id, template).is_none());

    let verdict = wait_for_verdict(&verdicts, &id, template).await;
    assert_eq!(verdict.template_id, id);

    for _ in 0..5 {
        assert!(verdicts.verdict_for(&id, template).is_some());
    }
    // Let any stray judge fire before counting.
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(request_count(&server).await, 1);

    verdicts.close().await.expect("close");
    cleanup(&dir);
}

/// Chosen confidences reach the cached verdict unchanged.
#[tokio::test]
async fn confidence_survives_the_round_trip() {
    let dir = tempdir();
    let server = MockServer::start().await;
    mount_ok(&server, ok_body()).await;

    let verdicts = Verdicts::open(
        &dir,
        Client::new(server.uri(), "test-key", "typesafe/jev-1.13"),
        questions(),
        merge_questions(),
        bundle(),
    )
    .expect("open");

    let template = "sshd[<VAR:NUM>]: Failed password for <VAR:USER>";
    let id = TemplateId::of(template);
    assert!(verdicts.verdict_for(&id, template).is_none());
    let verdict = wait_for_verdict(&verdicts, &id, template).await;

    match verdict.answers.get("kind") {
        Some(VerdictAnswer::Choice { confidence, .. }) => assert_eq!(*confidence, 0.93),
        other => panic!("expected the chosen 0.93, got {other:?}"),
    }
    match verdict.answers.get("severity") {
        Some(VerdictAnswer::Score { confidence, .. }) => assert_eq!(*confidence, 0.61),
        other => panic!("expected the chosen 0.61, got {other:?}"),
    }

    verdicts.close().await.expect("close");
    cleanup(&dir);
}

/// A payload missing `confidence` fails to parse and poisons nothing: no
/// verdict, no record, and the template stays re-enqueueable.
#[tokio::test]
async fn a_missing_confidence_never_synthesizes_one() {
    let dir = tempdir();
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "model": "typesafe/jev-1.13-20260917",
            "answers": {
                "kind": {
                    "type": "choice",
                    "choice": "software",
                    "probabilities": {"software": 0.9, "hardware": 0.1}
                }
            },
            "usage": {"input_tokens": 10, "output_tokens": 2}
        })))
        .mount(&server)
        .await;

    let verdicts = Verdicts::open(
        &dir,
        Client::new(server.uri(), "test-key", "typesafe/jev-1.13"),
        questions(),
        merge_questions(),
        bundle(),
    )
    .expect("open");

    let template = "task <VAR:NUM> failed";
    let id = TemplateId::of(template);
    assert!(verdicts.verdict_for(&id, template).is_none());
    tokio::time::sleep(Duration::from_secs(1)).await;

    assert!(verdicts.verdict_for(&id, template).is_none());
    assert!(verdicts.verdicts_for_template(&id).is_empty());
    assert!(verdicts.records_for_template(&id).is_empty());

    // Still re-enqueueable: the failure did not wedge the queue.
    assert!(verdicts.verdict_for(&id, template).is_none());
    tokio::time::sleep(Duration::from_secs(1)).await;
    assert!(request_count(&server).await >= 2);
    assert!(verdicts.verdict_for(&id, template).is_none());

    verdicts.close().await.expect("close");
    cleanup(&dir);
}

/// A reworded question set misses the disk cache and re-judges, even across
/// a restart against the same fjall home.
#[tokio::test]
async fn a_changed_question_set_rejudges() {
    let dir = tempdir();
    let server = MockServer::start().await;
    mount_ok(&server, ok_body()).await;

    let template = "task <VAR:NUM> failed";
    let id = TemplateId::of(template);

    let verdicts = Verdicts::open(
        &dir,
        Client::new(server.uri(), "test-key", "typesafe/jev-1.13"),
        questions(),
        merge_questions(),
        bundle(),
    )
    .expect("open");
    assert!(verdicts.verdict_for(&id, template).is_none());
    wait_for_verdict(&verdicts, &id, template).await;
    assert_eq!(request_count(&server).await, 1);
    verdicts.persist().expect("persist");
    verdicts.close().await.expect("close");

    let mut reworded = questions();
    reworded.insert(
        "actionable".to_owned(),
        Question::noul(
            serde_json::json!("Is there anything at all a human could do?"),
            None,
        ),
    );

    let verdicts = Verdicts::open(
        &dir,
        Client::new(server.uri(), "test-key", "typesafe/jev-1.13"),
        reworded,
        merge_questions(),
        bundle(),
    )
    .expect("reopen");
    // The old verdict is on disk under the old hash and must not serve.
    assert!(verdicts.verdict_for(&id, template).is_none());
    wait_for_verdict(&verdicts, &id, template).await;
    assert_eq!(request_count(&server).await, 2);

    verdicts.close().await.expect("close");
    cleanup(&dir);
}

/// An edited bundle misses the key: the old verdict is on disk under the
/// old bundle hash and must never serve for text another bundle produced.
#[tokio::test]
async fn an_edited_bundle_misses_the_key() {
    let dir = tempdir();
    let server = MockServer::start().await;
    mount_ok(&server, ok_body()).await;

    let template = "task <VAR:NUM> failed";
    let id = TemplateId::of(template);

    let verdicts = Verdicts::open(
        &dir,
        Client::new(server.uri(), "test-key", "typesafe/jev-1.13"),
        questions(),
        merge_questions(),
        bundle(),
    )
    .expect("open");
    wait_for_verdict(&verdicts, &id, template).await;
    verdicts.persist().expect("persist");
    verdicts.close().await.expect("close");

    let verdicts = Verdicts::open(
        &dir,
        Client::new(server.uri(), "test-key", "typesafe/jev-1.13"),
        questions(),
        merge_questions(),
        BundleHash::from_bytes([8u8; 32]),
    )
    .expect("reopen");
    // Same questions, same model, other bundle: a miss, then a re-judge.
    assert!(verdicts.verdict_for(&id, template).is_none());
    wait_for_verdict(&verdicts, &id, template).await;
    assert_eq!(request_count(&server).await, 2);

    verdicts.close().await.expect("close");
    cleanup(&dir);
}

/// The resolved model rides in the key: the same template under a moved
/// build misses, so a new build never aliases the old verdict.
#[tokio::test]
async fn a_changed_resolved_model_misses_the_key() {
    let dir = tempdir();
    let server = MockServer::start().await;
    mount_ok(&server, ok_body()).await;

    let verdicts = Verdicts::open(
        &dir,
        Client::new(server.uri(), "test-key", "typesafe/jev-1.13"),
        questions(),
        merge_questions(),
        bundle(),
    )
    .expect("open");

    let template = "task <VAR:NUM> failed";
    let id = TemplateId::of(template);
    let verdict = wait_for_verdict(&verdicts, &id, template).await;
    assert_eq!(verdict.model, "typesafe/jev-1.13-20260917");

    let hash = verdicts.questions_hash();
    assert!(
        verdicts
            .verdict_by_key(&id, &hash, &bundle(), "typesafe/jev-1.13-20260917")
            .is_some()
    );
    assert!(
        verdicts
            .verdict_by_key(&id, &hash, &bundle(), "typesafe/jev-1.13-20261001")
            .is_none()
    );
    // Same questions and model, other bundle: misses, never false-hits.
    assert!(
        verdicts
            .verdict_by_key(
                &id,
                &hash,
                &BundleHash::from_bytes([8u8; 32]),
                "typesafe/jev-1.13-20260917"
            )
            .is_none()
    );

    verdicts.close().await.expect("close");
    cleanup(&dir);
}

/// A 500 leaves the cache empty and the template re-enqueueable: once the
/// provider recovers, the same template judges without any restart.
#[tokio::test]
async fn failure_is_survivable_and_reenqueueable() {
    let dir = tempdir();
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(500).set_body_string("overloaded"))
        .mount(&server)
        .await;

    let verdicts = Verdicts::open(
        &dir,
        Client::new(server.uri(), "test-key", "typesafe/jev-1.13"),
        questions(),
        merge_questions(),
        bundle(),
    )
    .expect("open");

    let template = "task <VAR:NUM> failed";
    let id = TemplateId::of(template);
    assert!(verdicts.verdict_for(&id, template).is_none());
    tokio::time::sleep(Duration::from_secs(1)).await;

    assert!(verdicts.verdict_for(&id, template).is_none());
    assert!(verdicts.verdicts_for_template(&id).is_empty());
    assert!(verdicts.records_for_template(&id).is_empty());

    server.reset().await;
    mount_ok(&server, ok_body()).await;
    wait_for_verdict(&verdicts, &id, template).await;

    verdicts.close().await.expect("close");
    cleanup(&dir);
}

/// A record exists for every cached verdict; the reverse may not hold — a
/// crash between writes leaves an orphan record, which is harmless.
#[tokio::test]
async fn every_cached_verdict_has_a_record() {
    let dir = tempdir();
    let server = MockServer::start().await;
    mount_ok(&server, ok_body()).await;

    let verdicts = Verdicts::open(
        &dir,
        Client::new(server.uri(), "test-key", "typesafe/jev-1.13"),
        questions(),
        merge_questions(),
        bundle(),
    )
    .expect("open");

    let template = "task <VAR:NUM> failed";
    let id = TemplateId::of(template);
    wait_for_verdict(&verdicts, &id, template).await;

    let cached = verdicts.verdicts_for_template(&id);
    assert!(!cached.is_empty());
    let records = verdicts.records_for_template(&id);
    for verdict in &cached {
        assert!(
            records.iter().any(|record| record.model == verdict.model
                && record.questions_hash == verdict.questions_hash
                && record.template_id == verdict.template_id),
            "no record explains the cached verdict under {}",
            verdict.model
        );
    }

    verdicts.close().await.expect("close");
    cleanup(&dir);
}

/// A `template_id` prefix scan returns every verdict the template has held —
/// here one per question revision — oldest first.
#[tokio::test]
async fn replay_returns_every_verdict_the_template_has_held() {
    let dir = tempdir();
    let server = MockServer::start().await;
    mount_ok(&server, ok_body()).await;

    let template = "task <VAR:NUM> failed";
    let id = TemplateId::of(template);

    let verdicts = Verdicts::open(
        &dir,
        Client::new(server.uri(), "test-key", "typesafe/jev-1.13"),
        questions(),
        merge_questions(),
        bundle(),
    )
    .expect("open");
    let first = wait_for_verdict(&verdicts, &id, template).await;
    verdicts.persist().expect("persist");
    verdicts.close().await.expect("close");

    let mut reworded = questions();
    reworded.insert(
        "actionable".to_owned(),
        Question::noul(
            serde_json::json!("Is there anything at all a human could do?"),
            None,
        ),
    );
    let verdicts = Verdicts::open(
        &dir,
        Client::new(server.uri(), "test-key", "typesafe/jev-1.13"),
        reworded,
        merge_questions(),
        bundle(),
    )
    .expect("reopen");
    let second = wait_for_verdict(&verdicts, &id, template).await;
    assert_ne!(first.questions_hash, second.questions_hash);

    let history = verdicts.verdicts_for_template(&id);
    assert_eq!(history.len(), 2);
    assert!(history[0].judged_at <= history[1].judged_at);
    assert!(
        history
            .iter()
            .any(|v| v.questions_hash == first.questions_hash)
    );
    assert!(
        history
            .iter()
            .any(|v| v.questions_hash == second.questions_hash)
    );

    verdicts.close().await.expect("close");
    cleanup(&dir);
}
