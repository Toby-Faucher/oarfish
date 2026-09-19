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

use axum::Json;
use axum::extract::State;
use axum::http::StatusCode;
use axum::response::sse::{Event as SseEvent, KeepAlive, Sse};
use axum::routing::get;
use oarfish_core::{Alarm, Snapshot};
use oarfish_engine::AlarmChange;
use tokio::sync::broadcast;
use tokio_util::sync::CancellationToken;
use tower_http::services::ServeDir;

/// What the server needs: the live open-alarm set, the change stream, and
/// optionally the built board to serve around them. The shutdown token ends
/// open SSE streams: without it `serve` waits for in-flight connections that
/// wait for a sender that lives inside `serve`, and shutdown deadlocks.
#[derive(Debug, Clone)]
pub struct ApiState {
    snapshot: Snapshot,
    sender: broadcast::Sender<AlarmChange>,
    static_dir: Option<PathBuf>,
    shutdown: CancellationToken,
}

impl ApiState {
    pub fn new(
        snapshot: Snapshot,
        sender: broadcast::Sender<AlarmChange>,
        static_dir: Option<PathBuf>,
        shutdown: CancellationToken,
    ) -> Self {
        Self {
            snapshot,
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

/// The alarm routes: JSON first paint plus the SSE stream. The static board
/// is a separate adapter, composed in [`router`].
pub fn alarm_router(state: ApiState) -> axum::Router {
    axum::Router::new()
        .route("/api/alarms", get(alarms))
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
    use oarfish_core::AlarmId;

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
