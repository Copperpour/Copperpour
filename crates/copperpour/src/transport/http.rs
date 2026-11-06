//! Streamable HTTP transport (MCP spec 2025-03-26 / 2025-06-18).
//!
//! A single MCP endpoint at `/mcp` supports:
//!   POST /mcp    — client sends one JSON-RPC message.
//!                  Requests get an `application/json` response body, except
//!                  a `tools/call` with `_meta.progressToken` from a client
//!                  that accepts `text/event-stream`: it gets an SSE stream of
//!                  its own, with the call's `notifications/progress` and then
//!                  the response.
//!                  Notifications/responses get `202 Accepted` with no body.
//!   GET  /mcp    — opens an SSE stream for server-initiated messages not tied
//!                  to a request (e.g. `notifications/tools/list_changed`).
//!   DELETE /mcp  — 405 (this server is stateless; no session termination).
//!
//! `GET /mcp/sse` is kept as an alias for older configs. `GET /health`
//! returns "ok" for probes.
//!
//! Security (per the MCP spec's Streamable HTTP requirements): the server
//! binds to 127.0.0.1 by default, and every request's `Origin` header — when
//! present — must be a localhost origin, otherwise 403. This prevents DNS
//! rebinding attacks from remote websites against the local server.

use anyhow::Result;
use axum::{
    extract::State,
    http::{header::ACCEPT, HeaderMap, StatusCode},
    middleware::{self, Next},
    response::{
        sse::{Event, KeepAlive, Sse},
        IntoResponse, Response,
    },
    routing::get,
    Json, Router,
};
use copperpour_core::mcp::handler::McpHandler;
use copperpour_core::mcp::protocol::tools_call_progress_token;
use serde_json::Value;
use std::convert::Infallible;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};
use tokio::sync::mpsc;
use tokio_stream::wrappers::ReceiverStream;
use tracing::info;

#[derive(Clone)]
struct AppState {
    handler: Arc<McpHandler>,
}

/// Run the MCP server over Streamable HTTP.
pub async fn run_http(handler: McpHandler, addr: &str) -> Result<()> {
    let state = AppState {
        handler: Arc::new(handler),
    };

    let app = Router::new()
        .route(
            "/mcp",
            axum::routing::post(handle_post)
                .get(handle_sse)
                .delete(|| async { StatusCode::METHOD_NOT_ALLOWED }),
        )
        .route("/mcp/sse", get(handle_sse)) // legacy alias
        .route("/health", get(handle_health))
        .layer(middleware::from_fn(validate_origin))
        .with_state(state);

    let listener = tokio::net::TcpListener::bind(addr).await?;
    info!("Streamable HTTP transport listening on http://{}/mcp", addr);

    axum::serve(listener, app).await?;
    Ok(())
}

/// Reject any request whose `Origin` header is present and not a localhost
/// origin (MCP spec: servers MUST validate Origin to prevent DNS rebinding).
/// Requests without an Origin header (curl, native MCP clients) pass through.
async fn validate_origin(
    headers: HeaderMap,
    request: axum::extract::Request,
    next: Next,
) -> Response {
    if let Some(origin) = headers.get("origin").and_then(|v| v.to_str().ok()) {
        let allowed = origin
            .strip_prefix("http://")
            .or_else(|| origin.strip_prefix("https://"))
            .map(|rest| {
                let host = rest.split(':').next().unwrap_or("");
                host == "localhost" || host == "127.0.0.1" || host == "[::1]"
            })
            .unwrap_or(false);
        if !allowed {
            return (StatusCode::FORBIDDEN, "Origin not allowed").into_response();
        }
    }
    next.run(request).await
}

async fn handle_post(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> Response {
    if accepts_event_stream(&headers) && tools_call_progress_token(&body).is_some() {
        let handler = state.handler.clone();
        let events = call_events(move |progress| async move {
            let response = handler.handle_message_with_progress(body, progress).await?;
            serde_json::to_string(&response).ok()
        });
        return Sse::new(events)
            .keep_alive(KeepAlive::default())
            .into_response();
    }
    match state.handler.handle_message(body).await {
        // JSON-RPC request → single JSON response object.
        Some(resp) => Json(resp).into_response(),
        // JSON-RPC notification or response → 202 Accepted, no body.
        None => StatusCode::ACCEPTED.into_response(),
    }
}

/// Whether the `Accept` header lists `text/event-stream`.
fn accepts_event_stream(headers: &HeaderMap) -> bool {
    headers
        .get_all(ACCEPT)
        .iter()
        .filter_map(|value| value.to_str().ok())
        .flat_map(|value| value.split(','))
        .any(|range| {
            range
                .split(';')
                .next()
                .unwrap_or("")
                .trim()
                .eq_ignore_ascii_case("text/event-stream")
        })
}

/// The SSE stream answering one call: what `run` sends on its progress channel
/// while it runs, then the response it returns, then the end of the stream.
/// The response goes through the same channel after every report, so it is
/// always the last event. Dropping the stream (client gone) cancels the call,
/// as dropping a JSON response would.
fn call_events<F, Fut>(run: F) -> CallEvents
where
    F: FnOnce(mpsc::Sender<String>) -> Fut,
    Fut: Future<Output = Option<String>> + Send + 'static,
{
    let (tx, rx) = mpsc::channel::<String>(32);
    let call = run(tx.clone());
    let task = tokio::spawn(async move {
        if let Some(response) = call.await {
            let _ = tx.send(response).await;
        }
    });
    CallEvents {
        events: ReceiverStream::new(rx),
        call: task,
    }
}

struct CallEvents {
    events: ReceiverStream<String>,
    call: tokio::task::JoinHandle<()>,
}

impl tokio_stream::Stream for CallEvents {
    type Item = Result<Event, Infallible>;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        Pin::new(&mut self.events)
            .poll_next(cx)
            .map(|json| json.map(|json| Ok(Event::default().data(json))))
    }
}

impl Drop for CallEvents {
    fn drop(&mut self) {
        self.call.abort();
    }
}

/// GET on the MCP endpoint: open an SSE stream for server-initiated messages.
async fn handle_sse(
    State(state): State<AppState>,
) -> Sse<impl tokio_stream::Stream<Item = Result<Event, Infallible>>> {
    let (tx, rx) = tokio::sync::mpsc::channel::<Event>(32);

    // Register SSE sender with handler so it can push notifications
    // (e.g. tools/list_changed after load_toolset / unload_toolset).
    state.handler.register_sse_sender(tx).await;

    use tokio_stream::StreamExt;
    let stream = ReceiverStream::new(rx).map(Ok);
    Sse::new(stream)
}

async fn handle_health() -> &'static str {
    "ok"
}

#[cfg(test)]
mod tests {
    use super::*;

    fn accept(value: &str) -> HeaderMap {
        let mut headers = HeaderMap::new();
        headers.insert(ACCEPT, value.parse().unwrap());
        headers
    }

    #[test]
    fn the_accept_header_must_list_the_event_stream() {
        assert!(accepts_event_stream(&accept(
            "application/json, text/event-stream"
        )));
        assert!(accepts_event_stream(&accept("Text/Event-Stream;q=0.5")));
        assert!(!accepts_event_stream(&accept("application/json")));
        assert!(!accepts_event_stream(&accept("text/event-streamx")));
        assert!(!accepts_event_stream(&HeaderMap::new()));
    }

    /// The `data:` lines of the stream as a client reads them.
    async fn data_of(events: CallEvents) -> Vec<String> {
        let body = Sse::new(events).into_response().into_body();
        let bytes = axum::body::to_bytes(body, usize::MAX).await.unwrap();
        String::from_utf8(bytes.to_vec())
            .unwrap()
            .lines()
            .filter_map(|line| line.strip_prefix("data: ").map(str::to_string))
            .collect()
    }

    #[tokio::test]
    async fn progress_comes_first_then_the_response_then_the_end() {
        let events = call_events(|progress| async move {
            progress.try_send("p1".to_string()).unwrap();
            tokio::task::yield_now().await;
            progress.try_send("p2".to_string()).unwrap();
            Some("response".to_string())
        });
        assert_eq!(data_of(events).await, ["p1", "p2", "response"]);
    }

    #[tokio::test]
    async fn a_call_without_a_response_ends_the_stream_empty() {
        let events = call_events(|_progress| async { None });
        assert!(data_of(events).await.is_empty());
    }

    #[tokio::test]
    async fn dropping_the_stream_cancels_the_call() {
        struct Dropped(Option<tokio::sync::oneshot::Sender<()>>);
        impl Drop for Dropped {
            fn drop(&mut self) {
                if let Some(tx) = self.0.take() {
                    let _ = tx.send(());
                }
            }
        }
        let (tx, rx) = tokio::sync::oneshot::channel();
        let (started_tx, started) = tokio::sync::oneshot::channel();
        let events = call_events(move |_progress| async move {
            let _guard = Dropped(Some(tx));
            let _ = started_tx.send(());
            std::future::pending::<()>().await;
            None
        });
        started.await.unwrap();
        drop(events);
        tokio::time::timeout(std::time::Duration::from_secs(5), rx)
            .await
            .expect("the call was cancelled")
            .unwrap();
    }
}
