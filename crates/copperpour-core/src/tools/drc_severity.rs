//! `set_drc_severity`: set the severity of named DRC checks in a project's
//! `.kicad_pro`, the sibling of `set_erc_severity`.
//!
//! KiCad 10.0.6 keeps them under `board.design_settings.rule_severities`, one
//! key per check, and reads each by its key
//! (`pcbnew/board_design_settings.cpp:337-376`). A key it does not know is
//! passed over, and any string but `warning` or `ignore` reads as `error`
//! (`common/widgets/ui_common.cpp:56-64`), so a mistyped check or severity
//! would be written and never take effect; both are refused. A check is known
//! when KiCad 10.0.6 has it or the file already names it, as a later KiCad
//! writes the checks it adds. The checks are not a schema enum, whose refusal
//! names only two.

use crate::mcp::{error::ToolErrorKind, protocol::CallToolResult};
use crate::tool;
use crate::tools::{get_path, invalid_arg, ToolContext, ToolDef};
use serde_json::json;

/// The checks whose severity KiCad 10.0.6 reads and writes: the settings keys
/// of `DRC_ITEM::allItemTypes` before `heading_internal`
/// (`pcbnew/drc/drc_item.cpp:320-401`, `pcbnew/drc/drc_item.h:143-159`).
const DRC_CHECKS: &[&str] = &[
    "annular_width",
    "clearance",
    "connection_width",
    "copper_edge_clearance",
    "copper_sliver",
    "courtyards_overlap",
    "creepage",
    "diff_pair_gap_out_of_range",
    "diff_pair_uncoupled_length_too_long",
    "drill_out_of_range",
    "duplicate_footprints",
    "extra_footprint",
    "footprint",
    "footprint_filters_mismatch",
    "footprint_symbol_field_mismatch",
    "footprint_symbol_mismatch",
    "footprint_type_mismatch",
    "hole_clearance",
    "hole_to_hole",
    "holes_co_located",
    "invalid_outline",
    "isolated_copper",
    "item_on_disabled_layer",
    "items_not_allowed",
    "length_out_of_range",
    "lib_footprint_issues",
    "lib_footprint_mismatch",
    "malformed_courtyard",
    "microvia_drill_out_of_range",
    "mirrored_text_on_front_layer",
    "missing_courtyard",
    "missing_footprint",
    "missing_tuning_profile",
    "net_conflict",
    "nonmirrored_text_on_back_layer",
    "npth_inside_courtyard",
    "padstack",
    "pth_inside_courtyard",
    "shorting_items",
    "silk_edge_clearance",
    "silk_over_copper",
    "silk_overlap",
    "skew_out_of_range",
    "solder_mask_bridge",
    "starved_thermal",
    "text_height",
    "text_on_edge_cuts",
    "text_thickness",
    "through_hole_pad_without_hole",
    "too_many_vias",
    "track_angle",
    "track_dangling",
    "track_not_centered_on_via",
    "track_on_post_machined_layer",
    "track_segment_length",
    "track_width",
    "tracks_crossing",
    "tuning_profile_track_geometries",
    "unconnected_items",
    "unresolved_variable",
    "via_dangling",
    "zones_intersect",
];

/// What `SeverityToString` writes (`common/widgets/ui_common.cpp:67-75`).
const SEVERITIES: &[&str] = &["error", "warning", "ignore"];

pub(super) fn tool() -> ToolDef {
    tool!(
        "set_drc_severity",
        "Set the severity of DRC checks in the project file: `severities` maps a check, by \
         the key KiCad gives it in the .kicad_pro (e.g. silk_edge_clearance), to error, \
         warning or ignore. A check is one KiCad 10.0.6 has or one the file already names. \
         Changes only the checks named, under board.design_settings.rule_severities, and \
         run_drc reports by them from its next run; a check set to ignore is not run at all. \
         The reply gives each check's previous and current severity, previous null where \
         the file did not name it. Refuses while KiCad holds the project (its \
         ~<project>.kicad_pro.lck), because pcbnew and Eeschema rewrite the project file \
         from their own copy when they save.",
        json!({
            "type": "object",
            "properties": {
                "project": { "type": "string", "description": "Path to the .kicad_pro file" },
                "severities": {
                    "type": "object",
                    "minProperties": 1,
                    "description": "DRC check to severity, e.g. {\"silk_edge_clearance\": \"error\"}",
                    "additionalProperties": { "type": "string", "enum": SEVERITIES }
                }
            },
            "required": ["project", "severities"]
        }),
        |args, ctx| async move { handle_set_drc_severity(args, ctx).await }
    )
}

async fn handle_set_drc_severity(
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
            format!("{reason}. DRC severities live in the project file beside the board."),
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
                    "No project file at {}. DRC severities live in the .kicad_pro beside the \
                     board.",
                    project.display()
                ),
            ))
        }
        Err(error) => return Err(error.into()),
    };
    let mut settings: serde_json::Value = serde_json::from_str(&read)
        .map_err(|e| anyhow::anyhow!("{} is not valid JSON: {e}", project.display()))?;
    let stored = rule_severities(&mut settings, &project)?;

    let unknown: Vec<&str> = requested
        .iter()
        .map(|(check, _)| check.as_str())
        .filter(|check| !DRC_CHECKS.contains(check) && !stored.contains_key(*check))
        .collect();
    if !unknown.is_empty() {
        let mut known: Vec<&str> = DRC_CHECKS.to_vec();
        known.extend(stored.keys().map(String::as_str));
        known.sort_unstable();
        known.dedup();
        return Ok(invalid_arg(
            "severities",
            &format!(
                "neither KiCad 10.0.6 nor this project file has a DRC check {}. The checks \
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
            "set_drc_severity",
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

/// `board.design_settings.rule_severities`, made where the file lacks a level.
/// KiCad reads the section without its `meta`: the missing version is caught
/// and the keys loaded anyway (`common/settings/nested_settings.cpp:77-86,123`,
/// `json_settings.cpp:354-355`). So a project that has none gets only the
/// checks named here, and KiCad its defaults for the rest
/// (`pcbnew/board_design_settings.cpp:167-212`).
fn rule_severities<'a>(
    settings: &'a mut serde_json::Value,
    project: &std::path::Path,
) -> anyhow::Result<&'a mut serde_json::Map<String, serde_json::Value>> {
    let mut level = settings
        .as_object_mut()
        .ok_or_else(|| anyhow::anyhow!("{}: top level is not a JSON object", project.display()))?;
    let mut path = String::new();
    for key in ["board", "design_settings", "rule_severities"] {
        if !path.is_empty() {
            path.push('.');
        }
        path.push_str(key);
        level = level
            .entry(key)
            .or_insert_with(|| json!({}))
            .as_object_mut()
            .ok_or_else(|| anyhow::anyhow!("{}: {path} is not an object", project.display()))?;
    }
    Ok(level)
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
            "must be an object of DRC check to severity",
        ));
    };
    if requested.is_empty() {
        return Err(invalid_arg(
            "severities",
            "must name at least one DRC check",
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

    /// pcbnew 10.0.6's own save of a project, its `board.design_settings`
    /// whole (`netclass_diff_pair_kicad10.README.md`).
    const KICAD_PROJECT: &str =
        include_str!("../../tests/fixtures/netclass_diff_pair_kicad10.kicad_pro");

    /// A board whose J1 pads sit closer to the outline than the copper edge
    /// clearance, which KiCad reports as `copper_edge_clearance` at its
    /// default `error` (`drc_ownership_j1.README.md`).
    const BOARD: &str = include_str!("../../tests/fixtures/drc_ownership_j1.kicad_pcb");

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
        call_with("kicad-cli", "set_drc_severity", arguments).await
    }

    /// The fixture board as `j1.kicad_pcb`, beside pcbnew's own project file,
    /// or, with `kicad` false, beside the one `create_project` writes, whose
    /// `board.design_settings` is empty.
    async fn project(kicad: bool) -> (tempfile::TempDir, PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let pro = dir.path().join("j1.kicad_pro");
        if kicad {
            std::fs::write(&pro, KICAD_PROJECT).unwrap();
        } else {
            let body = call_with(
                "kicad-cli",
                "create_project",
                json!({ "path": dir.path(), "name": "j1" }),
            )
            .await;
            assert_ne!(body["is_error"], true, "{body}");
            assert_eq!(read_json(&pro)["board"]["design_settings"], json!({}));
        }
        std::fs::write(dir.path().join("j1.kicad_pcb"), BOARD).unwrap();
        (dir, pro)
    }

    fn read_json(path: &Path) -> serde_json::Value {
        serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap()
    }

    #[test]
    fn the_checks_are_the_keys_pcbnew_writes() {
        let project: serde_json::Value = serde_json::from_str(KICAD_PROJECT).unwrap();
        let written: Vec<&str> = project["board"]["design_settings"]["rule_severities"]
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect();
        assert_eq!(written, DRC_CHECKS);
    }

    #[tokio::test]
    async fn only_the_named_check_changes_in_kicads_own_file() {
        let (_dir, pro) = project(true).await;
        let body =
            call(json!({ "project": pro, "severities": { "silk_edge_clearance": "error" } })).await;
        assert_ne!(body["is_error"], true, "{body}");
        assert_eq!(body["changed"], true, "{body}");
        assert_eq!(
            body["severities"],
            json!({ "silk_edge_clearance": { "previous": "warning", "current": "error" } })
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
                "        \"silk_edge_clearance\": \"warning\",",
                "        \"silk_edge_clearance\": \"error\","
            )]
        );
    }

    #[tokio::test]
    async fn a_project_without_severities_gets_only_the_named_checks() {
        let (_dir, pro) = project(false).await;
        let before = read_json(&pro);
        let body = call(json!({
            "project": pro,
            "severities": { "silk_edge_clearance": "error", "silk_overlap": "ignore" }
        }))
        .await;
        assert_ne!(body["is_error"], true, "{body}");
        assert_eq!(
            body["severities"],
            json!({
                "silk_edge_clearance": { "previous": null, "current": "error" },
                "silk_overlap": { "previous": null, "current": "ignore" }
            })
        );
        let mut after = read_json(&pro);
        assert_eq!(
            after["board"]["design_settings"],
            json!({ "rule_severities": { "silk_edge_clearance": "error", "silk_overlap": "ignore" } })
        );
        after["board"]["design_settings"] = json!({});
        assert_eq!(after, before);
    }

    /// No `board` section at all: each level is made, and nothing else moves.
    #[tokio::test]
    async fn a_project_without_a_board_section_gets_one() {
        let (_dir, pro) = project(false).await;
        let mut before = read_json(&pro);
        before.as_object_mut().unwrap().remove("board");
        std::fs::write(&pro, serde_json::to_string_pretty(&before).unwrap()).unwrap();
        let body = call(json!({ "project": pro, "severities": { "silk_overlap": "error" } })).await;
        assert_ne!(body["is_error"], true, "{body}");
        let mut after = read_json(&pro);
        assert_eq!(
            after.as_object_mut().unwrap().remove("board"),
            Some(json!({ "design_settings": { "rule_severities": { "silk_overlap": "error" } } }))
        );
        assert_eq!(after, before);
    }

    #[tokio::test]
    async fn naming_the_severity_the_file_holds_writes_nothing() {
        let (dir, pro) = project(true).await;
        // A write would replace the file with this; none must come.
        write_hook::set("{}\n".to_string());
        let body =
            call(json!({ "project": pro, "severities": { "silk_edge_clearance": "warning" } }))
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
            "\"silk_overlap\": \"warning\"",
            "\"silk_overlap\": \"error\"",
        );
        assert_ne!(saved, KICAD_PROJECT);
        write_hook::set(saved.clone());
        let body =
            call(json!({ "project": pro, "severities": { "silk_edge_clearance": "error" } })).await;
        assert_eq!(body["is_error"], true, "{body}");
        assert_eq!(body["error"]["kind"], "conflict", "{body}");
        assert_eq!(std::fs::read_to_string(&pro).unwrap(), saved);
    }

    /// KiCad's internal checks have no severity a user can set,
    /// `hole_near_hole` is KiCad 8's name for `hole_to_hole`, and an ERC check
    /// is not a DRC one.
    #[tokio::test]
    async fn a_check_kicad_does_not_have_is_refused_naming_those_it_has() {
        let (_dir, pro) = project(true).await;
        for check in [
            "silk_edge_clearances",
            "Silk_Edge_Clearance",
            "generic_error",
            "padstack_invalid",
            "hole_near_hole",
            "single_global_label",
        ] {
            let body = call(json!({ "project": pro, "severities": { check: "error" } })).await;
            assert_eq!(body["error"]["kind"], "invalid_argument", "{check}: {body}");
            let message = body["message"].as_str().unwrap();
            for known in DRC_CHECKS {
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
            call(json!({ "project": pro, "severities": { "silk_over_pad": "error" } })).await;
        assert_eq!(body["error"]["kind"], "invalid_argument", "{body}");
        let later = KICAD_PROJECT.replace(
            "\"silk_overlap\": \"warning\",",
            "\"silk_overlap\": \"warning\",\n        \"silk_over_pad\": \"warning\",",
        );
        assert_ne!(later, KICAD_PROJECT);
        std::fs::write(&pro, &later).unwrap();
        let body =
            call(json!({ "project": pro, "severities": { "silk_over_pad": "error" } })).await;
        assert_ne!(body["is_error"], true, "{body}");
        assert_eq!(
            body["severities"]["silk_over_pad"],
            json!({ "previous": "warning", "current": "error" })
        );
    }

    #[tokio::test]
    async fn a_project_file_of_another_shape_is_refused_and_left_alone() {
        let (_dir, pro) = project(true).await;
        for text in [
            "not json",
            "[]",
            "{\"board\": 1}",
            "{\"board\": {\"design_settings\": []}}",
            "{\"board\": {\"design_settings\": {\"rule_severities\": []}}}",
        ] {
            std::fs::write(&pro, text).unwrap();
            let body =
                call(json!({ "project": pro, "severities": { "silk_edge_clearance": "error" } }))
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
            json!({ "silk_edge_clearance": "Error" }),
            json!({ "silk_edge_clearance": 2 }),
        ] {
            let result =
                handle_set_drc_severity(&json!({ "project": pro, "severities": severities }), &ctx)
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
                "severities": { "silk_edge_clearance": severity }
            }))
            .await;
            assert_eq!(
                body["error"]["kind"], "invalid_argument",
                "{severity}: {body}"
            );
            assert_eq!(
                body["error"]["field"], "severities.silk_edge_clearance",
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
        let board = dir.path().join("j1.kicad_pcb");
        let body =
            call(json!({ "project": board, "severities": { "silk_edge_clearance": "error" } }))
                .await;
        // A path present but unusable is not an argument error (DEV.md).
        assert_eq!(body["error"]["kind"], "handler_error", "{body}");
        assert_eq!(std::fs::read_to_string(&board).unwrap(), BOARD);
        // A project under another name: only the extension check stops the
        // write, since no project lock is looked for beside it.
        let copy = dir.path().join("j1.json");
        std::fs::write(&copy, KICAD_PROJECT).unwrap();
        let body =
            call(json!({ "project": copy, "severities": { "silk_edge_clearance": "error" } }))
                .await;
        assert_eq!(body["error"]["kind"], "handler_error", "{body}");
        assert_eq!(std::fs::read_to_string(&copy).unwrap(), KICAD_PROJECT);
    }

    #[tokio::test]
    async fn a_project_kicad_holds_is_refused_even_where_nothing_would_change() {
        let (dir, pro) = project(true).await;
        std::fs::write(dir.path().join("~j1.kicad_pro.lck"), "").unwrap();
        for severity in ["error", "warning"] {
            let body = call(json!({
                "project": pro,
                "severities": { "silk_edge_clearance": severity }
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
        let pro = dir.path().join("j1.kicad_pro");
        let body =
            call(json!({ "project": pro, "severities": { "silk_edge_clearance": "error" } })).await;
        assert_eq!(body["error"]["kind"], "file_not_found", "{body}");
        assert!(!pro.exists());
    }

    /// KiCad's DRC decides: kicad-cli reports the board's
    /// `copper_edge_clearance` violations at the severity set, and none once
    /// it is `ignore`, from pcbnew's own project file and from one with empty
    /// design settings.
    ///
    ///     KICAD_CLI=/path/to/kicad-cli cargo test -p copperpour-core --lib \
    ///         drc_severity::tests::live -- --ignored
    #[tokio::test]
    #[ignore = "needs a real kicad-cli"]
    async fn live_run_drc_reports_copper_edge_clearance_as_set() {
        let cli = std::env::var("KICAD_CLI").unwrap_or_else(|_| "kicad-cli".to_string());
        for kicad in [true, false] {
            let (dir, pro) = project(kicad).await;
            let board = dir.path().join("j1.kicad_pcb");
            let edge = |severity: &str| {
                let (cli, board, severity) = (cli.clone(), board.clone(), severity.to_string());
                async move {
                    let body =
                        call_with(&cli, "run_drc", json!({ "board": board, "limit": 500 })).await;
                    assert_ne!(body["is_error"], true, "{body}");
                    let found: Vec<_> = body["violations"]
                        .as_array()
                        .unwrap()
                        .iter()
                        .filter(|violation| violation["rule"] == "copper_edge_clearance")
                        .collect();
                    match severity.as_str() {
                        "ignore" => assert!(found.is_empty(), "{body}"),
                        _ => {
                            assert!(!found.is_empty(), "{body}");
                            for violation in found {
                                assert_eq!(violation["severity"], severity, "{body}");
                            }
                        }
                    }
                }
            };
            // KiCad's default, and pcbnew's file, report it as an error.
            edge("error").await;
            for severity in ["warning", "ignore", "error"] {
                let body = call(json!({
                    "project": pro,
                    "severities": { "copper_edge_clearance": severity }
                }))
                .await;
                assert_ne!(body["is_error"], true, "{body}");
                edge(severity).await;
            }
        }
    }
}
