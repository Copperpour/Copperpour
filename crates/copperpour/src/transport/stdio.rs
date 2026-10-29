use anyhow::Result;
use copperpour_core::mcp::{handler::McpHandler, protocol::*};
use serde_json::Value;
use std::sync::Arc;
use tokio::io::Stdout;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::sync::{mpsc, Mutex};
use tracing::{debug, error, info};

pub enum StdioExit {
    Eof,
    #[cfg(unix)]
    Reload(copperpour_core::router::meta_tools::ReloadPlan),
}

/// Run the MCP server over STDIO (stdin/stdout).
/// All logging must go to stderr — stdout is reserved for the MCP protocol.
pub async fn run_stdio(handler: McpHandler) -> Result<StdioExit> {
    info!("Starting STDIO transport");

    let stdin = tokio::io::stdin();
    let mut reader = BufReader::new(stdin);
    // Shared with the immediate-notification writer below: each JSON line is
    // written and flushed under the lock, so lines never interleave.
    let stdout = Arc::new(Mutex::new(tokio::io::stdout()));
    let mut line = String::new();

    // Server-initiated notifications (e.g. tools/list_changed after
    // load_toolset / unload_toolset) are queued here by the handler and
    // flushed to stdout after the triggering message is handled. Without this
    // sink, the handler's notification reached only the HTTP/SSE transport and
    // was silently dropped on stdio, so stdio clients (Claude Code) never
    // re-fetched tools/list and tools added mid-session stayed uncallable
    // (issue #19). Notifications are only ever generated synchronously while
    // handling a request, so draining right after each message catches them —
    // no separate task or cancellation-prone select! over read_line is needed.
    let (notif_tx, mut notif_rx) = mpsc::channel::<String>(16);
    handler.register_notification_sink(notif_tx).await;

    // notifications/progress is emitted *during* a tool call, while this loop
    // is still awaiting handle_message, so it cannot wait for the drain below:
    // a separate task writes it as it arrives.
    let (immediate_tx, mut immediate_rx) = mpsc::channel::<String>(64);
    handler.register_immediate_sink(immediate_tx);
    let immediate_out = stdout.clone();
    tokio::spawn(async move {
        while let Some(notif) = immediate_rx.recv().await {
            if write_line(&immediate_out, notif).await.is_err() {
                break;
            }
        }
    });

    loop {
        line.clear();
        let n = reader.read_line(&mut line).await?;
        if n == 0 {
            // EOF — client disconnected
            info!("STDIO: EOF received, shutting down");
            break;
        }

        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }

        debug!("STDIO recv: {}", trimmed);

        let response = match serde_json::from_str::<Value>(trimmed) {
            Ok(msg) => handler.handle_message(msg).await,
            Err(e) => {
                error!("Failed to parse JSON: {}", e);
                Some(JsonRpcResponse::error(
                    Value::Null,
                    JsonRpcError {
                        code: PARSE_ERROR,
                        message: format!("Parse error: {}", e),
                        data: None,
                    },
                ))
            }
        };

        if let Some(resp) = response {
            let json = serde_json::to_string(&resp)?;
            debug!("STDIO send: {}", json);
            write_line(&stdout, json).await?;
        }

        // Flush any notifications the handler queued while processing this
        // message, after the response so the client sees the response first.
        while let Ok(notif) = notif_rx.try_recv() {
            debug!("STDIO send notification: {}", notif);
            write_line(&stdout, notif).await?;
        }

        // The handler only queues a reload. Reaching this point proves the
        // response and all synchronous notifications were flushed. Return the
        // sealed handoff before reading another request so the standalone
        // process can release its run record and perform the exec.
        #[cfg(unix)]
        if let Some(plan) = handler.take_reload_request() {
            return Ok(StdioExit::Reload(plan));
        }
    }

    Ok(StdioExit::Eof)
}

async fn write_line(stdout: &Mutex<Stdout>, mut json: String) -> std::io::Result<()> {
    json.push('\n');
    let mut stdout = stdout.lock().await;
    stdout.write_all(json.as_bytes()).await?;
    stdout.flush().await
}
