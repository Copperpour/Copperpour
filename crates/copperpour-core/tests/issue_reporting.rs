#![cfg(feature = "issue-reporting")]

use copperpour_core::issue_reporting::{
    IssueReporter, IssueReportingConfig, DEFAULT_SENTRY_DSN, VALID_CATEGORIES,
};
use copperpour_core::mcp::handler::McpHandler;
use copperpour_core::tools::ServerConfig;
use sentry::protocol::Event;
use sentry::{Envelope, Transport, TransportFactory};
use serde_json::{json, Value};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

struct RecordingTransport {
    events: Arc<Mutex<Vec<Event<'static>>>>,
}

impl Transport for RecordingTransport {
    fn send_envelope(&self, envelope: Envelope) {
        if let Some(event) = envelope.event() {
            self.events.lock().unwrap().push(event.clone());
        }
    }
}

struct RecordingFactory {
    events: Arc<Mutex<Vec<Event<'static>>>>,
}

impl TransportFactory for RecordingFactory {
    fn create_transport(&self, _options: &sentry::ClientOptions) -> Arc<dyn Transport> {
        Arc::new(RecordingTransport {
            events: self.events.clone(),
        })
    }
}

async fn make_handler(
    cfg: IssueReportingConfig,
    store_dir: PathBuf,
    config_file: Option<PathBuf>,
    project_dir: Option<PathBuf>,
) -> (McpHandler, Arc<Mutex<Vec<Event<'static>>>>) {
    let events = Arc::new(Mutex::new(Vec::new()));
    let handler = McpHandler::new(ServerConfig {
        project_dir: project_dir.clone(),
        ..Default::default()
    })
    .await
    .unwrap();
    let reporter = Arc::new(IssueReporter::with_overrides(
        cfg,
        config_file,
        Some(store_dir),
        Some(DEFAULT_SENTRY_DSN.to_string()),
        Some(Arc::new(RecordingFactory {
            events: events.clone(),
        })),
        "stdio".to_string(),
        project_dir,
    ));
    handler.set_issue_reporter(reporter);
    (handler, events)
}

async fn call_tool(handler: &McpHandler, name: &str, arguments: Value) -> (bool, Value) {
    let resp = handler
        .handle_message(json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "tools/call",
            "params": { "name": name, "arguments": arguments }
        }))
        .await
        .expect("response");
    assert!(resp.error.is_none(), "unexpected rpc error: {resp:?}");
    let result = resp.result.unwrap();
    let is_error = result["isError"].as_bool().unwrap_or(false);
    let body: Value = serde_json::from_str(result["content"][0]["text"].as_str().unwrap()).unwrap();
    (is_error, body)
}

async fn call_initialize(handler: &McpHandler) -> Value {
    let resp = handler
        .handle_message(json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": { "clientInfo": { "name": "test-client" } }
        }))
        .await
        .expect("response");
    assert!(resp.error.is_none(), "unexpected rpc error: {resp:?}");
    resp.result.unwrap()
}

#[tokio::test]
async fn report_issue_validates_categories_and_conditional_required_fields() {
    let dir = tempfile::tempdir().unwrap();
    let (handler, events) = make_handler(
        IssueReportingConfig::default(),
        dir.path().to_path_buf(),
        None,
        None,
    )
    .await;

    // Unknown category rejected by the schema gate.
    let (is_err, body) = call_tool(
        &handler,
        "report_issue",
        json!({"category": "bogus_category", "summary": "test"}),
    )
    .await;
    assert!(is_err, "unknown category must be refused: {body}");
    assert_eq!(body["error"]["kind"], "invalid_argument");

    // missing_tool requires capability.
    let (is_err, body) = call_tool(
        &handler,
        "report_issue",
        json!({"category": "missing_tool", "summary": "need something"}),
    )
    .await;
    assert!(is_err);
    assert_eq!(body["error"]["kind"], "invalid_argument");
    assert_eq!(body["error"]["field"], "capability");

    // missing_argument requires tool and argument.
    let (is_err, body) = call_tool(
        &handler,
        "report_issue",
        json!({"category": "missing_argument", "summary": "need opt", "tool": "add_wire"}),
    )
    .await;
    assert!(is_err);
    assert_eq!(body["error"]["kind"], "invalid_argument");
    assert_eq!(body["error"]["field"], "argument");

    let (is_err, body) = call_tool(
        &handler,
        "report_issue",
        json!({"category": "missing_argument", "summary": "need opt", "argument": "width"}),
    )
    .await;
    assert!(is_err);
    assert_eq!(body["error"]["kind"], "invalid_argument");
    assert_eq!(body["error"]["field"], "tool");

    // Every valid category is accepted when required fields are supplied.
    for cat in VALID_CATEGORIES {
        let mut args = json!({
            "category": cat,
            "summary": format!("report for {cat}"),
        });
        if *cat == "missing_tool" {
            args["capability"] = json!("impedance solver");
        } else if *cat == "missing_argument" {
            args["tool"] = json!("add_wire");
            args["argument"] = json!("stroke_style");
        }
        let (is_err, body) = call_tool(&handler, "report_issue", args).await;
        assert!(!is_err, "category {cat} should succeed: {body}");
        assert_eq!(body["sent"], true);
        assert!(!body["event_id"].as_str().unwrap_or("").is_empty());
        let expected_type = if matches!(*cat, "missing_tool" | "missing_argument") {
            "feature_request"
        } else {
            "bug"
        };
        assert_eq!(body["report_type"], expected_type);
    }

    assert_eq!(events.lock().unwrap().len(), VALID_CATEGORIES.len());
}

#[tokio::test]
async fn report_issue_refuses_when_disabled_and_sends_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let (handler, events) = make_handler(
        IssueReportingConfig {
            enabled: false,
            capture: false,
            automatic: false,
        },
        dir.path().to_path_buf(),
        None,
        None,
    )
    .await;

    let (is_err, body) = call_tool(
        &handler,
        "report_issue",
        json!({"category": "other", "summary": "should not send"}),
    )
    .await;
    assert!(is_err);
    assert_eq!(body["error"]["kind"], "invalid_configuration");
    assert_eq!(body["error"]["reason"], "issue_reporting_disabled");
    assert!(events.lock().unwrap().is_empty());
}

#[tokio::test]
async fn scrubbing_strips_project_path_home_and_prior_tool_arguments() {
    let dir = tempfile::tempdir().unwrap();
    let fake_project = PathBuf::from("/tmp/unique_project_root_94827/secret_board_name");
    let (handler, events) = make_handler(
        IssueReportingConfig::default(),
        dir.path().to_path_buf(),
        None,
        Some(fake_project.clone()),
    )
    .await;

    // Make a prior tool call carrying unique secret net & schematic names in its arguments.
    let _ = call_tool(
        &handler,
        "find_tools",
        json!({"query": "SECRET_NET_ALPHA_998877 /tmp/unique_project_root_94827/secret_board_name/main.kicad_sch"}),
    )
    .await;

    let (is_err, body) = call_tool(
        &handler,
        "report_issue",
        json!({
            "category": "wrong_result",
            "summary": "Failed on /tmp/unique_project_root_94827/secret_board_name/main.kicad_sch",
            "tool": "find_tools",
            "actual": "Bad output for /tmp/unique_project_root_94827/secret_board_name/sheet2.kicad_sch at 10.20.30.40"
        }),
    )
    .await;
    assert!(!is_err, "{body}");

    let captured = events.lock().unwrap();
    assert_eq!(captured.len(), 1);
    let serialized = serde_json::to_string(&captured[0]).unwrap();
    assert!(
        !serialized.contains("unique_project_root_94827"),
        "project root leaked: {serialized}"
    );
    assert!(
        !serialized.contains("secret_board_name"),
        "board dir leaked: {serialized}"
    );
    assert!(
        !serialized.contains("main.kicad_sch"),
        "filename leaked: {serialized}"
    );
    assert!(
        !serialized.contains("sheet2.kicad_sch"),
        "filename leaked: {serialized}"
    );
    assert!(
        !serialized.contains("SECRET_NET_ALPHA_998877"),
        "prior tool args leaked: {serialized}"
    );
    assert!(
        !serialized.contains("10.20.30.40"),
        "IP leaked: {serialized}"
    );
    assert!(captured[0].server_name.is_none());
    assert!(captured[0].user.is_none());
    assert_eq!(
        captured[0].release.as_deref(),
        Some(format!("copperpour@{}", env!("CARGO_PKG_VERSION")).as_str())
    );
}

#[tokio::test]
async fn pending_panic_surfaces_in_next_initialize_and_sends_or_discards() {
    let dir = tempfile::tempdir().unwrap();
    let store_dir = dir.path().join("pending");
    let config_file = dir.path().join("config.toml");

    // Session 1: panic captured locally, nothing sent.
    let (handler1, events1) = make_handler(
        IssueReportingConfig::default(),
        store_dir.clone(),
        Some(config_file.clone()),
        None,
    )
    .await;
    let stored = handler1
        .issue_reporter()
        .capture_panic(
            "forced test panic",
            Some("crates/copperpour-core/src/tools/foo.rs:42:1"),
        )
        .expect("pending report stored");
    assert!(events1.lock().unwrap().is_empty());

    // Session 2: initialize carries pending_reports notice with ID and payload.
    let (handler2, events2) = make_handler(
        IssueReportingConfig::default(),
        store_dir.clone(),
        Some(config_file.clone()),
        None,
    )
    .await;
    let init = call_initialize(&handler2).await;
    let instructions = init["instructions"].as_str().unwrap();
    assert!(instructions.contains("pending_reports:"), "{instructions}");
    assert!(instructions.contains(&stored.id), "{instructions}");
    assert!(instructions.contains("always_send"), "{instructions}");
    assert!(instructions.contains("never_ask"), "{instructions}");

    // Send pending report + enable always_send.
    let (is_err, body) = call_tool(
        &handler2,
        "report_issue",
        json!({"pending_ids": [stored.id], "always_send": true}),
    )
    .await;
    assert!(!is_err, "{body}");
    assert_eq!(body["always_send_enabled"], true);
    assert_eq!(body["remaining_pending"], 0);
    assert_eq!(body["sent"].as_array().unwrap().len(), 1);
    assert_eq!(events2.lock().unwrap().len(), 1);
    assert!(handler2.issue_reporter().list_pending().is_empty());
    assert!(handler2.issue_reporter().effective_config().automatic);

    // Config file now has issue_reporting.automatic = true.
    let cfg_text = std::fs::read_to_string(&config_file).unwrap();
    assert!(cfg_text.contains("automatic = true"), "{cfg_text}");

    // Discard + never_ask flow.
    let (handler3, events3) = make_handler(
        IssueReportingConfig::default(),
        store_dir.clone(),
        Some(config_file.clone()),
        None,
    )
    .await;
    let stored2 = handler3
        .issue_reporter()
        .capture_panic(
            "second panic",
            Some("crates/copperpour-core/src/tools/bar.rs:99:1"),
        )
        .expect("stored");
    let (is_err, body) = call_tool(
        &handler3,
        "discard_pending_reports",
        json!({"pending_ids": [stored2.id], "never_ask": true}),
    )
    .await;
    assert!(!is_err, "{body}");
    assert_eq!(body["never_ask_enabled"], true);
    assert_eq!(body["remaining_count"], 0);
    assert!(events3.lock().unwrap().is_empty(), "discard must not send");
    assert!(!handler3.issue_reporter().effective_config().capture);
    let cfg_text2 = std::fs::read_to_string(&config_file).unwrap();
    assert!(cfg_text2.contains("capture = false"), "{cfg_text2}");
}

#[tokio::test]
async fn call_tool_policy_requires_allow_writes_for_issue_reporting_tools() {
    let dir = tempfile::tempdir().unwrap();
    let (handler, _events) = make_handler(
        IssueReportingConfig::default(),
        dir.path().to_path_buf(),
        None,
        None,
    )
    .await;

    for target in ["report_issue", "discard_pending_reports"] {
        let (is_err, body) = call_tool(
            &handler,
            "call_tool",
            json!({"name": target, "arguments": {}}),
        )
        .await;
        assert!(
            is_err,
            "{target} must be refused through call_tool by default"
        );
        assert_eq!(body["error"]["kind"], "tool_not_permitted");
    }

    handler.allow_call_tool_writes();
    let (is_err, body) = call_tool(
        &handler,
        "call_tool",
        json!({
            "name": "report_issue",
            "arguments": {"category": "other", "summary": "via call_tool with writes allowed"}
        }),
    )
    .await;
    assert!(
        !is_err,
        "should succeed with call_tool_allow_writes: {body}"
    );
    assert_eq!(body["sent"], true);
}

#[tokio::test]
async fn handler_error_capture_attaches_pending_reports_at_most_once_per_session() {
    let dir = tempfile::tempdir().unwrap();
    let (handler, events) = make_handler(
        IssueReportingConfig::default(),
        dir.path().to_path_buf(),
        None,
        None,
    )
    .await;

    // Directly invoke capture_tool_error twice in one session (simulating two
    // failing tool calls): first returns the notice, second returns None while
    // still deduplicating/updating the pending store.
    let first = handler.issue_reporter().capture_tool_error(
        "add_wire",
        "handler_error",
        "unexpected error in /tmp/secret_proj/a.kicad_sch",
        None,
        None,
        Vec::new(),
    );
    assert!(
        first.is_some(),
        "first runtime error must produce pending_reports notice"
    );
    let first_notice = first.unwrap();
    assert_eq!(first_notice["reports"].as_array().unwrap().len(), 1);
    assert!(
        !first_notice.to_string().contains("secret_proj"),
        "notice payload must be scrubbed: {first_notice}"
    );

    let second = handler.issue_reporter().capture_tool_error(
        "add_wire",
        "handler_error",
        "unexpected error in /tmp/secret_proj/a.kicad_sch",
        None,
        None,
        Vec::new(),
    );
    assert!(
        second.is_none(),
        "pending_reports question must not be repeated in the same session"
    );
    assert!(
        events.lock().unwrap().is_empty(),
        "default mode must not send"
    );
    let pending = handler.issue_reporter().list_pending();
    assert_eq!(pending.len(), 1);
    assert_eq!(pending[0].count, 2);
}

#[tokio::test]
async fn duplicate_pending_ids_send_once_and_capture_false_silences_initialize() {
    let dir = tempfile::tempdir().unwrap();
    let store_dir = dir.path().join("pending");
    let config_file = dir.path().join("config.toml");

    let (handler, events) = make_handler(
        IssueReportingConfig::default(),
        store_dir.clone(),
        Some(config_file.clone()),
        None,
    )
    .await;
    let stored = handler
        .issue_reporter()
        .capture_panic(
            "dup test",
            Some("crates/copperpour-core/src/tools/x.rs:7:1"),
        )
        .unwrap();

    // First initialize shows pending_reports; second initialize in same session does not.
    let init1 = call_initialize(&handler).await;
    assert!(init1["instructions"]
        .as_str()
        .unwrap()
        .contains("pending_reports:"));
    let init2 = call_initialize(&handler).await;
    assert!(!init2["instructions"]
        .as_str()
        .unwrap()
        .contains("pending_reports:"));

    // Duplicate pending_ids must send once, not twice.
    let (is_err, body) = call_tool(
        &handler,
        "report_issue",
        json!({"pending_ids": [stored.id, stored.id]}),
    )
    .await;
    assert!(!is_err, "{body}");
    assert_eq!(events.lock().unwrap().len(), 1);

    // Unknown pending_ids returns invalid_argument.
    let (is_err, body) = call_tool(
        &handler,
        "report_issue",
        json!({"pending_ids": ["rep_does_not_exist"]}),
    )
    .await;
    assert!(is_err);
    assert_eq!(body["error"]["kind"], "invalid_argument");

    // Empty discard_pending_reports returns invalid_argument.
    let (is_err, body) = call_tool(&handler, "discard_pending_reports", json!({})).await;
    assert!(is_err);
    assert_eq!(body["error"]["kind"], "invalid_argument");

    // When capture = false, leftover pending reports on disk are NOT surfaced in initialize.
    let (handler_cap_off, _events_off) = make_handler(
        IssueReportingConfig {
            enabled: true,
            capture: false,
            automatic: false,
        },
        store_dir.clone(),
        Some(config_file),
        None,
    )
    .await;
    // Plant a report via handler (capture=true) then inspect via handler_cap_off.
    let _ = handler
        .issue_reporter()
        .capture_panic(
            "leftover",
            Some("crates/copperpour-core/src/tools/y.rs:8:1"),
        )
        .unwrap();
    let init_off = call_initialize(&handler_cap_off).await;
    assert!(
        !init_off["instructions"]
            .as_str()
            .unwrap()
            .contains("pending_reports:"),
        "capture=false must not surface pending_reports on initialize"
    );

    // discard_pending_reports(all: true) clears all remaining reports.
    let (is_err, body) = call_tool(&handler, "discard_pending_reports", json!({"all": true})).await;
    assert!(!is_err, "{body}");
    assert_eq!(body["remaining_count"], 0);
}
