//! `set_erc_severity`: set the severity of named ERC checks in a project's
//! `.kicad_pro`.
//!
//! KiCad 10.0.6 keeps them under `erc.rule_severities`, one key per check,
//! and reads each by its key (`eeschema/erc/erc_settings.cpp:126-160`). A key
//! it does not know is passed over, and any string but `warning` or `ignore`
//! reads as `error` (`common/widgets/ui_common.cpp:56-64`), so a mistyped
//! check or severity would be written and never take effect; both are
//! refused. A check is known when KiCad 10.0.6 has it or the file already
//! names it, as a later KiCad writes the checks it adds. The checks are not a
//! schema enum, whose refusal names only two.

use crate::mcp::{error::ToolErrorKind, protocol::CallToolResult};
use crate::tool;
use crate::tools::{get_path, invalid_arg, ToolContext, ToolDef};
use serde_json::json;

/// The checks whose severity KiCad 10.0.6 reads and writes: the non-empty
/// settings keys of `ERC_ITEM::allItemTypes` before `heading_internal`
/// (`eeschema/erc/erc_item.cpp:246-305`, `eeschema/erc/erc_item.h:76-92`).
const ERC_CHECKS: &[&str] = &[
    "bus_definition_conflict",
    "bus_entry_needed",
    "bus_to_bus_conflict",
    "bus_to_net_conflict",
    "different_unit_footprint",
    "different_unit_net",
    "duplicate_reference",
    "duplicate_sheet_names",
    "endpoint_off_grid",
    "extra_units",
    "field_name_whitespace",
    "footprint_filter",
    "footprint_link_issues",
    "four_way_junction",
    "ground_pin_not_ground",
    "hier_label_mismatch",
    "isolated_pin_label",
    "label_dangling",
    "label_multiple_wires",
    "lib_symbol_issues",
    "lib_symbol_mismatch",
    "missing_bidi_pin",
    "missing_input_pin",
    "missing_power_pin",
    "missing_unit",
    "multiple_net_names",
    "net_not_bus_member",
    "no_connect_connected",
    "no_connect_dangling",
    "pin_not_connected",
    "pin_not_driven",
    "pin_to_pin",
    "power_pin_not_driven",
    "same_local_global_label",
    "similar_label_and_power",
    "similar_labels",
    "similar_power",
    "simulation_model_issue",
    "single_global_label",
    "stacked_pin_name",
    "unannotated",
    "unconnected_wire_endpoint",
    "undefined_netclass",
    "unit_value_mismatch",
    "unresolved_variable",
    "wire_dangling",
];

/// What `SeverityToString` writes (`common/widgets/ui_common.cpp:67-75`). Of
/// `pin_to_pin` KiCad reads only whether it is `ignore`: the pin map sets
/// each pin conflict's severity (`eeschema/erc/erc_settings.cpp:277-296`).
const SEVERITIES: &[&str] = &["error", "warning", "ignore"];

pub(super) fn tool() -> ToolDef {
    tool!(
        "set_erc_severity",
        "Set the severity of ERC checks in the project file: `severities` maps a check, by \
         the key KiCad gives it in the .kicad_pro (e.g. single_global_label), to error, \
         warning or ignore. A check is one KiCad 10.0.6 has or one the file already names. \
         For pin_to_pin, error and warning both turn the check on, and the pin map sets \
         each conflict's severity. Changes only the checks named, under \
         erc.rule_severities, and run_erc reports by them from its next run. The reply \
         gives each check's previous and current severity, previous null where the file \
         did not name it. Refuses while KiCad holds the project (its \
         ~<project>.kicad_pro.lck), because Eeschema and pcbnew rewrite the project file \
         from their own copy when they save.",
        json!({
            "type": "object",
            "properties": {
                "project": { "type": "string", "description": "Path to the .kicad_pro file" },
                "severities": {
                    "type": "object",
                    "minProperties": 1,
                    "description": "ERC check to severity, e.g. {\"single_global_label\": \"error\"}",
                    "additionalProperties": { "type": "string", "enum": SEVERITIES }
                }
            },
            "required": ["project", "severities"]
        }),
        |args, ctx| async move { handle_set_erc_severity(args, ctx).await }
    )
}

async fn handle_set_erc_severity(
    args: &serde_json::Value,
    _ctx: &ToolContext,
) -> anyhow::Result<CallToolResult> {
    let project = get_path(args, "project")?;
    // KiCad's project lock is found only beside a .kicad_pro, so any other
    // path would pass the guard below unexamined.
    if !project
        .extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| extension.eq_ignore_ascii_case("kicad_pro"))
    {
        let reason = format!("{} is not a .kicad_pro file", project.display());
        return Ok(CallToolResult::error_kind(
            ToolErrorKind::HandlerError {
                reason: reason.clone(),
            },
            format!("{reason}. ERC severities live in the project file beside the root schematic."),
        ));
    }
    let requested = match read_severities(args) {
        Ok(requested) => requested,
        Err(refusal) => return Ok(refusal),
    };
    // Before the read as well as the write: while KiCad holds the project the
    // file can be older than what KiCad holds, so even a reply that changes
    // nothing would report severities from a stale copy.
    if let Some(refusal) =
        crate::tools::pcb_board::refuse_if_project_locked_by_kicad(&project, "severity change")
    {
        return Ok(refusal);
    }
    let read = match std::fs::read_to_string(&project) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(CallToolResult::error_kind(
                ToolErrorKind::FileNotFound {
                    path: project.display().to_string(),
                },
                format!(
                    "No project file at {}. ERC severities live in the .kicad_pro beside the \
                     root schematic.",
                    project.display()
                ),
            ))
        }
        Err(error) => return Err(error.into()),
    };
    let mut settings: serde_json::Value = serde_json::from_str(&read)
        .map_err(|e| anyhow::anyhow!("{} is not valid JSON: {e}", project.display()))?;
    // KiCad reads the section without its `meta`: the missing version is
    // caught and the keys loaded anyway (`common/settings/nested_settings.cpp:
    // 77-86,123`, `json_settings.cpp:354-355`). So a project that has none gets
    // only the checks named here.
    let stored = settings
        .as_object_mut()
        .ok_or_else(|| anyhow::anyhow!("{}: top level is not a JSON object", project.display()))?
        .entry("erc")
        .or_insert_with(|| json!({}))
        .as_object_mut()
        .ok_or_else(|| anyhow::anyhow!("{}: erc is not an object", project.display()))?
        .entry("rule_severities")
        .or_insert_with(|| json!({}))
        .as_object_mut()
        .ok_or_else(|| {
            anyhow::anyhow!(
                "{}: erc.rule_severities is not an object",
                project.display()
            )
        })?;

    let unknown: Vec<&str> = requested
        .iter()
        .map(|(check, _)| check.as_str())
        .filter(|check| !ERC_CHECKS.contains(check) && !stored.contains_key(*check))
        .collect();
    if !unknown.is_empty() {
        let mut known: Vec<&str> = ERC_CHECKS.to_vec();
        known.extend(stored.keys().map(String::as_str));
        known.sort_unstable();
        known.dedup();
        return Ok(invalid_arg(
            "severities",
            &format!(
                "neither KiCad 10.0.6 nor this project file has an ERC check {}. The checks \
                 are: {}",
                unknown.join(", "),
                known.join(", ")
            ),
        ));
    }

    let mut severities = serde_json::Map::new();
    let mut changed = false;
    for (check, severity) in requested {
        let current = json!(severity);
        let previous = stored.insert(check.clone(), current.clone());
        changed |= previous.as_ref() != Some(&current);
        severities.insert(check, json!({ "previous": previous, "current": current }));
    }
    // Saving the severities the file already holds would rewrite the whole
    // project file for a call that decided nothing.
    if changed {
        if let Some(refusal) = crate::tools::pcb_board::write_project_if_unchanged(
            &project,
            "set_erc_severity",
            &read,
            &settings,
        )? {
            return Ok(refusal);
        }
    }

    Ok(CallToolResult::json(&json!({
        "file": project.display().to_string(),
        "changed": changed,
        "severities": severities
    })))
}

/// The requested severities, each one KiCad reads; the schema says the same,
/// and a direct call is held to it here. The checks are judged against the
/// file, once it is read.
fn read_severities(
    args: &serde_json::Value,
) -> Result<Vec<(String, &'static str)>, CallToolResult> {
    let Some(requested) = args["severities"].as_object() else {
        return Err(invalid_arg(
            "severities",
            "must be an object of ERC check to severity",
        ));
    };
    if requested.is_empty() {
        return Err(invalid_arg(
            "severities",
            "must name at least one ERC check",
        ));
    }
    let mut read = Vec::new();
    for (check, severity) in requested {
        let field = format!("severities.{check}");
        let Some(severity) = severity
            .as_str()
            .and_then(|severity| SEVERITIES.iter().find(|known| **known == severity))
        else {
            return Err(invalid_arg(
                &field,
                "must be error, warning or ignore; KiCad reads any other string as error",
            ));
        };
        read.push((check.clone(), *severity));
    }
    Ok(read)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mcp::handler::McpHandler;
    use crate::tools::{write_hook, ServerConfig};
    use std::path::{Path, PathBuf};

    /// eeschema 10.0.6's own save of a project, its `erc` section whole.
    const KICAD_PROJECT: &str =
        include_str!("../../tests/fixtures/annotate_nested_hierarchy/nested_pages.kicad_pro");

    /// KiCad 10.0.6's save of a sheet whose global label `LONE` appears once
    /// and `PAIR` twice.
    const SHEET: &str = include_str!("../../tests/fixtures/erc_severity_kicad10.kicad_sch");

    async fn call_with(
        kicad_cli: &str,
        tool: &str,
        arguments: serde_json::Value,
    ) -> serde_json::Value {
        let handler = McpHandler::new(ServerConfig {
            eager_toolsets: true,
            kicad_cli: kicad_cli.to_string(),
            ..Default::default()
        })
        .await
        .unwrap();
        let response = handler
            .handle_message(json!({
                "jsonrpc": "2.0", "id": 1, "method": "tools/call",
                "params": {"name": tool, "arguments": arguments}
            }))
            .await
            .unwrap()
            .result
            .unwrap();
        let text = response["content"][0]["text"].as_str().unwrap();
        let mut body: serde_json::Value =
            serde_json::from_str(text).unwrap_or_else(|_| json!({ "text": text }));
        body["is_error"] = response["isError"].clone();
        body
    }

    async fn call(arguments: serde_json::Value) -> serde_json::Value {
        call_with("kicad-cli", "set_erc_severity", arguments).await
    }

    /// The fixture sheet as `lone.kicad_sch`, beside eeschema's own project
    /// file, or, with `kicad` false, beside the one `create_project` writes,
    /// which has no `erc` section.
    async fn project(kicad: bool) -> (tempfile::TempDir, PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let pro = dir.path().join("lone.kicad_pro");
        if kicad {
            std::fs::write(&pro, KICAD_PROJECT).unwrap();
        } else {
            let body = call_with(
                "kicad-cli",
                "create_project",
                json!({ "path": dir.path(), "name": "lone" }),
            )
            .await;
            assert_ne!(body["is_error"], true, "{body}");
            assert!(read_json(&pro).get("erc").is_none());
        }
        std::fs::write(dir.path().join("lone.kicad_sch"), SHEET).unwrap();
        (dir, pro)
    }

    fn read_json(path: &Path) -> serde_json::Value {
        serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap()
    }

    #[test]
    fn the_checks_are_the_keys_eeschema_writes() {
        let project: serde_json::Value = serde_json::from_str(KICAD_PROJECT).unwrap();
        let written: Vec<&str> = project["erc"]["rule_severities"]
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect();
        assert_eq!(written, ERC_CHECKS);
    }

    #[tokio::test]
    async fn only_the_named_check_changes_in_kicads_own_file() {
        let (_dir, pro) = project(true).await;
        let body =
            call(json!({ "project": pro, "severities": { "single_global_label": "error" } })).await;
        assert_ne!(body["is_error"], true, "{body}");
        assert_eq!(body["changed"], true, "{body}");
        assert_eq!(
            body["severities"],
            json!({ "single_global_label": { "previous": "ignore", "current": "error" } })
        );
        let written = std::fs::read_to_string(&pro).unwrap();
        let changed: Vec<(&str, &str)> = KICAD_PROJECT
            .lines()
            .zip(written.lines())
            .filter(|(before, after)| before != after)
            .collect();
        assert_eq!(KICAD_PROJECT.lines().count(), written.lines().count());
        assert_eq!(
            changed,
            [(
                "      \"single_global_label\": \"ignore\",",
                "      \"single_global_label\": \"error\","
            )]
        );
    }

    #[tokio::test]
    async fn a_project_without_an_erc_section_gets_only_the_named_checks() {
        let (_dir, pro) = project(false).await;
        let before = read_json(&pro);
        let body = call(json!({
            "project": pro,
            "severities": { "single_global_label": "error", "label_dangling": "ignore" }
        }))
        .await;
        assert_ne!(body["is_error"], true, "{body}");
        assert_eq!(
            body["severities"],
            json!({
                "label_dangling": { "previous": null, "current": "ignore" },
                "single_global_label": { "previous": null, "current": "error" }
            })
        );
        let mut after = read_json(&pro);
        assert_eq!(
            after["erc"],
            json!({ "rule_severities": { "label_dangling": "ignore", "single_global_label": "error" } })
        );
        after.as_object_mut().unwrap().remove("erc");
        assert_eq!(after, before);
    }

    #[tokio::test]
    async fn naming_the_severity_the_file_holds_writes_nothing() {
        let (dir, pro) = project(true).await;
        // A write would replace the file with this; none must come.
        write_hook::set("{}\n".to_string());
        let body =
            call(json!({ "project": pro, "severities": { "single_global_label": "ignore" } }))
                .await;
        let armed = write_hook::armed();
        if armed {
            write_hook::run(&dir.path().join("unwritten"));
        }
        assert_ne!(body["is_error"], true, "{body}");
        assert_eq!(body["changed"], false, "{body}");
        assert!(armed, "the project file was written");
        assert_eq!(std::fs::read_to_string(&pro).unwrap(), KICAD_PROJECT);
    }

    #[tokio::test]
    async fn a_project_saved_since_the_read_is_left_as_saved() {
        let (_dir, pro) = project(true).await;
        let saved = KICAD_PROJECT.replace(
            "\"pin_not_connected\": \"error\"",
            "\"pin_not_connected\": \"warning\"",
        );
        write_hook::set(saved.clone());
        let body =
            call(json!({ "project": pro, "severities": { "single_global_label": "error" } })).await;
        assert_eq!(body["is_error"], true, "{body}");
        assert_eq!(body["error"]["kind"], "conflict", "{body}");
        assert_eq!(std::fs::read_to_string(&pro).unwrap(), saved);
    }

    #[tokio::test]
    async fn a_check_kicad_does_not_have_is_refused_naming_those_it_has() {
        let (_dir, pro) = project(true).await;
        for check in [
            "single_global_labels",
            "Single_Global_Label",
            "generic-error",
            "duplicate_pins",
        ] {
            let body = call(json!({ "project": pro, "severities": { check: "error" } })).await;
            assert_eq!(body["error"]["kind"], "invalid_argument", "{check}: {body}");
            let message = body["message"].as_str().unwrap();
            for known in ERC_CHECKS {
                assert!(message.contains(&format!(" {known}")), "{known}: {body}");
            }
        }
        assert_eq!(std::fs::read_to_string(&pro).unwrap(), KICAD_PROJECT);
    }

    /// A later KiCad writes the checks it adds, so a check the file already
    /// names is known even where KiCad 10.0.6 has no such check.
    #[tokio::test]
    async fn a_check_the_file_already_names_is_known() {
        let (_dir, pro) = project(true).await;
        let body =
            call(json!({ "project": pro, "severities": { "same_local_global_power": "error" } }))
                .await;
        assert_eq!(body["error"]["kind"], "invalid_argument", "{body}");
        let later = KICAD_PROJECT.replace(
            "\"same_local_global_label\": \"warning\",",
            "\"same_local_global_label\": \"warning\",\n      \"same_local_global_power\": \"warning\",",
        );
        assert_ne!(later, KICAD_PROJECT);
        std::fs::write(&pro, &later).unwrap();
        let body =
            call(json!({ "project": pro, "severities": { "same_local_global_power": "error" } }))
                .await;
        assert_ne!(body["is_error"], true, "{body}");
        assert_eq!(
            body["severities"]["same_local_global_power"],
            json!({ "previous": "warning", "current": "error" })
        );
    }

    #[tokio::test]
    async fn a_project_file_of_another_shape_is_refused_and_left_alone() {
        let (_dir, pro) = project(true).await;
        for text in [
            "not json",
            "[]",
            "{\"erc\": 1}",
            "{\"erc\": {\"rule_severities\": []}}",
        ] {
            std::fs::write(&pro, text).unwrap();
            let body =
                call(json!({ "project": pro, "severities": { "single_global_label": "error" } }))
                    .await;
            assert_eq!(body["is_error"], true, "{text}: {body}");
            assert_eq!(std::fs::read_to_string(&pro).unwrap(), text);
        }
    }

    /// The schema refuses these before the handler runs; a direct call is
    /// refused by the handler alone.
    #[tokio::test]
    async fn the_handler_refuses_what_the_schema_refuses() {
        let (_dir, pro) = project(true).await;
        let ctx = ToolContext::new(
            ServerConfig::default(),
            std::sync::Arc::new(crate::router::ToolRouter::new()),
        );
        for severities in [
            json!("error"),
            json!({}),
            json!({ "single_global_label": "Error" }),
            json!({ "single_global_label": 2 }),
        ] {
            let result =
                handle_set_erc_severity(&json!({ "project": pro, "severities": severities }), &ctx)
                    .await
                    .unwrap();
            let crate::mcp::protocol::ToolContent::Text { text } = &result.content[0] else {
                panic!("expected text");
            };
            let body: serde_json::Value = serde_json::from_str(text).unwrap();
            assert_eq!(
                body["error"]["kind"], "invalid_argument",
                "{severities}: {body}"
            );
        }
        assert_eq!(std::fs::read_to_string(&pro).unwrap(), KICAD_PROJECT);
    }

    /// Through the served call these are the schema's refusals, in its own
    /// words; the handler's all begin "must".
    #[tokio::test]
    async fn a_severity_kicad_would_read_as_error_is_refused() {
        let (_dir, pro) = project(true).await;
        for severity in [
            json!("Error"),
            json!("info"),
            json!("exclusion"),
            json!(""),
            json!(2),
        ] {
            let body = call(json!({
                "project": pro,
                "severities": { "single_global_label": severity }
            }))
            .await;
            assert_eq!(
                body["error"]["kind"], "invalid_argument",
                "{severity}: {body}"
            );
            assert_eq!(
                body["error"]["field"], "severities.single_global_label",
                "{body}"
            );
            let reason = body["error"]["reason"].as_str().unwrap();
            assert!(
                !reason.starts_with("must"),
                "the handler refused it: {body}"
            );
        }
        assert_eq!(std::fs::read_to_string(&pro).unwrap(), KICAD_PROJECT);
    }

    #[tokio::test]
    async fn no_check_named_is_refused() {
        let (_dir, pro) = project(false).await;
        let before = std::fs::read_to_string(&pro).unwrap();
        let body = call(json!({ "project": pro, "severities": {} })).await;
        assert_eq!(body["error"]["kind"], "invalid_argument", "{body}");
        let reason = body["error"]["reason"].as_str().unwrap();
        assert!(
            !reason.starts_with("must"),
            "the handler refused it: {body}"
        );
        assert_eq!(std::fs::read_to_string(&pro).unwrap(), before);
    }

    #[tokio::test]
    async fn a_path_that_is_not_a_project_file_is_refused() {
        let (dir, _pro) = project(true).await;
        let sheet = dir.path().join("lone.kicad_sch");
        let body =
            call(json!({ "project": sheet, "severities": { "single_global_label": "error" } }))
                .await;
        // A path present but unusable is not an argument error (DEV.md).
        assert_eq!(body["error"]["kind"], "handler_error", "{body}");
        assert_eq!(std::fs::read_to_string(&sheet).unwrap(), SHEET);
        // A project under another name: only the extension check stops the
        // write, since no project lock is looked for beside it.
        let copy = dir.path().join("lone.json");
        std::fs::write(&copy, KICAD_PROJECT).unwrap();
        let body =
            call(json!({ "project": copy, "severities": { "single_global_label": "error" } }))
                .await;
        assert_eq!(body["error"]["kind"], "handler_error", "{body}");
        assert_eq!(std::fs::read_to_string(&copy).unwrap(), KICAD_PROJECT);
    }

    #[tokio::test]
    async fn a_project_kicad_holds_is_refused_even_where_nothing_would_change() {
        let (dir, pro) = project(true).await;
        std::fs::write(dir.path().join("~lone.kicad_pro.lck"), "").unwrap();
        for severity in ["error", "ignore"] {
            let body = call(json!({
                "project": pro,
                "severities": { "single_global_label": severity }
            }))
            .await;
            assert_eq!(
                body["error"]["kind"], "unsafe_file_fallback",
                "{severity}: {body}"
            );
        }
        assert_eq!(std::fs::read_to_string(&pro).unwrap(), KICAD_PROJECT);
    }

    #[tokio::test]
    async fn a_missing_project_file_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let pro = dir.path().join("lone.kicad_pro");
        let body =
            call(json!({ "project": pro, "severities": { "single_global_label": "error" } })).await;
        assert_eq!(body["error"]["kind"], "file_not_found", "{body}");
        assert!(!pro.exists());
    }

    /// KiCad's ERC decides: kicad-cli reports `single_global_label` on `LONE`
    /// alone at the severity set, and not at all once it is `ignore`, from
    /// eeschema's own project file and from one with no `erc` section.
    ///
    ///     KICAD_CLI=/path/to/kicad-cli cargo test -p copperpour-core --lib \
    ///         erc_severity::tests::live -- --ignored
    #[tokio::test]
    #[ignore = "needs a real kicad-cli"]
    async fn live_run_erc_reports_single_global_label_as_set() {
        let cli = std::env::var("KICAD_CLI").unwrap_or_else(|_| "kicad-cli".to_string());
        for kicad in [true, false] {
            let (dir, pro) = project(kicad).await;
            let sheet = dir.path().join("lone.kicad_sch");
            let lone = |severity: &str| {
                let (cli, sheet, severity) = (cli.clone(), sheet.clone(), severity.to_string());
                async move {
                    let body = call_with(
                        &cli,
                        "run_erc",
                        json!({ "schematic": sheet, "severity": "info" }),
                    )
                    .await;
                    assert_ne!(body["is_error"], true, "{body}");
                    let found: Vec<_> = body["violations"]
                        .as_array()
                        .unwrap()
                        .iter()
                        .filter(|violation| violation["rule"] == "single_global_label")
                        .collect();
                    match severity.as_str() {
                        "ignore" => assert!(found.is_empty(), "{body}"),
                        _ => {
                            assert_eq!(found.len(), 1, "{body}");
                            assert_eq!(found[0]["severity"], severity, "{body}");
                            assert!(found[0].to_string().contains("LONE"), "{body}");
                        }
                    }
                }
            };
            // KiCad's default, and eeschema's file, leave the check off.
            lone("ignore").await;
            for severity in ["error", "warning", "ignore"] {
                let body = call(json!({
                    "project": pro,
                    "severities": { "single_global_label": severity }
                }))
                .await;
                assert_ne!(body["is_error"], true, "{body}");
                lone(severity).await;
            }
        }
    }
}
