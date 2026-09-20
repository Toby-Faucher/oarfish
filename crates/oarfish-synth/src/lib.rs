//! Client for a general chat/completions model, via OpenRouter.
//!
//! `oarfish-jev` is transport only for the *decisions* endpoint — calibrated
//! `choice`/`score`/`noul` answers, and `chat/completions` rejects decisions
//! models outright. Mask synthesis needs the opposite shape: free-form text
//! in, free-form text out. This crate is that transport, holding no policy
//! about what the text says — that lives in `oarfish-mask::synth`.
//!
//! Depends on nothing else in the workspace: it hands back a raw string,
//! nothing more.

#![forbid(unsafe_code)]

/// The default endpoint. Overridden in tests to point at wiremock.
pub const DEFAULT_BASE_URL: &str = "https://openrouter.ai/api/v1/chat/completions";

/// What can go wrong on the one call this crate makes.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// The transport failed before a response was read.
    #[error("synthesis request failed: {0}")]
    Transport(#[from] reqwest::Error),
    /// The provider answered with an error status.
    #[error("synthesis request rejected with {status}: {message}")]
    Api {
        status: reqwest::StatusCode,
        message: String,
    },
    /// The payload did not parse as a chat/completions response.
    #[error("synthesis returned an unreadable payload: {0}")]
    Parse(String),
    /// `OPENROUTER_API_KEY` was absent.
    #[error("OPENROUTER_API_KEY is not set")]
    MissingApiKey,
}

#[derive(Debug, serde::Deserialize)]
struct ChatResponse {
    choices: Vec<Choice>,
}

#[derive(Debug, serde::Deserialize)]
struct Choice {
    message: Message,
}

#[derive(Debug, serde::Deserialize)]
struct Message {
    content: String,
}

/// The one call, against `chat/completions`.
#[derive(Debug, Clone)]
pub struct Client {
    http: reqwest::Client,
    base_url: String,
    api_key: String,
    model: String,
}

impl Client {
    /// Point at any chat/completions-compatible endpoint. Tests pass
    /// wiremock here.
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

    /// The production client: default endpoint, key from the environment.
    /// `model` has no default — the operator names one explicitly (spec §3).
    pub fn from_env(model: impl Into<String>) -> Result<Self, Error> {
        let api_key = std::env::var("OPENROUTER_API_KEY").map_err(|_| Error::MissingApiKey)?;
        Ok(Self::new(DEFAULT_BASE_URL, api_key, model))
    }

    /// The model this client asks.
    pub fn model(&self) -> &str {
        &self.model
    }

    /// Send one prompt, get back the model's raw text response.
    pub async fn synthesize(&self, prompt: &str) -> Result<String, Error> {
        let body = serde_json::json!({
            "model": self.model,
            "messages": [{"role": "user", "content": prompt}],
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
        let parsed: ChatResponse =
            serde_json::from_slice(&bytes).map_err(|e| Error::Parse(e.to_string()))?;
        let content = parsed
            .choices
            .into_iter()
            .next()
            .ok_or_else(|| Error::Parse("no choices in response".to_owned()))?
            .message
            .content;
        Ok(content)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use wiremock::matchers::{header, method};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn chat_body(content: &str) -> serde_json::Value {
        serde_json::json!({
            "choices": [{"message": {"role": "assistant", "content": content}}]
        })
    }

    #[tokio::test]
    async fn synthesize_posts_the_chat_shape_and_reads_the_content_back() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(header("authorization", "Bearer test-key"))
            .respond_with(ResponseTemplate::new(200).set_body_json(chat_body("[]")))
            .expect(1)
            .mount(&server)
            .await;

        let client = Client::new(server.uri(), "test-key", "some-model");
        let response = client
            .synthesize("propose some slots")
            .await
            .expect("synthesize");
        assert_eq!(response, "[]");
    }

    #[tokio::test]
    async fn a_500_is_an_error_and_carries_the_status() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(500).set_body_string("overloaded"))
            .expect(1)
            .mount(&server)
            .await;

        let client = Client::new(server.uri(), "test-key", "some-model");
        let err = client
            .synthesize("propose some slots")
            .await
            .expect_err("500 must fail");
        match err {
            Error::Api { status, .. } => assert_eq!(status.as_u16(), 500),
            other => panic!("expected an Api error, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn an_unreadable_payload_is_a_parse_error() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(serde_json::json!({"nope": true})),
            )
            .expect(1)
            .mount(&server)
            .await;

        let client = Client::new(server.uri(), "test-key", "some-model");
        let err = client
            .synthesize("propose some slots")
            .await
            .expect_err("must fail to parse");
        assert!(matches!(err, Error::Parse(_)), "got {err:?}");
    }
}
