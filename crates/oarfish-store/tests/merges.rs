//! M5.5 acceptance: a close pair is judged once, both answers are cached,
//! and a changed question, bundle or model re-judges. Proven against
//! wiremock: tests never reach the real API, and the pair tests need *chosen*
//! `noul` values so a merge and a rejection are each provable.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use oarfish_core::TemplateId;
use oarfish_jev::{Client, Question};
use oarfish_mask::BundleHash;
use oarfish_store::{MergeDecision, Verdicts};
use wiremock::matchers::method;
use wiremock::{Mock, MockServer, ResponseTemplate};

static COUNTER: AtomicU64 = AtomicU64::new(0);

/// A fresh fjall home per test. `fjall` holds a lock per path, so sharing
/// would serialize — or fail — unrelated tests.
fn tempdir() -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "oarfish-merge-acc-{}-{}",
        std::process::id(),
        COUNTER.fetch_add(1, Ordering::SeqCst)
    ));
    std::fs::create_dir_all(&dir).expect("create tempdir");
    dir
}

fn cleanup(dir: &PathBuf) {
    let _ = std::fs::remove_dir_all(dir);
}

/// A pinned bundle identity. Every test judges under the same bundle, so the
/// cache hits; the rebundle test pins a second one.
fn bundle() -> BundleHash {
    BundleHash::from_bytes([7u8; 32])
}

/// The standing verdict question set. The merge tests never judge verdicts,
/// but the composition root takes both sets.
fn questions() -> BTreeMap<String, Question> {
    BTreeMap::from([(
        "actionable".to_owned(),
        Question::noul(
            serde_json::json!("Can a human do anything about this?"),
            None,
        ),
    )])
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

fn pair() -> (TemplateId, String, TemplateId, String) {
    let a = "backup done files ok size mb";
    let b = "backup done files ok size mb extra";
    (
        TemplateId::of(a),
        a.to_owned(),
        TemplateId::of(b),
        b.to_owned(),
    )
}

/// A merge-judge success payload answering `same_event` with a chosen value.
fn merge_body(noul: f64) -> serde_json::Value {
    serde_json::json!({
        "model": "typesafe/jev-1.13-20260917",
        "answers": {
            "same_event": {"type": "noul", "noul": noul}
        },
        "usage": {"input_tokens": 200, "output_tokens": 10}
    })
}

async fn mount_merge(server: &MockServer, noul: f64) {
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200).set_body_json(merge_body(noul)))
        .mount(server)
        .await;
}

/// Poll the synchronous read until the background merge judge lands the
/// decision for the pair.
async fn wait_for_decision(verdicts: &Verdicts, a: &TemplateId, b: &TemplateId) -> MergeDecision {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    loop {
        if let Some(decision) = verdicts.merges().lookup(a, b) {
            return decision;
        }
        if tokio::time::Instant::now() > deadline {
            panic!("timed out waiting for ({a}, {b}) to be judged");
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

async fn request_count(server: &MockServer) -> usize {
    server.received_requests().await.unwrap_or_default().len()
}

async fn open(dir: &PathBuf, server: &MockServer) -> Verdicts {
    Verdicts::open(
        dir,
        Client::new(server.uri(), "test-key", "typesafe/jev-1.13"),
        questions(),
        merge_questions(),
        bundle(),
    )
    .expect("open")
}

/// The milestone's first acceptance: a close pair is judged once. The second
/// sighting — `(B, A)` after `(A, B)`, the order the pipeline cannot
/// guarantee — fires zero further requests, asserted on wiremock's count.
#[tokio::test]
async fn a_close_pair_is_judged_once() {
    let dir = tempdir();
    let server = MockServer::start().await;
    mount_merge(&server, 0.91).await;

    let verdicts = open(&dir, &server).await;
    let (a, ta, b, tb) = pair();

    verdicts.merges().refer(a, &ta, b, &tb);
    let decision = wait_for_decision(&verdicts, &a, &b).await;
    assert!(decision.merged);
    assert_eq!(decision.model, "typesafe/jev-1.13-20260917");
    // The lower id always wins: the alias hooks the greater to the lesser.
    let (lo, hi) = if a <= b { (a, b) } else { (b, a) };
    assert_eq!(
        verdicts.merges().resolve(&hi),
        (lo, Some(decision.template_lo.clone()))
    );

    let count = request_count(&server).await;
    assert_eq!(count, 1, "one pair, one request");
    // The reversed sighting is the same row, already decided: no enqueue, no
    // request. The lookup inside `refer` is synchronous, so the count holds
    // immediately — no sleep, no flake.
    verdicts.merges().refer(b, &tb, a, &ta);
    verdicts.merges().refer(a, &ta, b, &tb);
    assert_eq!(request_count(&server).await, count);

    verdicts.close().await.expect("close");
    cleanup(&dir);
}

/// A `no` is cached too: a rejected pair is not re-asked after a restart,
/// when the moka cache is empty and only fjall remembers.
#[tokio::test]
async fn a_rejected_pair_is_not_reasked_after_a_restart() {
    let dir = tempdir();
    let server = MockServer::start().await;
    mount_merge(&server, 0.12).await;

    let (a, ta, b, tb) = pair();
    {
        let verdicts = open(&dir, &server).await;
        verdicts.merges().refer(a, &ta, b, &tb);
        let decision = wait_for_decision(&verdicts, &a, &b).await;
        assert!(!decision.merged);
        assert_eq!(request_count(&server).await, 1);
        verdicts.close().await.expect("close");
    }

    // Reopen over the same path: the alias table reloads (nothing merged),
    // and the rejection is still in fjall.
    let verdicts = open(&dir, &server).await;
    assert!(verdicts.merges().lookup(&a, &b).is_some());
    verdicts.merges().refer(a, &ta, b, &tb);
    verdicts.merges().refer(b, &tb, a, &ta);
    assert_eq!(
        request_count(&server).await,
        1,
        "a rejection must survive the restart without a re-ask"
    );
    // And nothing aliased: the pair stays two independent templates.
    assert_eq!(verdicts.merges().resolve(&a), (a, None));
    assert_eq!(verdicts.merges().resolve(&b), (b, None));

    verdicts.close().await.expect("close");
    cleanup(&dir);
}

/// A merged pair survives the restart as an alias: the second boot resolves
/// without judging.
#[tokio::test]
async fn a_merged_pair_resolves_after_a_restart_without_rejudging() {
    let dir = tempdir();
    let server = MockServer::start().await;
    mount_merge(&server, 0.91).await;

    let (a, ta, b, tb) = pair();
    let (lo, hi) = if a <= b { (a, b) } else { (b, a) };
    {
        let verdicts = open(&dir, &server).await;
        verdicts.merges().refer(a, &ta, b, &tb);
        wait_for_decision(&verdicts, &a, &b).await;
        verdicts.close().await.expect("close");
    }

    let verdicts = open(&dir, &server).await;
    let (resolved, _) = verdicts.merges().resolve(&hi);
    assert_eq!(resolved, lo);
    assert_eq!(request_count(&server).await, 1);

    verdicts.close().await.expect("close");
    cleanup(&dir);
}

/// Re-judge on change: a changed question, bundle or resolved model each
/// misses the key, so the next sighting re-judges rather than serving stale.
#[tokio::test]
async fn a_changed_question_bundle_or_model_each_misses_the_key() {
    let dir = tempdir();
    let server = MockServer::start().await;
    mount_merge(&server, 0.91).await;

    let verdicts = open(&dir, &server).await;
    let (a, ta, b, tb) = pair();
    let (lo, hi) = if a <= b { (a, b) } else { (b, a) };
    verdicts.merges().refer(a, &ta, b, &tb);
    wait_for_decision(&verdicts, &a, &b).await;
    let questions_hash = verdicts.merges().questions_hash();

    // Same key hits.
    assert!(
        verdicts
            .merges()
            .by_key(
                &lo,
                &hi,
                &questions_hash,
                &bundle(),
                "typesafe/jev-1.13-20260917"
            )
            .is_some()
    );
    // A reworded question, an edited bundle, a moved build: each misses.
    assert!(
        verdicts
            .merges()
            .by_key(
                &lo,
                &hi,
                &oarfish_core::QuestionsHash::of(b"reworded"),
                &bundle(),
                "typesafe/jev-1.13-20260917"
            )
            .is_none()
    );
    assert!(
        verdicts
            .merges()
            .by_key(
                &lo,
                &hi,
                &questions_hash,
                &BundleHash::from_bytes([8u8; 32]),
                "typesafe/jev-1.13-20260917"
            )
            .is_none()
    );
    assert!(
        verdicts
            .merges()
            .by_key(
                &lo,
                &hi,
                &questions_hash,
                &bundle(),
                "typesafe/jev-1.13-20260918"
            )
            .is_none()
    );

    verdicts.close().await.expect("close");
    cleanup(&dir);
}
