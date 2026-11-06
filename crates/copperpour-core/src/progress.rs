//! MCP progress notifications for long-running tools.
//!
//! A client that wants progress for a `tools/call` sends `_meta.progressToken`.
//! The handler then runs the tool inside [`scope`], and any code on that call's
//! task can [`report`] without threading a handle through every signature.
//! Without a token, or outside a tool call, `report` does nothing.
//!
//! Notifications go to the transport's *immediate* sinks: unlike
//! `tools/list_changed`, which stdio flushes after the response, progress is
//! only worth anything while the call is still running.

use serde_json::{json, Value};
use std::sync::Arc;

type Emit = Arc<dyn Fn(String) + Send + Sync>;

#[derive(Clone)]
pub struct ProgressReporter {
    token: Value,
    emit: Emit,
}

impl ProgressReporter {
    pub fn new(token: Value, emit: impl Fn(String) + Send + Sync + 'static) -> Self {
        Self {
            token,
            emit: Arc::new(emit),
        }
    }

    fn notification(&self, progress: f64, total: Option<f64>, message: &str) -> String {
        let mut params = json!({
            "progressToken": self.token,
            "progress": progress,
            "message": message,
        });
        if let Some(total) = total {
            params["total"] = json!(total);
        }
        json!({
            "jsonrpc": "2.0",
            "method": "notifications/progress",
            "params": params,
        })
        .to_string()
    }
}

tokio::task_local! {
    static REPORTER: Option<ProgressReporter>;
}

/// Run `future` with `reporter` as the current call's progress channel.
pub async fn scope<F: std::future::Future>(
    reporter: Option<ProgressReporter>,
    future: F,
) -> F::Output {
    REPORTER.scope(reporter, future).await
}

/// Report progress for the current tool call. `progress` must increase from one
/// report to the next (MCP requirement); `total` is optional.
pub fn report(progress: f64, total: Option<f64>, message: &str) {
    let _ = REPORTER.try_with(|reporter| {
        if let Some(reporter) = reporter {
            (reporter.emit)(reporter.notification(progress, total, message));
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    #[tokio::test]
    async fn reports_reach_the_sink_only_inside_a_scope_with_a_token() {
        let sent = Arc::new(Mutex::new(Vec::<String>::new()));
        let sink = sent.clone();
        let reporter =
            ProgressReporter::new(json!("tok-1"), move |line| sink.lock().unwrap().push(line));

        report(1.0, None, "outside any call");
        scope(None, async { report(1.0, None, "call without a token") }).await;
        scope(Some(reporter), async {
            report(1.0, None, "pass 1");
            report(2.0, Some(10.0), "pass 2");
        })
        .await;

        let sent = sent.lock().unwrap();
        assert_eq!(sent.len(), 2);
        let first: Value = serde_json::from_str(&sent[0]).unwrap();
        assert_eq!(first["method"], "notifications/progress");
        assert_eq!(first["params"]["progressToken"], "tok-1");
        assert_eq!(first["params"]["message"], "pass 1");
        assert!(first["params"].get("total").is_none());
        let second: Value = serde_json::from_str(&sent[1]).unwrap();
        assert_eq!(second["params"]["total"], 10.0);
    }
}
