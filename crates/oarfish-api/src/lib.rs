//! HTTP surface: the JSON API, the SSE alarm stream, and static hosting for
//! the built Astro board.
//!
//! The board is a static shell with one live island; SSE is what keeps it
//! current without a poll or a rebuild. `GET /api/alarms` renders the
//! server-side first paint and every resync; `GET /api/alarms/stream` carries
//! each [`AlarmChange`](oarfish_engine::AlarmChange) after that. Anything
//! else serves the built board.
//!
//! Broadcast lag is handled explicitly, because the failure is silent. A
//! lagging receiver returns `Lagged` and skips messages — and a skipped
//! `Cleared` would leave a phantom alarm on the board forever. On `Lagged`
//! the stream emits a `resync` event and the board re-fetches `/api/alarms`,
//! the same path reconnect already uses. There is no replay buffer and no
//! `Last-Event-ID`: the open-alarm set is small, so re-fetching is cheap and
//! always correct, where a ring buffer would be another thing to get subtly
//! wrong.
//!
//! Depends on `oarfish-core` for `Alarm` and the open-alarm snapshot, on
//! `oarfish-engine` for the change stream, and on nothing else in the
//! workspace.

#![forbid(unsafe_code)]

use std::convert::Infallible;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::time::Duration;

use std::sync::Arc;

use axum::Json;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::sse::{Event as SseEvent, KeepAlive, Sse};
use axum::routing::get;
use oarfish_core::{Alarm, AlarmDetail, AlarmId, DecisionRecordView, Snapshot, Verdict};
use oarfish_engine::AlarmChange;
use oarfish_store::{DecisionRecord, Verdicts};
use tokio::sync::broadcast;
use tokio_util::sync::CancellationToken;
use tower_http::services::ServeDir;

/// What the server needs: the live open-alarm set, the cached judgments, the
/// change stream, and optionally the built board to serve around them. The
/// shutdown token ends open SSE streams: without it `serve` waits for
/// in-flight connections that wait for a sender that lives inside `serve`,
/// and shutdown deadlocks.
///
/// Clone but not Debug: `Verdicts` holds judge channels, which do not render.
#[derive(Clone)]
pub struct ApiState {
    snapshot: Snapshot,
    verdicts: Arc<Verdicts>,
    sender: broadcast::Sender<AlarmChange>,
    static_dir: Option<PathBuf>,
    shutdown: CancellationToken,
}

impl ApiState {
    pub fn new(
        snapshot: Snapshot,
        verdicts: Arc<Verdicts>,
        sender: broadcast::Sender<AlarmChange>,
        static_dir: Option<PathBuf>,
        shutdown: CancellationToken,
    ) -> Self {
        Self {
            snapshot,
            verdicts,
            sender,
            static_dir,
            shutdown,
        }
    }
}

/// Open alarms as JSON — the board's server-rendered first paint, and every
/// resync after a reconnect or a lagged stream. The read crosses the
/// [`Snapshot`] interface, which owns the poison policy and the sort: a
/// poisoned lock serves the last-known state, never a silent empty list.
async fn alarms(State(state): State<ApiState>) -> Json<Vec<Alarm>> {
    Json(state.snapshot.snapshot())
}

/// Map one stored record to the board's provenance row.
fn record_view_of(record: &DecisionRecord) -> DecisionRecordView {
    DecisionRecordView {
        model: record.model.clone(),
        recorded_at: record.recorded_at(),
        input_tokens: record.input_tokens,
        output_tokens: record.output_tokens,
        cost: record.cost,
    }
}

/// Pure assembly, so tests pin the shape without a store: latest verdict and
/// latest record win, and either may be absent.
fn detail_for(alarm: Alarm, verdicts: Vec<Verdict>, records: Vec<DecisionRecord>) -> AlarmDetail {
    AlarmDetail {
        alarm,
        verdict: verdicts.into_iter().last(),
        record: records
            .into_iter()
            .last()
            .map(|record| record_view_of(&record)),
    }
}

/// One open alarm with its forensics. Reads the prefix-scan helpers, never
/// the judging read: a board view must not enqueue judge work. A cleared or
/// unknown id is 404; the snapshot is the open set, not history.
async fn alarm_detail(
    State(state): State<ApiState>,
    Path(id): Path<AlarmId>,
) -> Result<Json<AlarmDetail>, StatusCode> {
    let alarm = state
        .snapshot
        .snapshot()
        .into_iter()
        .find(|alarm| alarm.id == id)
        .ok_or(StatusCode::NOT_FOUND)?;
    let verdicts = state.verdicts.verdicts_for_template(&alarm.template_id);
    let records = state.verdicts.records_for_template(&alarm.template_id);
    Ok(Json(detail_for(alarm, verdicts, records)))
}

/// One [`AlarmChange`] as one SSE event. Serialization cannot fail for these
/// types in practice; a `"null"` payload keeps the stream alive if it ever
/// does, rather than breaking the board's parser mid-stream.
fn sse_event(change: &AlarmChange) -> SseEvent {
    let data = serde_json::to_string(change).unwrap_or_else(|_| "null".to_owned());
    let name = match change {
        AlarmChange::Raised(_) => "raised",
        AlarmChange::Updated(_) => "updated",
        AlarmChange::Cleared(_) => "cleared",
        AlarmChange::Resync => "resync",
    };
    SseEvent::default().event(name).data(data)
}

/// The stream behind the handler, as a value so tests drive it without HTTP:
/// changes map to named events, and a lagged receiver — which skipped an
/// unknown number of messages, possibly a `Cleared` — maps to `resync`. A
/// closed sender ends the stream, and so does shutdown: the `ApiState`
/// sender clone would otherwise keep `Closed` unreachable while `serve`
/// waits for this stream, deadlocking shutdown with any board connected.
fn sse_body_stream(
    rx: broadcast::Receiver<AlarmChange>,
    shutdown: CancellationToken,
) -> impl futures::Stream<Item = Result<SseEvent, Infallible>> {
    futures::stream::unfold((rx, shutdown), |(mut rx, shutdown)| async move {
        tokio::select! {
            _ = shutdown.cancelled() => None,
            received = rx.recv() => match received {
                Ok(change) => Some((Ok(sse_event(&change)), (rx, shutdown))),
                // The same JSON encoding as `AlarmChange::Resync` through
                // `sse_event`: the board parses every resync the same way,
                // and the lag path is the one that must parse.
                Err(broadcast::error::RecvError::Lagged(_)) => {
                    Some((Ok(sse_event(&AlarmChange::Resync)), (rx, shutdown)))
                }
                Err(broadcast::error::RecvError::Closed) => None,
            },
        }
    })
}

/// SSE over the engine's broadcast. Keep-alive comments hold the connection
/// through proxies: a board that silently stopped updating is
/// indistinguishable from a quiet night.
async fn stream(
    State(state): State<ApiState>,
) -> Sse<impl futures::Stream<Item = Result<SseEvent, Infallible>>> {
    Sse::new(sse_body_stream(
        state.sender.subscribe(),
        state.shutdown.clone(),
    ))
    .keep_alive(
        KeepAlive::new()
            .interval(Duration::from_secs(15))
            .text("keep-alive"),
    )
}

async fn not_found() -> StatusCode {
    StatusCode::NOT_FOUND
}

/// The alarm routes: JSON first paint, per-alarm forensics, plus the SSE
/// stream. The static board is a separate adapter, composed in [`router`].
pub fn alarm_router(state: ApiState) -> axum::Router {
    axum::Router::new()
        .route("/api/alarms", get(alarms))
        .route("/api/alarms/{id}/detail", get(alarm_detail))
        .route("/api/alarms/stream", get(stream))
        .with_state(state)
}

/// The router: JSON, SSE, and the built board around them.
pub fn router(state: ApiState) -> axum::Router {
    let router = alarm_router(state.clone());
    match state.static_dir {
        Some(dir) => router.fallback_service(ServeDir::new(dir)),
        None => router.fallback(not_found),
    }
}

/// Serve until the cancellation token fires, then shut down gracefully.
/// Binds before returning the future so a bind failure surfaces at startup;
/// prefer [`serve_on_listener`] when the caller already holds a bound
/// socket (tests, and the daemon, which must fail startup — not claim to
/// listen — when the port is taken).
pub async fn serve(
    state: ApiState,
    addr: SocketAddr,
    cancel: CancellationToken,
) -> std::io::Result<()> {
    let listener = tokio::net::TcpListener::bind(addr).await?;
    serve_on_listener(state, listener, cancel).await
}

/// Serve on an already-bound socket until the cancellation token fires.
/// Open SSE streams race the token so graceful shutdown completes with
/// boards connected.
pub async fn serve_on_listener(
    state: ApiState,
    listener: tokio::net::TcpListener,
    cancel: CancellationToken,
) -> std::io::Result<()> {
    let addr = listener.local_addr()?;
    tracing::info!(%addr, "api listening");
    let state = ApiState {
        shutdown: cancel.clone(),
        ..state
    };
    axum::serve(listener, router(state))
        .with_graceful_shutdown(async move { cancel.cancelled().await })
        .await
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::response::IntoResponse;
    use oarfish_core::{AlarmId, QuestionsHash, TemplateId, Verdict};
    use std::collections::BTreeMap;
    use time::OffsetDateTime;

    fn an_alarm() -> Alarm {
        Alarm {
            id: AlarmId::generate(),
            template_id: TemplateId::of("task <VAR:NUM> failed"),
            template: "task <VAR:NUM> failed".to_owned(),
            severity: oarfish_core::Severity::Minor,
            host: "web01".to_owned(),
            lane: oarfish_core::Lane::Dashboard,
            count: 3,
            opened_at: OffsetDateTime::UNIX_EPOCH,
        }
    }

    fn a_verdict(model: &str) -> Verdict {
        Verdict {
            template_id: TemplateId::of("task <VAR:NUM> failed"),
            questions_hash: QuestionsHash::of(b"{}"),
            model: model.to_owned(),
            answers: BTreeMap::new(),
            judged_at: OffsetDateTime::UNIX_EPOCH,
        }
    }

    fn a_record(model: &str, at_unix: i64) -> DecisionRecord {
        DecisionRecord {
            id: ulid::Ulid::from_parts(at_unix as u64, 0),
            template_id: TemplateId::of("task <VAR:NUM> failed"),
            questions_hash: QuestionsHash::of(b"{}"),
            model: model.to_owned(),
            template: "task <VAR:NUM> failed".to_owned(),
            state_json: "{}".to_owned(),
            questions_json: "{}".to_owned(),
            answers_json: "{}".to_owned(),
            input_tokens: 100,
            output_tokens: 20,
            cost: Some(0.001),
            recorded_at_unix: at_unix,
        }
    }

    /// Latest verdict and latest record win; either may be absent.
    #[test]
    fn detail_keeps_the_latest_verdict_and_record() {
        let alarm = an_alarm();
        let detail = detail_for(
            alarm.clone(),
            vec![
                a_verdict("typesafe/jev-1.13-20260901"),
                a_verdict("typesafe/jev-1.13-20260917"),
            ],
            vec![
                a_record("typesafe/jev-1.13-20260901", 100),
                a_record("typesafe/jev-1.13-20260917", 200),
            ],
        );
        assert_eq!(detail.alarm, alarm);
        assert_eq!(
            detail.verdict.expect("verdict").model,
            "typesafe/jev-1.13-20260917"
        );
        let record = detail.record.expect("record");
        assert_eq!(record.model, "typesafe/jev-1.13-20260917");
        assert_eq!(record.input_tokens, 100);
        assert_eq!(
            record.recorded_at,
            OffsetDateTime::from_unix_timestamp(200).expect("time")
        );
    }

    /// An open but unjudged alarm details without forensics, never as an error.
    #[test]
    fn detail_without_judgment_carries_no_forensics() {
        let detail = detail_for(an_alarm(), vec![], vec![]);
        assert!(detail.verdict.is_none());
        assert!(detail.record.is_none());
    }

    /// The wire pins RFC 3339 for the record timestamp, matching `opened_at`.
    #[test]
    fn detail_record_timestamp_serializes_as_rfc3339() {
        let detail = detail_for(an_alarm(), vec![], vec![a_record("m", 0)]);
        let json = serde_json::to_value(&detail).expect("serialize");
        assert_eq!(
            json["record"]["recorded_at"],
            serde_json::json!("1970-01-01T00:00:00Z")
        );
    }

    /// Render a body stream to its SSE wire text. Dropping the sender ends
    /// the stream, so collection terminates; the keep-alive never fires in
    /// time to matter.
    async fn wire_text(rx: broadcast::Receiver<AlarmChange>) -> String {
        let response = Sse::new(sse_body_stream(rx, CancellationToken::new())).into_response();
        let bytes = axum::body::to_bytes(response.into_body(), 64 * 1024)
            .await
            .expect("collect");
        String::from_utf8(bytes.to_vec()).expect("sse is text")
    }

    /// A lagged receiver maps to `resync`, never to a guessed replay: the
    /// skipped messages are unknowable, and replaying a guessed `Cleared`
    /// would leave a phantom alarm on the board.
    #[tokio::test]
    async fn a_lagged_receiver_resyncs_rather_than_skipping() {
        let (tx, rx) = broadcast::channel::<AlarmChange>(2);
        // Three sends into a capacity-two channel with no reader: the
        // receiver is lagged by construction, deterministically.
        for _ in 0..3 {
            tx.send(AlarmChange::Cleared(AlarmId::generate()))
                .expect("no receivers needed");
        }
        drop(tx);
        let text = wire_text(rx).await;
        // The first thing a lagged stream emits is `resync`, before any
        // buffered state: the board re-fetches rather than trusting a stream
        // it knows skipped messages. The buffered tail still follows — those
        // are live changes, not replays — and the re-fetch covers them.
        assert!(
            text.starts_with("event: resync"),
            "a lagged stream resyncs first, got {text:?}"
        );
        // The lag resync carries the same JSON encoding as a broadcast
        // `Resync`, so one `JSON.parse` path handles both.
        let expected = serde_json::to_string(&AlarmChange::Resync).expect("serialize");
        assert!(
            text.contains(&expected),
            "the lag resync rides as JSON {expected:?}, got {text:?}"
        );
    }

    /// Changes map to named events carrying their JSON.
    #[tokio::test]
    async fn changes_map_to_named_events() {
        let (tx, rx) = broadcast::channel::<AlarmChange>(16);
        let cleared = AlarmChange::Cleared(AlarmId::generate());
        tx.send(cleared.clone()).expect("send");
        drop(tx);
        let text = wire_text(rx).await;
        assert!(text.contains("event: cleared"), "got {text:?}");
        assert!(
            text.contains(&serde_json::to_string(&cleared).expect("serialize")),
            "the change rides as JSON, got {text:?}"
        );
    }

    /// Shutdown ends an idle stream: without the race the `serve` future
    /// waits for connections that wait for a sender living inside `serve`.
    #[tokio::test]
    async fn shutdown_ends_an_idle_stream() {
        let (_tx, rx) = broadcast::channel::<AlarmChange>(16);
        let shutdown = CancellationToken::new();
        let mut stream = Box::pin(sse_body_stream(rx, shutdown.clone()));
        shutdown.cancel();
        use futures::StreamExt as _;
        let next = tokio::time::timeout(std::time::Duration::from_secs(5), stream.next()).await;
        assert!(
            matches!(next, Ok(None)),
            "shutdown ends the stream, got {next:?}"
        );
    }
}
