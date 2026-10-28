//! Streamable HTTP transport (MCP spec 2025-03-26 / 2025-06-18).
//!
//! A single MCP endpoint at `/mcp` supports:
//!   POST /mcp    — client sends one JSON-RPC message.
//!                  Requests get an `application/json` response body;
//!                  notifications/responses get `202 Accepted` with no body.
//!   GET  /mcp    — opens an SSE stream for server-initiated messages
//!                  (e.g. `notifications/tools/list_changed`).
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
    http::{HeaderMap, StatusCode},
    middleware::{self, Next},
    response::{
        sse::{Event, Sse},
        IntoResponse, Response,
    },
    routing::get,
    Json, Router,
};
use copperpour_core::mcp::handler::McpHandler;
use serde_json::Value;
use std::convert::Infallible;
use std::sync::Arc;
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

async fn handle_post(State(state): State<AppState>, Json(body): Json<Value>) -> Response {
    match state.handler.handle_message(body).await {
        // JSON-RPC request → single JSON response object.
        Some(resp) => Json(resp).into_response(),
        // JSON-RPC notification or response → 202 Accepted, no body.
        None => StatusCode::ACCEPTED.into_response(),
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
