//! Shared, structured recovery guidance for runtime IPC failures (#525).
//!
//! This module is the single owner for mapping typed IPC classifications
//! ([`PingOutcome`], [`UnreachableReason`], [`BoardTargetError`],
//! [`IpcFailure`], [`BoardEditorAbsence`], [`IpcEndpoint`]) to
//! [`RecoveryGuidance`]. It never parses error message text to guess causes,
//! never performs I/O, config writes, app launches, lock removal, or retries,
//! and preserves unknown causes and `retry_safe: false` whenever outcome or
//! safety is not established.

use crate::mcp::error::{RecoveryGuidance, RecoveryStep};
use crate::tools::{BoardEditorAbsence, IpcEndpoint};
use copperpour_ipc::gen::kiapi::common::ApiStatusCode;
use copperpour_ipc::{BoardTargetError, IpcFailure, PingOutcome, UnreachableReason};
use serde_json::json;

const TROUBLESHOOTING_BASE: &str =
    "https://github.com/Copperpour/Copperpour/blob/main/docs/TROUBLESHOOTING.md";

fn troubleshooting_url(anchor: &str) -> String {
    format!("{TROUBLESHOOTING_BASE}#{anchor}")
}

fn agent_step(tool: &str, action: impl Into<String>) -> RecoveryStep {
    RecoveryStep {
        actor: "agent".to_string(),
        action: action.into(),
        tool: Some(tool.to_string()),
    }
}

fn agent_note(action: impl Into<String>) -> RecoveryStep {
    RecoveryStep {
        actor: "agent".to_string(),
        action: action.into(),
        tool: None,
    }
}

fn user_step(action: impl Into<String>) -> RecoveryStep {
    RecoveryStep {
        actor: "user".to_string(),
        action: action.into(),
        tool: None,
    }
}

fn redact_opt_address(address: Option<&str>) -> Option<String> {
    address
        .filter(|addr| !addr.is_empty())
        .map(copperpour_ipc::redact_endpoint)
}

fn redact_candidates(candidates: &[String]) -> Vec<String> {
    candidates
        .iter()
        .map(|candidate| copperpour_ipc::redact_endpoint(candidate))
        .collect()
}

/// Recovery guidance for an unreachable IPC endpoint, shared between
/// `PingOutcome::Unreachable`, `IpcFailure::Unreachable`, and
/// `IpcEndpoint::unavailable_reason`.
pub(crate) fn for_unreachable(
    reason: UnreachableReason,
    address: Option<&str>,
    candidates: &[String],
    message: &str,
) -> RecoveryGuidance {
    let redacted_address = redact_opt_address(address);
    let redacted_candidates = redact_candidates(candidates);

    match reason {
        UnreachableReason::NotConfigured if !redacted_candidates.is_empty() => RecoveryGuidance {
            cause: "ambiguous_endpoint".to_string(),
            observed: "Multiple KiCad IPC endpoints were detected and not exactly one answered a Ping, so no endpoint was selected automatically.".to_string(),
            evidence: json!({
                "kind": reason.as_str(),
                "candidates": redacted_candidates,
                "message": message,
            }),
            mutation_outcome: "none".to_string(),
            retry_safe: false,
            retry_prerequisites: vec![
                "Set ipc_address to the exact endpoint listed in KiCad Preferences > Plugins ('Enable KiCad API'), or quit the extra KiCad instance.".to_string(),
            ],
            next_steps: vec![
                user_step("Inspect KiCad Preferences > Plugins ('Enable KiCad API') and configure ipc_address to the intended endpoint (or quit extra KiCad instances), then restart the MCP client session if configuration changed."),
                agent_step("check_kicad_ui", "Call check_kicad_ui after the endpoint is selected to confirm IPC readiness."),
            ],
            troubleshooting_url: Some(troubleshooting_url("kicad-ipc-socket-path-not-configured")),
        },
        UnreachableReason::NotConfigured => RecoveryGuidance {
            cause: "not_configured".to_string(),
            observed: "No KiCad IPC endpoint address is configured or discovered.".to_string(),
            evidence: json!({
                "kind": reason.as_str(),
                "message": message,
            }),
            mutation_outcome: "none".to_string(),
            retry_safe: false,
            retry_prerequisites: vec![
                "Enable the KiCad IPC API and configure ipc_address or KICAD_API_SOCKET (or start KiCad so auto-discovery can find its socket).".to_string(),
            ],
            next_steps: vec![
                user_step("In KiCad, enable Preferences > Plugins > 'Enable KiCad API' and either leave auto-discovery active or paste the listed ipc:// address into Copperpour's settings."),
                agent_step("check_kicad_ui", "Call check_kicad_ui once KiCad is running and configured to verify IPC responsiveness."),
            ],
            troubleshooting_url: Some(troubleshooting_url("kicad-ipc-socket-path-not-configured")),
        },
        UnreachableReason::NoListener => {
            let mut evidence = json!({
                "kind": reason.as_str(),
                "message": message,
            });
            if let Some(addr) = redacted_address {
                evidence["address"] = json!(addr);
            }
            RecoveryGuidance {
                cause: "no_listener".to_string(),
                observed: "Nothing is listening at the configured or discovered KiCad IPC endpoint.".to_string(),
                evidence,
                mutation_outcome: "none".to_string(),
                retry_safe: false,
                retry_prerequisites: vec![
                    "Verify KiCad is running with the IPC API enabled and that the configured address matches KiCad's 'Listening on' address.".to_string(),
                ],
                next_steps: vec![
                    user_step("Verify KiCad is open, check Preferences > Plugins > 'Enable KiCad API', and compare KiCad's 'Listening on' address against the configured endpoint."),
                    agent_step("check_kicad_ui", "Call check_kicad_ui (or launch_kicad_ui if KiCad is not running) to verify editor and API readiness."),
                ],
                troubleshooting_url: Some(troubleshooting_url("kicad-is-running-but-ipc_failure-says-it-did-not-answer")),
            }
        }
        UnreachableReason::AccessDenied => {
            let mut evidence = json!({
                "kind": reason.as_str(),
                "message": message,
            });
            if let Some(addr) = redacted_address {
                evidence["address"] = json!(addr);
            }
            RecoveryGuidance {
                cause: "access_denied".to_string(),
                observed: "The operating system refused this account access to the KiCad IPC endpoint.".to_string(),
                evidence,
                mutation_outcome: "none".to_string(),
                retry_safe: false,
                retry_prerequisites: vec![
                    "Run Copperpour under an operating-system account permitted to access KiCad's IPC endpoint.".to_string(),
                ],
                next_steps: vec![
                    user_step("Run Copperpour under the same OS account as KiCad (a sandboxed client or restrictive ACL may be blocking access). If your client runs in a separate-account sandbox, run Copperpour outside the sandbox over loopback HTTP (127.0.0.1)."),
                    agent_step("get_installation_info", "Call get_installation_info if needed to inspect the server runtime and redacted endpoint, then call check_kicad_ui after restarting under the matching account."),
                ],
                troubleshooting_url: Some(troubleshooting_url("access_denied-from-a-sandboxed-ai-client")),
            }
        }
        UnreachableReason::HandshakeFailed => {
            let mut evidence = json!({
                "kind": reason.as_str(),
                "message": message,
            });
            if let Some(addr) = redacted_address {
                evidence["address"] = json!(addr);
            }
            RecoveryGuidance {
                cause: "handshake_failed".to_string(),
                observed: "A listener accepted the connection at the IPC endpoint but did not complete NNG's handshake.".to_string(),
                evidence,
                mutation_outcome: "none".to_string(),
                retry_safe: false,
                retry_prerequisites: vec![
                    "Confirm which process owns the IPC endpoint and ensure KiCad binds it cleanly.".to_string(),
                ],
                next_steps: vec![
                    user_step("Check which process holds the configured IPC address, close any conflicting listener, and restart KiCad if needed so KiCad can bind the endpoint (do not delete active endpoint files automatically)."),
                    agent_step("check_kicad_ui", "Call check_kicad_ui with timeout_seconds above 10 to verify whether handshake negotiation completes."),
                ],
                troubleshooting_url: Some(troubleshooting_url("kicad-is-running-but-ipc_failure-says-it-did-not-answer")),
            }
        }
        UnreachableReason::TransportError => {
            let mut evidence = json!({
                "kind": reason.as_str(),
                "message": message,
            });
            if let Some(addr) = redacted_address {
                evidence["address"] = json!(addr);
            }
            RecoveryGuidance {
                cause: "unknown".to_string(),
                observed: "The IPC connection failed before a request reached KiCad for an unclassified transport reason.".to_string(),
                evidence,
                mutation_outcome: "none".to_string(),
                retry_safe: false,
                retry_prerequisites: vec![
                    "Inspect the transport error and verify endpoint health before retrying.".to_string(),
                ],
                next_steps: vec![
                    agent_step("check_kicad_ui", "Call check_kicad_ui to re-check endpoint responsiveness."),
                    user_step("Inspect the reported transport error and verify the KiCad IPC endpoint configuration."),
                ],
                troubleshooting_url: Some(troubleshooting_url("kicad-is-running-but-ipc_failure-says-it-did-not-answer")),
            }
        }
    }
}

/// Recovery guidance for a [`PingOutcome`], or `None` when responsive.
pub(crate) fn for_ping_outcome(
    endpoint: Option<&IpcEndpoint>,
    outcome: &PingOutcome,
) -> Option<RecoveryGuidance> {
    let address = endpoint.map(IpcEndpoint::address);
    let endpoint_candidates: &[String] = match endpoint {
        Some(IpcEndpoint::Ambiguous(candidates)) => candidates,
        _ => &[],
    };

    match outcome {
        PingOutcome::Responsive => None,
        PingOutcome::Unreachable { reason, message } => Some(for_unreachable(
            *reason,
            address,
            endpoint_candidates,
            message,
        )),
        PingOutcome::RequestFailed {
            message,
            status: Some(ApiStatusCode::AsNotReady),
        } => {
            let mut evidence = json!({
                "kind": "request_failed",
                "status": "AS_NOT_READY",
                "message": message,
            });
            if let Some(addr) = redact_opt_address(address) {
                evidence["address"] = json!(addr);
            }
            Some(RecoveryGuidance {
                cause: "kicad_not_ready".to_string(),
                observed: "KiCad received the Ping and answered AS_NOT_READY; it may still be loading or may be held by a blocking modal dialog.".to_string(),
                evidence,
                mutation_outcome: "none".to_string(),
                retry_safe: false,
                retry_prerequisites: vec![
                    "Wait for KiCad to finish starting, or close any blocking modal dialog (such as the first-run wizard or a message box).".to_string(),
                ],
                next_steps: vec![
                    user_step("If KiCad is not still starting, check the KiCad window and close any blocking modal dialog."),
                    agent_step("check_kicad_ui", "Wait briefly and call check_kicad_ui to verify KiCad becomes ready."),
                ],
                troubleshooting_url: Some(troubleshooting_url("kicad-is-running-but-ipc_failure-says-it-did-not-answer")),
            })
        }
        PingOutcome::RequestFailed { message, status } => {
            let mut evidence = json!({
                "kind": "request_failed",
                "message": message,
            });
            if let Some(code) = status {
                evidence["status"] = json!(format!("{code:?}"));
            }
            if let Some(addr) = redact_opt_address(address) {
                evidence["address"] = json!(addr);
            }
            Some(RecoveryGuidance {
                cause: "unknown".to_string(),
                observed: "The Ping request did not complete and may have reached the endpoint; a receive timeout or unclassified status does not prove whether KiCad is loading, blocked by a dialog, or unresponsive.".to_string(),
                evidence,
                mutation_outcome: "none".to_string(),
                retry_safe: false,
                retry_prerequisites: vec![
                    "Verify the endpoint is responsive before relying on it for operations.".to_string(),
                ],
                next_steps: vec![
                    agent_step("check_kicad_ui", "Call check_kicad_ui with a sufficient timeout to re-check endpoint responsiveness."),
                    user_step("If the failure persists, inspect KiCad and verify what process holds the configured IPC address."),
                ],
                troubleshooting_url: Some(troubleshooting_url("kicad-is-running-but-ipc_failure-says-it-did-not-answer")),
            })
        }
    }
}

/// Recovery guidance for an [`IpcEndpoint`] that is unavailable before any
/// request is sent.
pub(crate) fn for_unavailable_endpoint(endpoint: &IpcEndpoint, message: &str) -> RecoveryGuidance {
    match endpoint {
        IpcEndpoint::Ambiguous(candidates) => {
            for_unreachable(UnreachableReason::NotConfigured, None, candidates, message)
        }
        IpcEndpoint::Address(address) => for_unreachable(
            UnreachableReason::NotConfigured,
            Some(address),
            &[],
            message,
        ),
    }
}

/// Recovery guidance for a [`BoardTargetError`] refusal.
pub(crate) fn for_board_target_error(error: &BoardTargetError) -> RecoveryGuidance {
    match error {
        BoardTargetError::NoOpenDocuments { requested } => RecoveryGuidance {
            cause: "no_open_documents".to_string(),
            observed: "KiCad IPC is reachable, but KiCad reported no open PCB documents.".to_string(),
            evidence: json!({
                "requested": requested,
                "open_documents": [],
            }),
            mutation_outcome: "none".to_string(),
            retry_safe: false,
            retry_prerequisites: vec![
                "Open the requested board in KiCad's PCB Editor.".to_string(),
            ],
            next_steps: vec![
                user_step("Open the requested .kicad_pcb file in KiCad's PCB Editor."),
                agent_step("open_project", "Call open_project to confirm the requested board appears in open_boards before retrying."),
            ],
            troubleshooting_url: Some(troubleshooting_url("pcb-tools-return-kicad-must-be-running-with-the-board-loaded")),
        },
        BoardTargetError::WrongDocument {
            requested,
            open_documents,
        } => RecoveryGuidance {
            cause: "wrong_document".to_string(),
            observed: "The requested board is not among the PCB documents currently open in KiCad.".to_string(),
            evidence: json!({
                "requested": requested,
                "open_documents": open_documents,
            }),
            mutation_outcome: "none".to_string(),
            retry_safe: false,
            retry_prerequisites: vec![
                "Open the requested board in KiCad or confirm which open board should be targeted.".to_string(),
            ],
            next_steps: vec![
                user_step("Open the requested board in KiCad's PCB Editor, or confirm which open document should be targeted instead."),
                agent_step("open_project", "Do not switch targets automatically; call open_project to verify the target board is open before retrying."),
            ],
            troubleshooting_url: Some(troubleshooting_url("pcb-tools-return-kicad-must-be-running-with-the-board-loaded")),
        },
        BoardTargetError::AmbiguousDocument {
            requested,
            candidates,
        } => RecoveryGuidance {
            cause: "ambiguous_document".to_string(),
            observed: "More than one open PCB document matched the requested board, so no target was chosen automatically.".to_string(),
            evidence: json!({
                "requested": requested,
                "candidates": candidates,
            }),
            mutation_outcome: "none".to_string(),
            retry_safe: false,
            retry_prerequisites: vec![
                "Disambiguate the open PCB documents so one exact document identity matches.".to_string(),
            ],
            next_steps: vec![
                agent_step("get_editor_state", "Do not switch targets automatically; call get_editor_state to inspect the open document identities."),
                user_step("Close duplicate open boards or specify the exact unambiguous board path."),
            ],
            troubleshooting_url: Some(troubleshooting_url("pcb-tools-return-kicad-must-be-running-with-the-board-loaded")),
        },
        BoardTargetError::UnresolvedDocumentIdentities { requested, reasons } => RecoveryGuidance {
            cause: "unresolved_document_identities".to_string(),
            observed: "KiCad's open documents could not be compared safely with the requested board.".to_string(),
            evidence: json!({
                "requested": requested,
                "reasons": reasons,
            }),
            mutation_outcome: "none".to_string(),
            retry_safe: false,
            retry_prerequisites: vec![
                "Ensure the requested board is saved with a concrete filesystem path and open in KiCad.".to_string(),
            ],
            next_steps: vec![
                agent_step("get_editor_state", "Call get_editor_state to inspect reported document identities without modifying any file."),
                user_step("Verify the board is saved to disk and open in KiCad's PCB Editor."),
            ],
            troubleshooting_url: Some(troubleshooting_url("pcb-tools-return-kicad-must-be-running-with-the-board-loaded")),
        },
        BoardTargetError::StaleDocument {
            requested,
            previously_bound,
            open_documents,
        } => RecoveryGuidance {
            cause: "stale_document".to_string(),
            observed: "The previously bound board document is no longer uniquely open in KiCad.".to_string(),
            evidence: json!({
                "requested": requested,
                "previously_bound": previously_bound,
                "open_documents": open_documents,
            }),
            mutation_outcome: "none".to_string(),
            retry_safe: false,
            retry_prerequisites: vec![
                "Reopen or restore the exact requested board in KiCad's PCB Editor.".to_string(),
            ],
            next_steps: vec![
                user_step("Reopen the requested board in KiCad's PCB Editor."),
                agent_step("open_project", "Call open_project to confirm the requested board is uniquely open before retrying."),
            ],
            troubleshooting_url: Some(troubleshooting_url("pcb-tools-return-kicad-must-be-running-with-the-board-loaded")),
        },
    }
}

/// Recovery guidance for `IpcOutcomeUnknown`.
pub(crate) fn for_ipc_outcome_unknown(reason: &str) -> RecoveryGuidance {
    RecoveryGuidance {
        cause: "ipc_outcome_unknown".to_string(),
        observed: "Copperpour sent an IPC request but could not establish its outcome, or an attempted rollback failed.".to_string(),
        evidence: json!({
            "reason": reason,
        }),
        mutation_outcome: "unknown".to_string(),
        retry_safe: false,
        retry_prerequisites: vec![
            "Inspect the live board in KiCad and reconcile any partially or fully applied changes before retrying.".to_string(),
        ],
        next_steps: vec![
            user_step("Inspect the requested board in KiCad's PCB Editor and reconcile what was applied; do not edit the saved file as a workaround."),
            agent_step("get_board_info", "Do not retry the mutation automatically. Preserve this diagnostic and inspect the live board state once reviewed."),
        ],
        troubleshooting_url: Some(troubleshooting_url("ipc-operation-returned-ipc_outcome_unknown")),
    }
}

/// Recovery guidance for `IpcBatchRecovered`.
pub(crate) fn for_ipc_batch_recovered(reason: &str) -> RecoveryGuidance {
    RecoveryGuidance {
        cause: "ipc_batch_recovered".to_string(),
        observed: "The apply was aborted after a late IPC reply, the commit was confirmed dropped, and the live snapshot matched the pre-apply snapshot.".to_string(),
        evidence: json!({
            "reason": reason,
            "drop_confirmed": true,
        }),
        mutation_outcome: "unchanged".to_string(),
        retry_safe: false,
        retry_prerequisites: vec![
            "Review the live board and run a fresh dry run before deciding whether to apply again.".to_string(),
        ],
        next_steps: vec![
            user_step("Review the live board in KiCad to confirm its state before re-applying."),
            agent_note("Do not replay the previous apply automatically; run a fresh dry run first."),
        ],
        troubleshooting_url: Some(troubleshooting_url("schematic-sync-returned-ipc_batch_recovered")),
    }
}

/// Recovery guidance for a [`BoardEditorAbsence`] when KiCad answered without
/// serving a PCB document.
pub(crate) fn for_board_editor_absence(
    absence: BoardEditorAbsence,
    message: &str,
) -> RecoveryGuidance {
    match absence {
        BoardEditorAbsence::NoPcbEditor => RecoveryGuidance {
            cause: absence.reason_code().to_string(),
            observed: "KiCad IPC is reachable, but no PCB editor is open at this endpoint.".to_string(),
            evidence: json!({
                "reason": message,
            }),
            mutation_outcome: "none".to_string(),
            retry_safe: false,
            retry_prerequisites: vec![
                "Open the requested board in KiCad's PCB Editor.".to_string(),
            ],
            next_steps: vec![
                user_step("Open the requested .kicad_pcb board in KiCad's PCB Editor."),
                agent_step("open_project", "Call open_project to confirm the board is open before retrying."),
            ],
            troubleshooting_url: Some(troubleshooting_url("pcb-tools-return-kicad-must-be-running-with-the-board-loaded")),
        },
        BoardEditorAbsence::OpenDocumentsUnimplemented => RecoveryGuidance {
            cause: "unknown".to_string(),
            observed: "KiCad IPC answered, but this KiCad build does not implement GetOpenDocuments, so whether a PCB editor holds the board was not established.".to_string(),
            evidence: json!({
                "reason": message,
            }),
            mutation_outcome: "none".to_string(),
            retry_safe: false,
            retry_prerequisites: vec![
                "Verify which KiCad version and editor are running at the configured endpoint.".to_string(),
            ],
            next_steps: vec![
                user_step("Verify KiCad version and open editors at the configured IPC endpoint."),
                agent_step("check_kicad_ui", "Call check_kicad_ui to inspect endpoint responsiveness."),
            ],
            troubleshooting_url: Some(troubleshooting_url("pcb-tools-return-kicad-must-be-running-with-the-board-loaded")),
        },
    }
}

/// Recovery guidance for any [`IpcFailure`].
pub(crate) fn for_ipc_failure(failure: &IpcFailure) -> RecoveryGuidance {
    match failure {
        IpcFailure::Unreachable {
            reason,
            message,
            candidates,
        } => for_unreachable(*reason, None, candidates, message),
        IpcFailure::Target { error, .. } => for_board_target_error(error),
        IpcFailure::Uncertain(message) => for_ipc_outcome_unknown(message),
        IpcFailure::Recovered(message) => for_ipc_batch_recovered(message),
        IpcFailure::ItemRejected { message, rejection, rollback } => {
            use copperpour_ipc::client::{ItemRejection, ItemRejectionRollback};
            let mut guidance = for_ipc_outcome_unknown(message);
            let (rollback_status, observed) = match rollback {
                ItemRejectionRollback::NotEstablished => (
                    "not_established",
                    "KiCad rejected one or more items. Other changes may have applied.",
                ),
                ItemRejectionRollback::Confirmed { commit_id } => {
                    guidance.evidence["commit_id"] = json!(commit_id);
                    ("confirmed", "KiCad rejected one or more items and acknowledged dropping the commit. That does not verify the overall board outcome.")
                }
                ItemRejectionRollback::Unconfirmed => (
                    "unconfirmed",
                    "KiCad rejected one or more items, but rollback was not confirmed.",
                ),
            };
            if !matches!(rollback, ItemRejectionRollback::Unconfirmed) {
                guidance.cause = "kicad_api_rejection".into();
                guidance.troubleshooting_url = Some(troubleshooting_url("kicad-rejected-items-during-an-ipc-write"));
            }
            guidance.observed = observed.into();
            guidance.evidence["rollback"] = json!(rollback_status);
            match rejection.as_ref() {
                ItemRejection::Create { requested_count, reported_created_count, rejected_items, .. } => {
                    guidance.evidence["command"] = json!("CreateItems");
                    guidance.evidence["requested_count"] = json!(requested_count);
                    guidance.evidence["reported_created_count"] = json!(reported_created_count);
                    guidance.evidence["rejected_items"] = json!(rejected_items.iter().map(|row| json!({
                        "reply_index": row.reply_index, "status": row.status.as_str_name(), "error_message": row.error_message,
                    })).collect::<Vec<_>>());
                }
                ItemRejection::Delete { reported_deleted_kiids, rejected_items, unclassified_kiids, unreported_kiids, .. } => {
                    guidance.evidence["command"] = json!("DeleteItems");
                    guidance.evidence["reported_deleted_kiids"] = json!(reported_deleted_kiids);
                    guidance.evidence["unclassified_kiids"] = json!(unclassified_kiids);
                    guidance.evidence["unreported_kiids"] = json!(unreported_kiids);
                    guidance.evidence["rejected_items"] = json!(rejected_items.iter().map(|row| json!({
                        "kiid": row.kiid, "status": row.status.as_str_name(),
                    })).collect::<Vec<_>>());
                }
            }
            guidance
        }
        IpcFailure::Preflight(refusal) => for_preflight_refusal(refusal),
        IpcFailure::Rejected { message, status } => RecoveryGuidance {
            cause: if status.is_some() { "kicad_api_rejection" } else { "unknown" }.to_string(),
            observed: if status.is_some() {
                "KiCad answered with an IPC error status."
            } else {
                "The operation failed, but the source of the failure and what was applied are not known."
            }.to_string(),
            evidence: match status {
                Some(status) => json!({ "message": message, "status": status }),
                None => json!({ "message": message }),
            },
            mutation_outcome: "unknown".to_string(),
            retry_safe: false,
            retry_prerequisites: vec![
                "Inspect the failure message and live board state before deciding whether to retry.".to_string(),
            ],
            next_steps: vec![
                agent_note("Inspect the reported error and the live board before retrying."),
            ],
            troubleshooting_url: None,
        },
    }
}

pub(crate) fn for_preflight_refusal(
    refusal: &copperpour_ipc::PreflightRefusal,
) -> RecoveryGuidance {
    RecoveryGuidance {
        cause: "preflight_refusal".to_string(),
        observed: "Copperpour refused the request before applying any change.".to_string(),
        evidence: match &refusal.kicad_status {
            Some(status) => json!({ "reason": refusal.reason, "status": status }),
            None => json!({ "reason": refusal.reason }),
        },
        mutation_outcome: "none".to_string(),
        retry_safe: false,
        retry_prerequisites: vec!["Address the reported reason before retrying.".to_string()],
        next_steps: vec![agent_note(
            "Use the reported reason to correct the request or board state. Nothing was applied.",
        )],
        troubleshooting_url: None,
    }
}

/// Human-readable message for an unreachable IPC failure that matches the
/// actual [`UnreachableReason`] rather than telling a user with an open board
/// to open it again.
pub(crate) fn unreachable_summary(
    reason: UnreachableReason,
    candidates: &[String],
    message: &str,
) -> String {
    match reason {
        UnreachableReason::NotConfigured if !candidates.is_empty() => format!(
            "Multiple KiCad IPC endpoints were found ({}); set ipc_address to select one: {message}",
            redact_candidates(candidates).join(", ")
        ),
        UnreachableReason::NotConfigured => {
            format!("KiCad IPC endpoint is not configured: {message}")
        }
        UnreachableReason::NoListener => format!(
            "Nothing is listening at the KiCad IPC endpoint (check editor/API readiness and configured address): {message}"
        ),
        UnreachableReason::AccessDenied => format!(
            "OS refused access to the KiCad IPC endpoint (run Copperpour under the same OS account as KiCad or over loopback HTTP): {message}"
        ),
        UnreachableReason::HandshakeFailed => format!(
            "Listener at the KiCad IPC endpoint did not complete NNG handshake (check endpoint ownership): {message}"
        ),
        UnreachableReason::TransportError => {
            format!("KiCad IPC transport failed before reaching KiCad: {message}")
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mcp::protocol::ToolContent;
    use crate::tools::{
        ipc_failure_evidence_for_endpoint, ipc_failure_result, ipc_unserved_result,
    };

    fn parse_result(result: &crate::mcp::protocol::CallToolResult) -> serde_json::Value {
        let ToolContent::Text { text } = &result.content[0] else {
            panic!("expected text content");
        };
        serde_json::from_str(text).expect("valid JSON tool body")
    }

    #[test]
    fn responsive_ping_has_no_recovery_guidance() {
        assert!(for_ping_outcome(None, &PingOutcome::Responsive).is_none());
    }

    #[test]
    fn ping_unreachable_variants_map_to_distinct_causes_and_never_mark_retry_safe() {
        let endpoint = IpcEndpoint::Address("ipc:///tmp/kicad/api.sock".to_string());

        let cases = [
            (
                UnreachableReason::NotConfigured,
                "not_configured",
                "kicad-ipc-socket-path-not-configured",
            ),
            (
                UnreachableReason::NoListener,
                "no_listener",
                "kicad-is-running-but-ipc_failure-says-it-did-not-answer",
            ),
            (
                UnreachableReason::AccessDenied,
                "access_denied",
                "access_denied-from-a-sandboxed-ai-client",
            ),
            (
                UnreachableReason::HandshakeFailed,
                "handshake_failed",
                "kicad-is-running-but-ipc_failure-says-it-did-not-answer",
            ),
            (
                UnreachableReason::TransportError,
                "unknown",
                "kicad-is-running-but-ipc_failure-says-it-did-not-answer",
            ),
        ];

        for (reason, expected_cause, expected_anchor) in cases {
            let outcome = PingOutcome::Unreachable {
                reason,
                message: "raw detail".to_string(),
            };
            let guidance = for_ping_outcome(Some(&endpoint), &outcome).expect("failure guidance");
            assert_eq!(guidance.cause, expected_cause, "{reason:?}");
            assert_eq!(guidance.mutation_outcome, "none", "{reason:?}");
            assert!(!guidance.retry_safe, "{reason:?}");
            assert!(!guidance.retry_prerequisites.is_empty(), "{reason:?}");
            assert!(
                guidance
                    .troubleshooting_url
                    .as_deref()
                    .is_some_and(|url| url.ends_with(expected_anchor)),
                "{reason:?}: {:?}",
                guidance.troubleshooting_url
            );
            assert!(
                guidance.next_steps.iter().any(|s| s.actor == "user")
                    && guidance.next_steps.iter().any(|s| s.actor == "agent"),
                "{reason:?}: expected both user and agent steps"
            );
        }
    }

    #[test]
    fn no_listener_does_not_assert_api_is_disabled() {
        let guidance = for_unreachable(
            UnreachableReason::NoListener,
            Some("ipc:///tmp/kicad/api.sock"),
            &[],
            "connection refused",
        );
        assert_eq!(guidance.cause, "no_listener");
        assert!(!guidance.observed.to_lowercase().contains("disabled"));
        assert!(guidance
            .next_steps
            .iter()
            .any(|s| s.action.contains("Listening on")));
    }

    #[test]
    fn access_denied_suggests_same_account_and_loopback_http_without_claiming_sandbox() {
        let guidance = for_unreachable(
            UnreachableReason::AccessDenied,
            Some("ipc:///tmp/kicad/api.sock"),
            &[],
            "permission denied",
        );
        assert_eq!(guidance.cause, "access_denied");
        assert!(guidance.observed.contains("operating system refused"));
        assert!(!guidance.observed.contains("sandbox"));
        assert!(guidance
            .next_steps
            .iter()
            .any(|s| s.action.contains("same OS account") && s.action.contains("127.0.0.1")));
    }

    #[test]
    fn ambiguous_endpoint_exposes_redacted_candidates_and_does_not_auto_select() {
        let endpoint = IpcEndpoint::Ambiguous(vec![
            "ipc:///tmp/kicad/api.sock?token=secret1".to_string(),
            "ipc:///tmp/kicad/api-42.sock".to_string(),
        ]);
        let outcome = PingOutcome::Unreachable {
            reason: UnreachableReason::NotConfigured,
            message: "ambiguous".to_string(),
        };
        let evidence = ipc_failure_evidence_for_endpoint(&endpoint, &outcome);
        assert_eq!(evidence["kind"], "not_configured");
        assert_eq!(evidence["recovery"]["cause"], "ambiguous_endpoint");
        assert_eq!(evidence["recovery"]["retry_safe"], false);
        let candidates = evidence["recovery"]["evidence"]["candidates"]
            .as_array()
            .expect("candidates array");
        assert_eq!(candidates.len(), 2);
        assert!(!candidates[0].as_str().unwrap().contains("secret1"));
    }

    #[test]
    fn request_failed_distinguishes_as_not_ready_from_unknown_timeout() {
        let not_ready = for_ping_outcome(
            None,
            &PingOutcome::RequestFailed {
                message: "AS_NOT_READY".to_string(),
                status: Some(ApiStatusCode::AsNotReady),
            },
        )
        .unwrap();
        assert_eq!(not_ready.cause, "kicad_not_ready");
        assert_eq!(not_ready.mutation_outcome, "none");
        assert!(!not_ready.retry_safe);
        assert!(not_ready.observed.contains("modal dialog"));

        let timeout = for_ping_outcome(
            None,
            &PingOutcome::RequestFailed {
                message: "receive timed out".to_string(),
                status: None,
            },
        )
        .unwrap();
        assert_eq!(timeout.cause, "unknown");
        assert_eq!(timeout.mutation_outcome, "none");
        assert!(!timeout.retry_safe);
        assert!(timeout.observed.contains("does not prove"));
    }

    #[test]
    fn board_target_errors_expose_identities_and_never_switch_targets() {
        let wrong = for_board_target_error(&BoardTargetError::WrongDocument {
            requested: "a.kicad_pcb".to_string(),
            open_documents: vec!["b.kicad_pcb".to_string()],
        });
        assert_eq!(wrong.cause, "wrong_document");
        assert_eq!(wrong.evidence["requested"], "a.kicad_pcb");
        assert_eq!(wrong.evidence["open_documents"], json!(["b.kicad_pcb"]));
        assert!(!wrong.retry_safe);

        let ambiguous = for_board_target_error(&BoardTargetError::AmbiguousDocument {
            requested: "a.kicad_pcb".to_string(),
            candidates: vec![
                "dir1/a.kicad_pcb".to_string(),
                "dir2/a.kicad_pcb".to_string(),
            ],
        });
        assert_eq!(ambiguous.cause, "ambiguous_document");
        assert_eq!(
            ambiguous.evidence["candidates"],
            json!(["dir1/a.kicad_pcb", "dir2/a.kicad_pcb"])
        );
        assert!(!ambiguous.retry_safe);
    }

    #[test]
    fn ipc_failure_result_preserves_unreachable_cause_and_uncertain_semantics() {
        let access = parse_result(&ipc_failure_result(IpcFailure::Unreachable {
            reason: UnreachableReason::AccessDenied,
            message: "permission denied".to_string(),
            candidates: Vec::new(),
        }));
        assert_eq!(access["error"]["kind"], "editor_unavailable");
        assert_eq!(access["recovery"]["cause"], "access_denied");
        assert_eq!(access["recovery"]["retry_safe"], false);
        assert!(!access["message"]
            .as_str()
            .unwrap()
            .contains("must be running with the board loaded"));

        let uncertain = parse_result(&ipc_failure_result(IpcFailure::Uncertain(
            "receive timed out".to_string(),
        )));
        assert_eq!(uncertain["error"]["kind"], "ipc_outcome_unknown");
        assert_eq!(uncertain["error"]["retry_safe"], false);
        assert_eq!(uncertain["recovery"]["cause"], "ipc_outcome_unknown");
        assert_eq!(uncertain["recovery"]["mutation_outcome"], "unknown");
        assert_eq!(uncertain["recovery"]["retry_safe"], false);

        let recovered = parse_result(&ipc_failure_result(IpcFailure::Recovered(
            "late reply".to_string(),
        )));
        assert_eq!(recovered["error"]["kind"], "ipc_batch_recovered");
        assert_eq!(recovered["error"]["retry_safe"], false);
        assert_eq!(recovered["recovery"]["cause"], "ipc_batch_recovered");
        assert_eq!(recovered["recovery"]["mutation_outcome"], "unchanged");
        assert_eq!(recovered["recovery"]["retry_safe"], false);
    }

    #[test]
    fn unclassified_error_does_not_infer_a_kicad_rejection_from_its_text() {
        let error = anyhow::anyhow!("KiCad IPC error: mock refusal (AS_BAD_REQUEST)");
        let body = parse_result(&ipc_failure_result(IpcFailure::from_error(error)));
        let recovery = &body["recovery"];
        assert_eq!(recovery["cause"], "unknown");
        assert_eq!(recovery["mutation_outcome"], "unknown");
        assert_eq!(recovery["retry_safe"], false);
        assert_eq!(
            recovery["observed"],
            "The operation failed, but the source of the failure and what was applied are not known."
        );
        assert!(recovery.get("troubleshooting_url").is_none());
    }

    #[test]
    fn delete_item_rejection_does_not_imply_that_nothing_applied() {
        use copperpour_ipc::client::{DeleteItemRejection, ItemRejection};
        use copperpour_ipc::gen::kiapi::common::commands::ItemDeletionStatus;

        let error = ItemRejection::Delete {
            reported_deleted_kiids: vec!["accepted-track".into()],
            rejected_items: vec![DeleteItemRejection {
                kiid: "protected-track".into(),
                status: ItemDeletionStatus::IdsImmutable,
            }],
            unclassified_kiids: vec![],
            unreported_kiids: vec!["unreported-track".into()],
            diagnostic: "protected track refused".into(),
        };
        let body = parse_result(&ipc_failure_result(IpcFailure::from_error(error.into())));
        let recovery = &body["recovery"];
        assert_eq!(recovery["cause"], "kicad_api_rejection");
        assert_eq!(recovery["mutation_outcome"], "unknown");
        assert_eq!(recovery["retry_safe"], false);
        assert!(recovery["troubleshooting_url"]
            .as_str()
            .unwrap()
            .ends_with("#kicad-rejected-items-during-an-ipc-write"));
        assert_eq!(recovery["evidence"]["command"], "DeleteItems");
        assert_eq!(
            recovery["evidence"]["rejected_items"],
            json!([
                {"kiid": "protected-track", "status": "IDS_IMMUTABLE"}
            ])
        );
        assert_eq!(
            recovery["evidence"]["reported_deleted_kiids"],
            json!(["accepted-track"])
        );
        assert_eq!(recovery["evidence"]["rollback"], "not_established");
        assert_eq!(
            recovery["evidence"]["unreported_kiids"],
            json!(["unreported-track"])
        );
    }

    #[test]
    fn board_editor_absence_maps_no_pcb_editor_vs_unimplemented() {
        let no_editor = parse_result(&ipc_unserved_result(
            BoardEditorAbsence::NoPcbEditor,
            "AS_UNHANDLED",
        ));
        assert_eq!(no_editor["error"]["kind"], "editor_unavailable");
        assert_eq!(no_editor["recovery"]["cause"], "no_pcb_editor_at_endpoint");
        assert_eq!(no_editor["recovery"]["retry_safe"], false);

        let unimpl = parse_result(&ipc_unserved_result(
            BoardEditorAbsence::OpenDocumentsUnimplemented,
            "AS_UNIMPLEMENTED",
        ));
        assert_eq!(unimpl["recovery"]["cause"], "unknown");
        assert_eq!(unimpl["recovery"]["retry_safe"], false);
    }
}
