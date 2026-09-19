//! M5.5 §9: a pair differing by an optional field is referred, and wiremock
//! sees the request. The full referral path in one test — pipeline, Drain
//! neighbours, merge judge — with a `no`-repeat proving candidate scanning
//! runs only when a genuinely new cluster appears.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use oarfish_core::{Source, TemplateId};
use oarfish_ingest::Pipeline;
use oarfish_jev::{Client, Question};
use oarfish_store::Verdicts;
use wiremock::matchers::{body_string_contains, method};
use wiremock::{Mock, MockServer, ResponseTemplate};

static COUNTER: AtomicU64 = AtomicU64::new(0);

fn tempdir() -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "oarfish-refer-test-{}-{}",
        std::process::id(),
        COUNTER.fetch_add(1, Ordering::SeqCst)
    ));
    std::fs::create_dir_all(&dir).expect("create tempdir");
    dir
}

fn event(raw: &'static str) -> oarfish_core::Event {
    oarfish_core::Event::new(
        bytes::Bytes::from_static(raw.as_bytes()),
        "nas01",
        Source::Syslog,
    )
}

#[tokio::test]
async fn an_optional_field_is_referred_and_wiremock_sees_the_request() {
    let dir = tempdir();
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(body_string_contains("same_event"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "model": "typesafe/jev-1.13-20260917",
            "answers": {"same_event": {"type": "noul", "noul": 0.91}},
            "usage": {"input_tokens": 50, "output_tokens": 5}
        })))
        .expect(1)
        .mount(&server)
        .await;

    let verdicts = Arc::new(
        Verdicts::open(
            &dir,
            Client::new(server.uri(), "test-key", "typesafe/jev-1.13"),
            BTreeMap::from([(
                "actionable".to_owned(),
                Question::noul(serde_json::json!("Can a human do anything?"), None),
            )]),
            BTreeMap::from([(
                oarfish_store::MERGE_QUESTION.to_owned(),
                Question::noul(
                    serde_json::json!("Do these describe the same event type?"),
                    None,
                ),
            )]),
            oarfish_mask::BundleHash::from_bytes([7u8; 32]),
        )
        .expect("open"),
    );
    let mut pipeline = Pipeline::new(
        oarfish_mask::curated().clone(),
        oarfish_drain::Drain::new(oarfish_drain::Config::default()).expect("drain"),
    )
    .with_merges(verdicts.merges_handle());

    // One real event split in two by an optional field: different token
    // counts, so Drain never compares them — the referral crosses counts on
    // purpose. Six tokens plus the optional seventh: 6/7 inside the band.
    let base = "backup done files ok size mb";
    let extended = "backup done files ok size mb extra";
    let masked_base = oarfish_mask::curated().mask(base).template().to_owned();
    let masked_extended = oarfish_mask::curated().mask(extended).template().to_owned();
    assert_ne!(masked_base, masked_extended);
    let a = TemplateId::of(&masked_base);
    let b = TemplateId::of(&masked_extended);

    let (first, _) = pipeline.assign(&event(base));
    let (second, _) = pipeline.assign(&event(extended));
    assert_ne!(
        first.seq, second.seq,
        "different counts never share a cluster"
    );
    assert_eq!(first.size, 1);
    assert_eq!(second.size, 1);

    // The pair is judged once through the merge review.
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    let decision = loop {
        if let Some(decision) = verdicts.merges().lookup(&a, &b) {
            break decision;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "timed out waiting for the referred pair"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    };
    assert!(decision.merged);

    // Repeats join their clusters — sizes pass 1, nothing new appears — so
    // no second referral fires. Candidate scanning runs only when a
    // genuinely new cluster appears.
    pipeline.assign(&event(base));
    pipeline.assign(&event(extended));
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(
        server.received_requests().await.unwrap_or_default().len(),
        1,
        "one pair, one request"
    );

    match Arc::try_unwrap(verdicts) {
        Ok(verdicts) => verdicts.close().await.expect("close"),
        Err(verdicts) => verdicts.persist().expect("persist"),
    }
    let _ = std::fs::remove_dir_all(&dir);
}
