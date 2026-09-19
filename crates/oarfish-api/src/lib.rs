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
//! Depends on `oarfish-core` for `Alarm` and `AlarmChange`, on
//! `oarfish-engine` for the change stream and the open-alarm snapshot, and
//! on nothing else in the workspace.

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
use oarfish_core::Alarm;
use oarfish_engine::{AlarmChange, Snapshot};
use tokio::sync::broadcast;
use tokio_util::sync::CancellationToken;
use tower_http::services::ServeDir;

/// What the server needs: the live open-alarm set, the change stream, and
/// optionally the built board to serve around them.
#[derive(Debug, Clone)]
pub struct ApiState {
    snapshot: Snapshot,
    sender: broadcast::Sender<AlarmChange>,
    static_dir: Option<PathBuf>,
}

impl ApiState {
    pub fn new(
        snapshot: Snapshot,
        sender: broadcast::Sender<AlarmChange>,
        static_dir: Option<PathBuf>,
    ) -> Self {
        Self {
            snapshot,
            sender,
            static_dir,
        }
    }
}

/// Open alarms as JSON — the board's server-rendered first paint, and every
/// resync after a reconnect or a lagged stream.
async fn alarms(State(state): State<ApiState>) -> Json<Vec<Alarm>> {
    let mut alarms: Vec<Alarm> = state
        .snapshot
        .read()
        .map(|snapshot| snapshot.values().cloned().collect())
        .unwrap_or_default();
    alarms.sort_by_key(|alarm| alarm.opened_at);
    Json(alarms)
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
/// closed sender ends the stream.
fn sse_body_stream(
    rx: broadcast::Receiver<AlarmChange>,
) -> impl futures::Stream<Item = Result<SseEvent, Infallible>> {
    futures::stream::unfold(rx, |mut rx| async move {
        match rx.recv().await {
            Ok(change) => Some((Ok(sse_event(&change)), rx)),
            Err(broadcast::error::RecvError::Lagged(_)) => {
                Some((Ok(SseEvent::default().event("resync").data("resync")), rx))
            }
            Err(broadcast::error::RecvError::Closed) => None,
        }
    })
}

/// SSE over the engine's broadcast. Keep-alive comments hold the connection
/// through proxies: a board that silently stopped updating is
/// indistinguishable from a quiet night.
async fn stream(
    State(state): State<ApiState>,
) -> Sse<impl futures::Stream<Item = Result<SseEvent, Infallible>>> {
    Sse::new(sse_body_stream(state.sender.subscribe())).keep_alive(
        KeepAlive::new()
            .interval(Duration::from_secs(15))
            .text("keep-alive"),
    )
}

async fn not_found() -> StatusCode {
    StatusCode::NOT_FOUND
}

/// The router: JSON, SSE, and the built board around them.
pub fn router(state: ApiState) -> axum::Router {
    let router = axum::Router::new()
        .route("/api/alarms", get(alarms))
        .route("/api/alarms/stream", get(stream))
        .with_state(state.clone());
    match state.static_dir {
        Some(dir) => router.fallback_service(ServeDir::new(dir)),
        None => router.fallback(not_found),
    }
}

/// Serve until the cancellation token fires, then shut down gracefully.
pub async fn serve(
    state: ApiState,
    addr: SocketAddr,
    cancel: CancellationToken,
) -> std::io::Result<()> {
    let listener = tokio::net::TcpListener::bind(addr).await?;
    tracing::info!(%addr, "api listening");
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
        let response = Sse::new(sse_body_stream(rx)).into_response();
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
}
