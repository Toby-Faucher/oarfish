//! Client for the TypeSafe System One API (Jev), via OpenRouter.
//!
//! Reached through the decisions endpoint
//! (`https://openrouter.ai/api/alpha/decisions`, model `typesafe/jev-1.13`,
//! bearer auth via `OPENROUTER_API_KEY`) rather than the native TypeSafe
//! endpoint: one key for every model oarfish calls, and it keeps a local-model
//! fallback a config change rather than a second client.
//!
//! **Not `chat/completions`** — it rejects decisions models outright. The body
//! is the native shape `{model, state, questions}`, passed through unmapped,
//! and the answers come back with calibrated `probabilities` and `confidence`.
//!
//! The call takes a `state` plus a map of typed questions and returns typed
//! answers. Questions in one request are evaluated in parallel, so ask
//! everything at once — a sixth question is close to free. The request budget
//! is ~32k tokens shared between state and questions.
//!
//! Confidence is read from the provider payload, never synthesized: the alarm
//! engine routes on it, and an invented number is worse than none. This is
//! enforced by the types — `confidence` is a required `f64`, never an
//! `Option` — so a payload without one fails to parse rather than defaulting.
//! (`noul` carries no separate confidence on the wire; its value *is* the
//! probability, so it has no confidence field to require.)
//!
//! Models the three primitives: `choice`, `score`, `noul`.
//!
//! This crate is a transport. It holds no policy about *which* questions to
//! ask; that lives in `oarfish-engine`. `Decision` is the wire: what the
//! provider returned. `Verdict`, the domain type the rest of oarfish stores
//! and renders, lives in `oarfish-core` and is mapped to on the way in.

#![forbid(unsafe_code)]

use std::collections::BTreeMap;

use oarfish_core::QuestionsHash;

/// The default endpoint. Overridden in tests to point at wiremock.
pub const DEFAULT_BASE_URL: &str = "https://openrouter.ai/api/alpha/decisions";

/// The pinned model id to request. The provider answers with a dated build
/// underneath it, which is recorded on every decision rather than assumed.
pub const DEFAULT_MODEL: &str = "typesafe/jev-1.13";

/// Which primitive a question asks for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum QuestionType {
    Choice,
    Score,
    Noul,
}

/// One typed question.
///
/// `instructions` accepts a string or a structured object — the wire allows
/// both — so it is carried as a [`serde_json::Value`]. `criteria` maps each
/// option to what it means for `choice`, lists the ordered rubric levels for
/// `score`, and is an optional clarification for `noul`; it is likewise
/// carried as a value so the transport never constrains the policy.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Question {
    #[serde(rename = "type")]
    pub kind: QuestionType,
    pub instructions: serde_json::Value,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub criteria: Option<serde_json::Value>,
}

impl Question {
    /// A `choice` question over the given criteria object.
    pub fn choice(
        instructions: impl Into<serde_json::Value>,
        criteria: impl Into<serde_json::Value>,
    ) -> Self {
        Self {
            kind: QuestionType::Choice,
            instructions: instructions.into(),
            criteria: Some(criteria.into()),
        }
    }

    /// A `score` question over the given ordered rubric levels.
    pub fn score(
        instructions: impl Into<serde_json::Value>,
        criteria: impl Into<serde_json::Value>,
    ) -> Self {
        Self {
            kind: QuestionType::Score,
            instructions: instructions.into(),
            criteria: Some(criteria.into()),
        }
    }

    /// A `noul` (yes/no) question. `criteria` is an optional clarification.
    pub fn noul(
        instructions: impl Into<serde_json::Value>,
        criteria: Option<serde_json::Value>,
    ) -> Self {
        Self {
            kind: QuestionType::Noul,
            instructions: instructions.into(),
            criteria,
        }
    }
}

/// One typed answer, in the wire's shape.
///
/// `confidence` is a required `f64` on `choice` and `score`: a payload
/// without one is a parse error, not a default. `noul` carries no separate
/// confidence — its `noul` value is P(yes) — so reading `.confidence` off
/// every answer would break on binaries; match instead.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum Answer {
    Choice {
        choice: String,
        confidence: f64,
        probabilities: BTreeMap<String, f64>,
    },
    Score {
        score: f64,
        confidence: f64,
        probabilities: BTreeMap<String, f64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        legend: Option<BTreeMap<String, String>>,
    },
    Noul {
        noul: f64,
    },
}

impl Answer {
    /// Map to the domain shape in `oarfish-core`. The content is carried
    /// across unchanged — including chosen confidences — so routing downstream
    /// always sees the provider's number.
    pub fn into_verdict_answer(self) -> oarfish_core::VerdictAnswer {
        match self {
            Answer::Choice {
                choice,
                confidence,
                probabilities,
            } => oarfish_core::VerdictAnswer::Choice {
                choice,
                confidence,
                probabilities,
            },
            Answer::Score {
                score,
                confidence,
                probabilities,
                legend,
            } => oarfish_core::VerdictAnswer::Score {
                score,
                confidence,
                probabilities,
                legend,
            },
            Answer::Noul { noul } => oarfish_core::VerdictAnswer::Noul { noul },
        }
    }
}

/// Token usage for one call. `cost` rides along when the provider reports it
/// and is `None` when it does not; it is accounting, never routing input.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Usage {
    pub input_tokens: u64,
    pub output_tokens: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cost: Option<f64>,
}

/// What the provider returned: the answers, the resolved model id, and usage.
///
/// `model` is the dated build that actually answered (e.g.
/// `typesafe/jev-1.13-20260917`), not the requested pin. It is recorded in
/// every decision record so behaviour moving overnight is visible in the
/// replay trail.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Decision {
    pub answers: BTreeMap<String, Answer>,
    pub model: String,
    #[serde(default)]
    pub usage: Option<Usage>,
}

/// What can go wrong on the one call this crate makes.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// The transport failed before a response was read.
    #[error("jev request failed: {0}")]
    Transport(#[from] reqwest::Error),
    /// The provider answered with an error status.
    #[error("jev rejected the request with {status}: {message}")]
    Api {
        status: reqwest::StatusCode,
        message: String,
    },
    /// The payload did not parse — including a missing `confidence`, which is
    /// a parse error by design rather than a default.
    #[error("jev returned an unreadable payload: {0}")]
    Parse(String),
    /// `OPENROUTER_API_KEY` was absent.
    #[error("OPENROUTER_API_KEY is not set")]
    MissingApiKey,
}

/// The one call, against the decisions endpoint.
#[derive(Debug, Clone)]
pub struct Client {
    http: reqwest::Client,
    base_url: String,
    api_key: String,
    model: String,
}

impl Client {
    /// Point at any decisions-compatible endpoint. Tests pass wiremock here.
    pub fn new(
        base_url: impl Into<String>,
        api_key: impl Into<String>,
        model: impl Into<String>,
    ) -> Self {
        Self {
            http: reqwest::Client::new(),
            base_url: base_url.into(),
            api_key: api_key.into(),
            model: model.into(),
        }
    }

    /// The production client: default endpoint and pinned model, key from the
    /// environment. The key comes from the environment only, never a file.
    pub fn from_env() -> Result<Self, Error> {
        let api_key = std::env::var("OPENROUTER_API_KEY").map_err(|_| Error::MissingApiKey)?;
        Ok(Self::new(DEFAULT_BASE_URL, api_key, DEFAULT_MODEL))
    }

    /// The requested model pin.
    pub fn model(&self) -> &str {
        &self.model
    }

    /// Ask everything in one request. `state` is any JSON — a string, an
    /// object of named fields, or a list — and `questions` maps caller-chosen
    /// ids to typed questions. All questions evaluate in parallel.
    pub async fn decide(
        &self,
        state: &serde_json::Value,
        questions: &BTreeMap<String, Question>,
    ) -> Result<Decision, Error> {
        let body = serde_json::json!({
            "model": self.model,
            "state": state,
            "questions": questions,
        });

        let response = self
            .http
            .post(&self.base_url)
            .bearer_auth(&self.api_key)
            .json(&body)
            .send()
            .await?;

        let status = response.status();
        if !status.is_success() {
            let message = response.text().await.unwrap_or_default();
            let message = message.chars().take(500).collect::<String>();
            return Err(Error::Api { status, message });
        }

        let bytes = response.bytes().await?;
        serde_json::from_slice::<Decision>(&bytes).map_err(|e| Error::Parse(e.to_string()))
    }
}

/// Hash a question set to the 16-byte [`QuestionsHash`] the verdict key
/// carries.
///
/// The input is canonicalised first — objects with sorted keys at every
/// level — so the hash depends on what was asked rather than on insertion
/// order. A reworded question misses the cache and re-judges.
pub fn questions_hash(questions: &BTreeMap<String, Question>) -> QuestionsHash {
    let value = serde_json::to_value(questions).expect("questions serialize");
    let canonical = canonical_value(&value);
    let bytes = serde_json::to_vec(&canonical).expect("canonical value serializes");
    QuestionsHash::of(&bytes)
}

/// Rebuild a JSON value with every object's keys in sorted order.
fn canonical_value(value: &serde_json::Value) -> serde_json::Value {
    match value {
        serde_json::Value::Object(map) => {
            let mut sorted = BTreeMap::new();
            for (key, val) in map {
                sorted.insert(key.clone(), canonical_value(val));
            }
            serde_json::Value::Object(sorted.into_iter().collect())
        }
        serde_json::Value::Array(items) => {
            serde_json::Value::Array(items.iter().map(canonical_value).collect())
        }
        other => other.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use wiremock::matchers::{header, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn questions() -> BTreeMap<String, Question> {
        BTreeMap::from([(
            "kind".to_owned(),
            Question::choice(
                serde_json::json!("Which kind of event is this?"),
                serde_json::json!({
                    "hardware": "A physical device failed",
                    "software": "A program erred",
                }),
            ),
        )])
    }

    fn decision_body() -> serde_json::Value {
        serde_json::json!({
            "model": "typesafe/jev-1.13-20260917",
            "answers": {
                "kind": {
                    "type": "choice",
                    "choice": "software",
                    "confidence": 0.93,
                    "probabilities": {"software": 0.9, "hardware": 0.1}
                }
            },
            "usage": {"input_tokens": 489, "output_tokens": 75}
        })
    }

    #[tokio::test]
    async fn decide_posts_the_native_shape_and_reads_the_wire_back() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/alpha/decisions"))
            .and(header("authorization", "Bearer test-key"))
            .respond_with(ResponseTemplate::new(200).set_body_json(decision_body()))
            .expect(1)
            .mount(&server)
            .await;

        let client = Client::new(
            format!("{}/api/alpha/decisions", server.uri()),
            "test-key",
            DEFAULT_MODEL,
        );
        let decision = client
            .decide(
                &serde_json::json!({"template": "task <VAR:NUM> failed"}),
                &questions(),
            )
            .await
            .expect("decide");

        assert_eq!(decision.model, "typesafe/jev-1.13-20260917");
        let answer = decision.answers.get("kind").expect("kind answer");
        match answer {
            Answer::Choice {
                choice,
                confidence,
                probabilities,
            } => {
                assert_eq!(choice, "software");
                assert_eq!(*confidence, 0.93);
                assert_eq!(probabilities.get("software"), Some(&0.9));
            }
            other => panic!("expected a choice, got {other:?}"),
        }
        let usage = decision.usage.expect("usage");
        assert_eq!(usage.input_tokens, 489);
        assert_eq!(usage.output_tokens, 75);
    }

    #[tokio::test]
    async fn a_payload_missing_confidence_fails_to_parse() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "model": "typesafe/jev-1.13-20260917",
                "answers": {
                    "kind": {
                        "type": "choice",
                        "choice": "software",
                        "probabilities": {"software": 0.9}
                    }
                },
                "usage": {"input_tokens": 1, "output_tokens": 1}
            })))
            .expect(1)
            .mount(&server)
            .await;

        let client = Client::new(server.uri(), "test-key", DEFAULT_MODEL);
        let err = client
            .decide(&serde_json::json!({}), &questions())
            .await
            .expect_err("missing confidence must fail");
        assert!(matches!(err, Error::Parse(_)), "got {err:?}");
    }

    #[tokio::test]
    async fn a_500_is_an_error_and_carries_the_status() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(500).set_body_string("overloaded"))
            .expect(1)
            .mount(&server)
            .await;

        let client = Client::new(server.uri(), "test-key", DEFAULT_MODEL);
        let err = client
            .decide(&serde_json::json!({}), &questions())
            .await
            .expect_err("500 must fail");
        match err {
            Error::Api { status, .. } => assert_eq!(status.as_u16(), 500),
            other => panic!("expected an Api error, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn noul_parses_without_a_confidence_field() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "model": "typesafe/jev-1.13-20260917",
                "answers": {
                    "actionable": {"type": "noul", "noul": 0.88}
                },
                "usage": {"input_tokens": 10, "output_tokens": 2}
            })))
            .expect(1)
            .mount(&server)
            .await;

        let client = Client::new(server.uri(), "test-key", DEFAULT_MODEL);
        let mut qs = BTreeMap::new();
        qs.insert(
            "actionable".to_owned(),
            Question::noul(serde_json::json!("Can a human do anything?"), None),
        );
        let decision = client
            .decide(&serde_json::json!({}), &qs)
            .await
            .expect("noul parses");
        assert_eq!(
            decision.answers.get("actionable"),
            Some(&Answer::Noul { noul: 0.88 })
        );
    }

    #[test]
    fn question_order_does_not_move_the_hash_but_a_rewording_does() {
        let mut first = questions();
        first.insert(
            "actionable".to_owned(),
            Question::noul(serde_json::json!("Can a human act?"), None),
        );
        let mut second = BTreeMap::new();
        second.insert(
            "actionable".to_owned(),
            Question::noul(serde_json::json!("Can a human act?"), None),
        );
        second.insert(
            "kind".to_owned(),
            Question::choice(
                serde_json::json!("Which kind of event is this?"),
                serde_json::json!({
                    "software": "A program erred",
                    "hardware": "A physical device failed",
                }),
            ),
        );
        assert_eq!(questions_hash(&first), questions_hash(&second));

        let mut reworded = first.clone();
        reworded.insert(
            "actionable".to_owned(),
            Question::noul(serde_json::json!("Can anyone do anything at all?"), None),
        );
        assert_ne!(questions_hash(&first), questions_hash(&reworded));
    }
}
