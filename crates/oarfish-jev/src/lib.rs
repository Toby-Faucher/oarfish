//! Client for the TypeSafe System One API (Jev).
//!
//! One endpoint: `POST https://api.typesafe.ai/v1/systemone`, taking a `state`
//! plus a map of typed questions, returning typed answers with calibrated
//! probabilities. Questions in one request are evaluated in parallel, so ask
//! everything at once - a sixth question is close to free.
//!
//! Models the three primitives: `Choice`, `Score`, `Noul`. The request budget
//! is ~32k tokens shared between state and questions.
//!
//! This crate is a transport. It holds no policy about *which* questions to
//! ask; that lives in `oarfish-engine`.

#![forbid(unsafe_code)]
