//! Client for the TypeSafe System One API (Jev).
//!
//! Reached through OpenRouter (`https://openrouter.ai/api/v1`, model
//! `typesafe/jev-1.13`, `OPENROUTER_API_KEY`) rather than the native TypeSafe
//! endpoint: one key for every model oarfish calls, and it keeps a local-model
//! fallback a config change rather than a second client.
//!
//! The call takes a `state` plus a map of typed questions and returns typed
//! answers with calibrated probabilities. Questions in one request are
//! evaluated in parallel, so ask everything at once - a sixth question is close
//! to free.
//!
//! Confidence is read from the provider payload, never synthesized: the alarm
//! engine routes on it, and an invented number is worse than none.
//!
//! Models the three primitives: `Choice`, `Score`, `Noul`. The request budget
//! is ~32k tokens shared between state and questions.
//!
//! This crate is a transport. It holds no policy about *which* questions to
//! ask; that lives in `oarfish-engine`.

#![forbid(unsafe_code)]
