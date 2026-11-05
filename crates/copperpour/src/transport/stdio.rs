use anyhow::Result;
use copperpour_core::mcp::{handler::McpHandler, protocol::*};
use serde_json::Value;
use std::future::Future;
use tokio::io::{AsyncBufReadExt, AsyncWrite, AsyncWriteExt, BufReader};
use tokio::sync::mpsc;
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
    let mut stdout = tokio::io::stdout();
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
    // call_writing_progress writes it while the call runs, and everything
    // queued before the call ends goes out before the response.
    //
    // Not covered end to end: no tool reports progress without a Freerouting
    // JAR, so tests/protocol_stdio.rs cannot drive one. The handler's delivery
    // to this sink and the ordering below have their own tests.
    let (immediate_tx, mut immediate_rx) = mpsc::channel::<String>(64);
    handler.register_immediate_sink(immediate_tx);

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
            Ok(msg) => {
                call_writing_progress(handler.handle_message(msg), &mut immediate_rx, &mut stdout)
                    .await?
            }
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
            write_line(&mut stdout, json).await?;
        }

        // Flush any notifications the handler queued while processing this
        // message, after the response so the client sees the response first.
        while let Ok(notif) = notif_rx.try_recv() {
            debug!("STDIO send notification: {}", notif);
            write_line(&mut stdout, notif).await?;
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

/// Run `call` while writing every progress notification as it arrives, then
/// the ones still queued when it ends, so that all of them precede the
/// response the caller writes next. A client that sees the response first
/// would get a progress token it no longer knows.
async fn call_writing_progress<F: Future, W: AsyncWrite + Unpin>(
    call: F,
    progress: &mut mpsc::Receiver<String>,
    out: &mut W,
) -> std::io::Result<F::Output> {
    tokio::pin!(call);
    let output = loop {
        tokio::select! {
            biased;
            output = &mut call => break output,
            Some(notif) = progress.recv() => write_line(out, notif).await?,
        }
    };
    while let Ok(notif) = progress.try_recv() {
        write_line(out, notif).await?;
    }
    Ok(output)
}

async fn write_line<W: AsyncWrite + Unpin>(out: &mut W, mut json: String) -> std::io::Result<()> {
    json.push('\n');
    out.write_all(json.as_bytes()).await?;
    out.flush().await
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lines(out: Vec<u8>) -> Vec<String> {
        String::from_utf8(out)
            .unwrap()
            .lines()
            .map(str::to_string)
            .collect()
    }

    #[tokio::test]
    async fn progress_sent_during_the_call_is_written_before_the_response() {
        let (tx, mut rx) = mpsc::channel(8);
        let mut out = Vec::new();
        let response = call_writing_progress(
            async {
                tx.try_send("p1".to_string()).unwrap();
                tokio::task::yield_now().await;
                tx.try_send("p2".to_string()).unwrap();
                tokio::task::yield_now().await;
                "response"
            },
            &mut rx,
            &mut out,
        )
        .await
        .unwrap();
        write_line(&mut out, response.to_string()).await.unwrap();
        assert_eq!(lines(out), ["p1", "p2", "response"]);
    }

    #[tokio::test]
    async fn progress_queued_just_before_the_end_still_precedes_the_response() {
        let (tx, mut rx) = mpsc::channel(8);
        let mut out = Vec::new();
        // The call ends without yielding after its last report: only the
        // drain after the call can write it.
        let response = call_writing_progress(
            async {
                tx.try_send("last".to_string()).unwrap();
                "response"
            },
            &mut rx,
            &mut out,
        )
        .await
        .unwrap();
        write_line(&mut out, response.to_string()).await.unwrap();
        assert_eq!(lines(out), ["last", "response"]);
    }
}
