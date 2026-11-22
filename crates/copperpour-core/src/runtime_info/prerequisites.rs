//! Read-only startup evidence. Operational tools still resolve and validate
//! their own targets at the point of use.

use super::{display_path, ipc_block, probe_command_version, VersionCommand, VersionProbe};
use crate::tools::{IpcEndpoint, IpcEndpointOrigin, ToolContext};
use copperpour_ipc::client::HealthProbePayloadError;
use copperpour_ipc::gen::kiapi::common::ApiStatusCode;
use copperpour_ipc::{unreachable_reason, ApiStatusError, IpcEditorKind};
use serde_json::{json, Value};
use std::path::Path;
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use tokio::sync::{watch, Mutex};

const IPC_BUDGET: Duration = Duration::from_secs(2);
const MIN_KICAD_MAJOR: u32 = 10;

#[derive(Default)]
pub(crate) struct PrerequisiteHealth {
    startup: OnceLock<StartupAssessment>,
    latest: Mutex<Option<Assessment>>,
}

struct StartupAssessment {
    endpoint_source: &'static str,
    report: watch::Receiver<Option<Assessment>>,
}

#[derive(Clone)]
pub(super) struct Assessment {
    pub(super) health: Value,
    pub(super) legacy_cli: Value,
}

impl PrerequisiteHealth {
    pub(crate) fn start(
        ctx: Arc<ToolContext>,
        endpoint_source: &'static str,
        endpoint: IpcEndpoint,
    ) {
        ctx.prerequisite_health.startup.get_or_init(|| {
            let (sender, report) = watch::channel(None);
            let ctx = ctx.clone();
            // A client's request may be canceled independently of startup.
            tokio::spawn(async move {
                let assessment = assess(
                    &ctx,
                    endpoint,
                    IpcEndpointOrigin::Startup,
                    Some(endpoint_source),
                    "startup",
                )
                .await;
                assessment.log();
                sender.send_replace(Some(assessment));
            });
            StartupAssessment {
                endpoint_source,
                report,
            }
        });
    }

    pub(super) async fn read(
        &self,
        ctx: &ToolContext,
        refresh: Option<(IpcEndpoint, IpcEndpointOrigin)>,
    ) -> Assessment {
        let mut latest = self.latest.lock().await;
        let startup = self.startup.get();
        let startup_source = startup.map(|startup| startup.endpoint_source);
        if latest.is_none() {
            let assessment = match startup {
                Some(startup) => {
                    let mut report = startup.report.clone();
                    report
                        .wait_for(Option::is_some)
                        .await
                        .ok()
                        .and_then(|assessment| assessment.clone())
                        .unwrap_or_else(Assessment::failed)
                }
                None => {
                    let (endpoint, origin) = ctx.diagnostic_ipc_endpoint();
                    assess(ctx, endpoint, origin, None, "diagnostic").await
                }
            };
            *latest = Some(assessment);
        }
        if let Some((endpoint, origin)) = refresh {
            *latest = Some(assess(ctx, endpoint, origin, startup_source, "refresh").await);
        }
        latest.as_ref().expect("assessment initialized").clone()
    }
}

impl Assessment {
    fn failed() -> Self {
        let failure = json!({
            "status": "unknown",
            "reason": "probe_failed",
            "guidance": "Refresh the prerequisite health report.",
        });
        Self {
            health: json!({
                "source": "startup",
                "checked_at_unix_ms": null,
                "kicad_cli": failure,
                "ipc": failure,
            }),
            legacy_cli: json!({"cli_path": null, "version": null, "probe_status": "executable_path_unavailable"}),
        }
    }

    fn log(&self) {
        let cli = &self.health["kicad_cli"];
        let ipc = &self.health["ipc"];
        tracing::info!(
            "KiCad prerequisite health: CLI {} ({}, {}), IPC {} ({}, {}); see get_installation_info",
            label(cli, "status"),
            label(cli, "reason"),
            label(cli, "source"),
            label(ipc, "status"),
            label(ipc, "reason"),
            label(ipc, "source"),
        );
    }
}

fn label<'a>(block: &'a Value, key: &str) -> &'a str {
    block[key].as_str().expect("assessment label")
}

async fn assess(
    ctx: &ToolContext,
    endpoint: IpcEndpoint,
    origin: IpcEndpointOrigin,
    startup_endpoint_source: Option<&str>,
    source: &str,
) -> Assessment {
    let checked_at = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .ok()
        .and_then(|elapsed| u64::try_from(elapsed.as_millis()).ok());
    let ipc_source = match origin {
        IpcEndpointOrigin::Startup if !endpoint.address().trim().is_empty() => {
            startup_endpoint_source
        }
        _ => None,
    };
    let ipc_location = ipc_block(&ctx.config.ipc_address, &endpoint, origin);
    let (cli, ipc) = tokio::join!(cli_health(&ctx.config.kicad_cli), ipc_health(endpoint));
    let mut ipc_health = ipc_location;
    if let Some(source) = ipc_source {
        ipc_health["source"] = json!(source);
    }
    ipc_health["status"] = json!(ipc.status);
    ipc_health["reason"] = json!(ipc.reason);
    ipc_health["api_status"] = json!(ipc.api_status);
    ipc_health["communication_confirmed"] = json!(ipc.communication_confirmed);
    ipc_health["version"] = json!(ipc.version);
    ipc_health["affected_tools"] = json!([
        "get_board_info (live source)",
        "route_trace",
        "place_component (live PCB)"
    ]);
    ipc_health["guidance"] = json!(ipc.guidance);

    Assessment {
        health: json!({
            "source": source,
            "checked_at_unix_ms": checked_at,
            "kicad_cli": cli.health,
            "ipc": ipc_health,
        }),
        legacy_cli: cli.legacy,
    }
}

struct CliAssessment {
    health: Value,
    legacy: Value,
}

async fn cli_health(configured: &str) -> CliAssessment {
    let configured = configured.trim();
    // The viewer and legacy diagnostics discover a CLI for an empty
    // setting. CLI-backed tools honor the blank value, which the health
    // finding reports separately from the viewer's fallback.
    let path = if configured.is_empty() {
        crate::kicad_install::find_cli("")
    } else {
        Some(crate::tools::cli::resolve_cli_executable(configured))
    };
    let probe = match path.as_deref() {
        Some(path) => probe_command_version(path, VersionCommand::KiCad).await,
        None => VersionProbe::not_found(),
    };
    let source = match path.as_deref() {
        _ if configured.is_empty() => "disabled",
        Some(path) if path != Path::new(configured) => "discovered",
        _ => "configured",
    };
    let version = probe.version.as_deref().and_then(kicad_version);
    let (status, reason, guidance) = match (source, probe.status, version) {
        ("disabled", _, _) => ("unavailable", "disabled", "Set kicad_cli to a KiCad 10 executable."),
        (_, "ok", Some((_, major))) if major >= MIN_KICAD_MAJOR => ("ready", "version_reported", "Use the CLI-backed tools as needed."),
        (_, "ok", Some(_)) => ("unavailable", "unsupported_version", "Configure a KiCad 10 or newer CLI."),
        (_, "timed_out", _) => ("unavailable", "timed_out", "Run kicad-cli --version under the account running Copperpour and check that it exits promptly."),
        (_, "not_found", _) | (_, "launch_failed", _) if probe.not_found => ("unavailable", "not_found", "Install KiCad 10 or correct kicad_cli or the server process PATH."),
        (_, "launch_failed", _) => ("unavailable", "unexecutable", "Check execute permissions and dependencies for the configured CLI."),
        (_, "nonzero_exit", _) => ("unavailable", "nonzero_exit", "Run the configured executable with --version and correct the reported failure."),
        _ => ("unavailable", "unrecognized_version", "Check that kicad_cli points to KiCad's CLI and reports its version."),
    };
    CliAssessment {
        health: json!({
            "status": status,
            "reason": reason,
            "source": source,
            "executable_path": path.as_deref().filter(|_| source != "disabled").map(display_path),
            "version": version.filter(|_| source != "disabled").map(|(version, _)| version),
            "affected_tools": ["run_erc", "run_drc (CLI backend)", "generate_netlist", "export_schematic_pdf"],
            "guidance": guidance,
        }),
        legacy: json!({
            "cli_path": path.as_deref().map(display_path),
            "probe_status": probe.status,
            "version": probe.version,
        }),
    }
}

fn kicad_version(line: &str) -> Option<(&str, u32)> {
    let version = line.strip_prefix("KiCad ").unwrap_or(line);
    let numeric = version.split(['-', '+']).next()?;
    let mut parts = numeric.split('.');
    let major = parts.next()?.parse().ok()?;
    parts.next()?.parse::<u32>().ok()?;
    parts.next()?.parse::<u32>().ok()?;
    parts.next().is_none().then_some((numeric, major))
}

struct IpcAssessment {
    status: &'static str,
    reason: &'static str,
    guidance: &'static str,
    version: Option<String>,
    api_status: Option<String>,
    communication_confirmed: bool,
}

impl IpcAssessment {
    fn new(status: &'static str, reason: &'static str, guidance: &'static str) -> Self {
        Self {
            status,
            reason,
            guidance,
            version: None,
            api_status: None,
            communication_confirmed: false,
        }
    }
}

async fn ipc_health(endpoint: IpcEndpoint) -> IpcAssessment {
    match &endpoint {
        IpcEndpoint::Ambiguous(_) => return ambiguous(),
        IpcEndpoint::Address(address) if address.trim().is_empty() => {
            return IpcAssessment::new("unavailable", "not_configured", "Check that KiCad is running, check its Enable KiCad API setting, and verify the endpoint.");
        }
        IpcEndpoint::Address(_) => {}
    }
    // Leave a margin for the async report deadline to return the socket's
    // typed result. Both requests use this one deadline.
    let deadline = Instant::now() + IPC_BUDGET - Duration::from_millis(100);
    let probe = tokio::task::spawn_blocking(move || probe_ipc(&endpoint, deadline));
    match tokio::time::timeout(IPC_BUDGET, probe).await {
        Ok(Ok(observation)) => observation,
        Ok(Err(_)) => IpcAssessment::new(
            "unavailable",
            "probe_failed",
            "Refresh and check the server log if the probe fails again.",
        ),
        Err(_) => IpcAssessment::new(
            "unknown",
            "timed_out",
            "Verify the endpoint and refresh when KiCad is responsive.",
        ),
    }
}

fn ambiguous() -> IpcAssessment {
    IpcAssessment::new("unavailable", "ambiguous_endpoint", "Use the endpoint shown by the intended KiCad instance or launch Copperpour from that instance.")
}

fn probe_ipc(endpoint: &IpcEndpoint, deadline: Instant) -> IpcAssessment {
    let client = endpoint.client();
    let version = match client.get_kicad_version_before(Some(deadline)) {
        Ok(version) => version,
        Err(error) => return ipc_failure(&error, false),
    };
    let mut result = if version.major < MIN_KICAD_MAJOR {
        IpcAssessment::new(
            "not_ready",
            "unsupported_version",
            "Connect to a supported KiCad 10 or newer instance.",
        )
    } else {
        match client.get_open_documents_before(IpcEditorKind::Pcb, Some(deadline)) {
            Ok(docs) if docs.is_empty() => IpcAssessment::new(
                "not_ready",
                "no_pcb_document",
                "Open the intended board in the PCB Editor.",
            ),
            Ok(_) => IpcAssessment::new(
                "ready",
                "pcb_documents_available",
                "Identify the exact board before using live PCB tools.",
            ),
            Err(error) => ipc_failure(&error, true),
        }
    };
    result.communication_confirmed = true;
    result.version = Some(format!(
        "{}.{}.{}",
        version.major, version.minor, version.patch
    ));
    result
}

fn ipc_failure(error: &anyhow::Error, communicated: bool) -> IpcAssessment {
    if let Some(status) = ApiStatusError::from_error(error) {
        let (reason, guidance) = match ApiStatusCode::try_from(status.code).ok() {
            Some(ApiStatusCode::AsNotReady | ApiStatusCode::AsBusy) => ("editor_not_ready", "Allow KiCad to finish starting or complete any open dialog, then refresh."),
            Some(ApiStatusCode::AsTokenMismatch) => ("token_mismatch", "Launch Copperpour from the intended KiCad session or verify its configured endpoint."),
            Some(ApiStatusCode::AsUnhandled) if communicated => ("pcb_editor_unavailable", "Open the PCB Editor and verify that Copperpour uses its endpoint."),
            _ if status.is_unsupported() => ("protocol_unsupported", "Verify that the endpoint belongs to a supported KiCad instance."),
            _ => ("request_refused", "Check the reported KiCad API status and verify the intended endpoint."),
        };
        return IpcAssessment {
            api_status: Some(status.code_name.clone()),
            communication_confirmed: true,
            ..IpcAssessment::new("not_ready", reason, guidance)
        };
    }
    if error.downcast_ref::<HealthProbePayloadError>().is_some() {
        return IpcAssessment {
            communication_confirmed: true,
            ..IpcAssessment::new(
                "not_ready",
                "incompatible_reply",
                "Verify that the endpoint belongs to a supported KiCad instance.",
            )
        };
    }
    if let Some(reason) = unreachable_reason(error) {
        let status = if communicated {
            "unknown"
        } else {
            "unreachable"
        };
        let guidance = match reason {
            copperpour_ipc::UnreachableReason::AccessDenied => "Verify the endpoint is accessible to the same account running Copperpour.",
            _ => "Verify the configured endpoint, whether KiCad is running, and its Enable KiCad API setting.",
        };
        return IpcAssessment::new(status, reason.as_str(), guidance);
    }
    IpcAssessment::new("unknown", "reply_unverified", "Verify that the endpoint belongs to a supported and responsive KiCad instance, then refresh.")
}

#[cfg(test)]
mod tests {
    use crate::mcp::handler::McpHandler;
    use crate::test_support::MockIpcServer;
    use crate::tools::{pcb_board::board_mock::board_document, IpcEndpoint, ServerConfig};
    use copperpour_ipc::{
        builders::{any_type_name, pack_any},
        gen::kiapi::common::{commands, types, ApiResponse, ApiResponseStatus, ApiStatusCode},
    };
    use prost::Message;
    use serde_json::{json, Value};
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;

    async fn health(handler: &McpHandler, arguments: Value) -> Value {
        let response = handler
            .handle_message(json!({
                "jsonrpc": "2.0", "id": 1, "method": "tools/call",
                "params": {"name": "get_installation_info", "arguments": arguments},
            }))
            .await
            .expect("tools/call response");
        let response = serde_json::to_value(response).unwrap();
        assert_ne!(response["result"]["isError"], true, "{response}");
        serde_json::from_str::<Value>(response["result"]["content"][0]["text"].as_str().unwrap())
            .unwrap()["prerequisite_health"]
            .clone()
    }

    #[tokio::test]
    async fn prerequisite_health_observes_protocol_and_pcb_documents_read_only() {
        let dir = tempfile::tempdir().unwrap();
        let board = dir.path().join("observed.kicad_pcb");
        let contents =
            include_bytes!("../../../copperpour-sexp/tests/fixtures/gr_poly_outline.kicad_pcb");
        std::fs::write(&board, contents).unwrap();
        let document = board_document(board.to_str().unwrap());
        let mode = Arc::new(AtomicUsize::new(0));
        let requests = Arc::new(AtomicUsize::new(0));
        let mock = MockIpcServer::spawn("prerequisite-health", {
            let mode = mode.clone();
            let requests = requests.clone();
            move |request| {
                requests.fetch_add(1, Ordering::SeqCst);
                let command = request.message.unwrap();
                let mode = mode.load(Ordering::SeqCst);
                let (status, message) = match (any_type_name(&command), mode) {
                    ("kiapi.common.commands.GetVersion", 3) => (ApiStatusCode::AsNotReady, None),
                    ("kiapi.common.commands.GetVersion", 4) => {
                        (ApiStatusCode::AsUnimplemented, None)
                    }
                    ("kiapi.common.commands.GetVersion", 6) => (
                        ApiStatusCode::AsOk,
                        Some(pack_any(
                            &commands::GetVersionResponse { version: None },
                            "kiapi.common.commands.GetVersionResponse",
                        )),
                    ),
                    ("kiapi.common.commands.GetVersion", _) => (
                        ApiStatusCode::AsOk,
                        Some(pack_any(
                            &commands::GetVersionResponse {
                                version: Some(types::KiCadVersion {
                                    major: 10,
                                    minor: 0,
                                    patch: 5,
                                    ..Default::default()
                                }),
                            },
                            "kiapi.common.commands.GetVersionResponse",
                        )),
                    ),
                    ("kiapi.common.commands.GetOpenDocuments", _) => {
                        let args =
                            commands::GetOpenDocuments::decode(command.value.as_slice()).unwrap();
                        assert_eq!(args.r#type, types::DocumentType::DoctypePcb as i32);
                        match mode {
                            2 => (ApiStatusCode::AsUnhandled, None),
                            5 => (
                                ApiStatusCode::AsOk,
                                Some(pack_any(&commands::Ping {}, "kiapi.common.commands.Ping")),
                            ),
                            _ => (
                                ApiStatusCode::AsOk,
                                Some(pack_any(
                                    &commands::GetOpenDocumentsResponse {
                                        documents: (mode == 0)
                                            .then(|| document.clone())
                                            .into_iter()
                                            .collect(),
                                    },
                                    "kiapi.common.commands.GetOpenDocumentsResponse",
                                )),
                            ),
                        }
                    }
                    (unexpected, _) => panic!("unexpected health probe request {unexpected}"),
                };
                ApiResponse {
                    status: Some(ApiResponseStatus {
                        status: status as i32,
                        error_message: "private peer diagnostic".to_string(),
                    }),
                    header: None,
                    message,
                }
            }
        });
        let handler = McpHandler::new(ServerConfig {
            kicad_cli: dir
                .path()
                .join("missing-cli")
                .to_string_lossy()
                .into_owned(),
            ipc_address: mock.address().to_owned(),
            project_dir: Some(dir.path().to_path_buf()),
            ..ServerConfig::default()
        })
        .await
        .unwrap();
        handler
            .start_prerequisite_checks("config", IpcEndpoint::Address(mock.address().to_owned()));

        tokio::time::timeout(std::time::Duration::from_secs(3), async {
            while requests.load(Ordering::SeqCst) < 2 {
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("startup should probe before the first diagnostic call");

        let first = health(&handler, json!({})).await;
        assert_eq!(first["source"], "startup");
        assert_eq!(first["ipc"]["status"], "ready", "{first}");
        assert_eq!(first["ipc"]["version"], "10.0.5");
        assert_eq!(first["ipc"]["communication_confirmed"], true);
        assert_eq!(requests.load(Ordering::SeqCst), 2);

        mode.store(1, Ordering::SeqCst);
        assert_eq!(
            health(&handler, json!({"refresh_prerequisite_health": false})).await,
            first
        );
        assert_eq!(requests.load(Ordering::SeqCst), 2);
        for (scenario, reason, status, request_count) in [
            (1, "no_pcb_document", Value::Null, 2),
            (2, "pcb_editor_unavailable", json!("AS_UNHANDLED"), 2),
            (3, "editor_not_ready", json!("AS_NOT_READY"), 1),
            (4, "protocol_unsupported", json!("AS_UNIMPLEMENTED"), 1),
            (5, "incompatible_reply", Value::Null, 2),
            (6, "incompatible_reply", Value::Null, 1),
        ] {
            mode.store(scenario, Ordering::SeqCst);
            requests.store(0, Ordering::SeqCst);
            let refreshed = health(&handler, json!({"refresh_prerequisite_health": true})).await;
            assert_eq!(refreshed["source"], "refresh");
            assert_eq!(refreshed["ipc"]["status"], "not_ready", "{refreshed}");
            assert_eq!(refreshed["ipc"]["reason"], reason);
            assert_eq!(refreshed["ipc"]["api_status"], status);
            assert_eq!(refreshed["ipc"]["communication_confirmed"], true);
            assert_eq!(requests.load(Ordering::SeqCst), request_count);
            assert!(!refreshed.to_string().contains("private peer diagnostic"));
        }
        assert_eq!(std::fs::read(&board).unwrap(), contents.as_slice());
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 1);
    }
}
