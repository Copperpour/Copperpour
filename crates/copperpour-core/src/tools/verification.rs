//! `verification` toolset — DRC, design rules, KiCAD UI management, routing utilities.
//!
//! DRC delegates to `kicad-cli`. Design rules are read/written as S-expressions.
//! KiCAD UI management uses process inspection + subprocess spawning.

use crate::mcp::error::ToolErrorKind;
use crate::mcp::protocol::CallToolResult;
use crate::tool;
use crate::tools::pcb_board::block_tag;
use crate::tools::pcb_components::escape_sexp_string;
use crate::tools::{
    get_path, invalid_arg, opt_str, require_f64, require_str, ToolContext, ToolDef,
};
use copperpour_sexp::board::exact_coordinate_pair;
use copperpour_sexp::geometry::round6;
use copperpour_sexp::net::{collect_net_keys, names_nets_in_place, net_name};
use copperpour_sexp::writer::{
    apply_edits, find_direct_child_blocks, new_uuid, read_consistent, write_atomic_if_unchanged,
    SexpEdit,
};
use copperpour_sexp::{parse_sexp, SexpError, SexpNode};
use serde_json::{json, Value};
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::{Path, PathBuf};
use tokio::task;

// ─── Tool definitions ─────────────────────────────────────────────────────────

pub fn tools() -> Vec<ToolDef> {
    vec![
        tool!(
            "run_drc",
            "Run the Design Rule Check on the PCB and return structured violation results, \
             with error/warning counts overall and by rule, and optional result filtering by rule. Prefer this over \
             `get_drc_violations` (pcb_export toolset) — they run the same underlying \
             kicad-cli check, but `run_drc` returns a cleaner breakdown. KiCad runs the \
             complete configured DRC ruleset; kicad-cli has no per-test selector. \
             `ignored_checks` names the checks whose severity is ignore: they were not run, \
             so no count covers them (`set_drc_severity` sets a check's severity).",
            json!({
                "type": "object",
                "properties": {
                    "board": { "type": "string", "description": "Path to .kicad_pcb file" },
                    "output": { "type": "string", "description": "Optional path to write DRC report JSON" },
                    "sync_live_board": { "type": "boolean", "default": false, "description": "Bind the requested open board, optionally refill, save and verify its persisted snapshot before CLI DRC. Finish other mutations first." },
                    "refill_zones": { "type": "boolean", "default": false, "description": "Refill before checking: persisted IPC fill with sync_live_board, analysis-only CLI fill otherwise." },
                    "skip_creepage": { "type": "boolean", "default": false, "description": "Check without the project's creepage constraints (KiCad checks creepage single-threaded over every net pair: minutes to hours on a large board). Every other rule still applies; the result says creepage_skipped. For iterations only: run the full DRC before manufacturing." },
                    "severity": {
                        "type": "string",
                        "description": "Minimum violation severity to include: 'error', 'warning' (default), 'info'",
                        "default": "warning"
                    },
                    "rules": {
                        "type": "array",
                        "items": { "type": "string" },
                        "description": "Include only exact, case-sensitive rule names in the returned results and by_rule summary. Omit for all rules; [] selects none; duplicate names are coalesced. Does not limit KiCad checks or the complete output report."
                    },
                    "limit": {
                        "type": "integer",
                        "description": "Maximum violations to display (0 for counts only). Full DRC and output report are unaffected.",
                        "minimum": 0,
                        "default": 50
                    }
                },
                "required": ["board"]
            }),
            |args, ctx| async move { handle_run_drc(args, ctx).await }
        )
        .with_board_access(crate::tools::BoardAccess::CliSaved),
        super::drc_severity::tool(),
        tool!(
            "set_design_rules",
            "Set board-level design rules (clearance, trace width, via size, via annular width, \
             hole clearance, copper-to-edge clearance, silkscreen text minima) in the sibling \
             KiCAD project file. Refuses while KiCad holds the board or the project (its \
             ~<project>.kicad_pro.lck), because pcbnew and Eeschema rewrite the project file \
             from their own copy when they save.",
            json!({
                "type": "object",
                "properties": {
                    "board": { "type": "string", "description": "Path to .kicad_pcb file" },
                    "min_clearance": { "type": "number", "minimum": 0, "maximum": 25, "description": "Minimum clearance in mm (0 to 25, the range KiCad loads)" },
                    "min_trace_width": { "type": "number", "minimum": 0, "maximum": 25, "description": "Minimum trace width in mm (0 to 25, the range KiCad loads)" },
                    "min_via_drill": { "type": "number", "minimum": 0, "maximum": 25, "description": "Minimum via drill diameter in mm (0 to 25, the range KiCad loads)" },
                    "min_via_size": { "type": "number", "minimum": 0, "maximum": 25, "description": "Minimum via pad diameter in mm (0 to 25, the range KiCad loads)" },
                    "min_hole_to_hole": { "type": "number", "minimum": 0, "maximum": 10, "description": "Minimum hole-to-hole clearance in mm (0 to 10, the range KiCad loads)" },
                    "min_via_annular_width": { "type": "number", "minimum": 0, "maximum": 25, "description": "Board-wide minimum annular ring width in mm for vias and plated through-hole pads (0 to 25, the range KiCad loads). A DRC limit only: it resizes no via or pad and does not prove a fab accepts the board." },
                    "min_hole_clearance": { "type": "number", "minimum": 0, "maximum": 100, "description": "Board-wide minimum hole-to-copper clearance in mm (0 to 100, the range KiCad loads). A DRC limit only: it moves no item and does not prove a fab accepts the board." },
                    "min_copper_edge_clearance": { "type": "number", "minimum": -0.01, "maximum": 25, "description": "Board-wide minimum copper-to-board-edge clearance in mm (-0.01 to 25, the range KiCad loads)" },
                    "min_text_height": { "type": "number", "minimum": 0, "maximum": 100, "description": "Board-wide minimum silkscreen text height in mm (0 to 100, the range KiCad loads)" },
                    "min_text_thickness": { "type": "number", "minimum": 0, "maximum": 25, "description": "Board-wide minimum silkscreen text stroke thickness in mm (0 to 25, the range KiCad loads). These minima are DRC limits only: they change no item on the board and do not prove a fab accepts it." }
                },
                "required": ["board"]
            }),
            |args, ctx| async move { handle_set_design_rules(args, ctx).await }
        )
        .with_board_access(crate::tools::BoardAccess::ClosedBoardOnly),
        tool!(
            "get_design_rules",
            "Return the current design rule constraints defined in the sibling KiCAD project file. \
             Reads the saved .kicad_pro file on disk (unsaved editor changes are excluded) and \
             reports provenance.",
            json!({
                "type": "object",
                "properties": {
                    "board": { "type": "string", "description": "Path to .kicad_pcb file" }
                },
                "required": ["board"]
            }),
            |args, ctx| async move { handle_get_design_rules(args, ctx).await }
        )
        .with_board_access(crate::tools::BoardAccess::SavedSnapshot),
        tool!(
            "set_predefined_sizes",
            "Write the PCB editor Pre-defined Sizes list (track widths and via pad/drill \
             pairs) into the sibling .kicad_pro. These populate the Track/Via dropdowns \
             and W/Shift+W while routing; they are not DRC limits and do not change \
             netclasses. A leading 0 mm track and 0/0 via is always kept as the \
             'use netclass' sentinel. Pass only the lists you want to replace. KiCad \
             reads the change on next project open. The board file is not modified. \
             Refuses while KiCad holds the board or the project (its \
             ~<project>.kicad_pro.lck), because pcbnew and Eeschema rewrite the project \
             file from their own copy when they save.",
            json!({
                "type": "object",
                "properties": {
                    "board": { "type": "string", "description": "Path to .kicad_pcb file; the sibling .kicad_pro is edited" },
                    "track_widths": {
                        "type": "array",
                        "description": "Track widths in mm for the router dropdown, excluding the 0 mm netclass sentinel (that row is always prepended). An empty array leaves only the sentinel.",
                        "items": { "type": "number" }
                    },
                    "via_dimensions": {
                        "type": "array",
                        "description": "Via pad/drill pairs in mm for the router dropdown, excluding the 0/0 netclass sentinel (that row is always prepended). An empty array leaves only the sentinel.",
                        "items": {
                            "type": "object",
                            "properties": {
                                "diameter": { "type": "number", "description": "Via pad diameter in mm" },
                                "drill": { "type": "number", "description": "Via drill diameter in mm" }
                            },
                            "required": ["diameter", "drill"]
                        }
                    }
                },
                "required": ["board"]
            }),
            |args, ctx| async move { handle_set_predefined_sizes(args, ctx).await }
        )
        .with_board_access(crate::tools::BoardAccess::ClosedBoardOnly),
        tool!(
            "get_predefined_sizes",
            "Return the PCB editor Pre-defined Sizes list from the sibling .kicad_pro: \
             track_widths (mm) and via_dimensions (diameter/drill mm), including the \
             0 / 0,0 netclass sentinel KiCad keeps at the front of each list. Reads the \
             saved .kicad_pro file on disk (unsaved editor changes are excluded) and reports \
             provenance.",
            json!({
                "type": "object",
                "properties": {
                    "board": { "type": "string", "description": "Path to .kicad_pcb file; the sibling .kicad_pro is read" }
                },
                "required": ["board"]
            }),
            |args, ctx| async move { handle_get_predefined_sizes(args, ctx).await }
        )
        .with_board_access(crate::tools::BoardAccess::SavedSnapshot),
        tool!(
            "check_kicad_ui",
            "Check whether the KiCad GUI application is running and whether IPC responds within a bounded timeout. When IPC does not answer, ipc_failure.kind says why: not_configured, no_listener, access_denied (the operating system refused this account; likely a different account or a restrictive ACL, as with a sandboxed client), handshake_failed (the listener did not complete NNG's handshake, so it is probably not KiCad; this takes NNG's 10 s limit, so pass timeout_seconds above 10 to see it), transport_error, or request_failed (the request did not complete and may have reached the endpoint; an explicit KiCad status such as AS_NOT_READY proves receipt, a timeout or malformed reply does not), and ipc_failure.recovery provides structured cause, mutation_outcome, retry_safe, next_steps, and troubleshooting_url. ipc_failure is null when no kind was established: the Ping succeeded with AS_OK, or this check's own timeout expired first (timed_out: true).",
            json!({
                "type": "object",
                "properties": {
                    "timeout_seconds": {
                        "type": "integer",
                        "description": "Timeout for the health check in seconds",
                        "minimum": 1,
                        "maximum": 300,
                        "default": 5
                    }
                },
                "required": []
            }),
            |args, ctx| async move { handle_check_kicad_ui(args, ctx).await }
        ),
        tool!(
            "launch_kicad_ui",
            "Launch the KiCAD GUI application and optionally open a project file. When KiCad \
             exits during startup, the error quotes its stderr; on Linux it also says when the \
             server has no DISPLAY or WAYLAND_DISPLAY, as when it runs from an SSH session.",
            json!({
                "type": "object",
                "properties": {
                    "project": { "type": "string", "description": "Path to .kicad_pro file to open (optional)" },
                    "display": {
                        "type": "string",
                        "description": "X display to open KiCad on, such as \":0\" for the desktop when the server runs from an SSH session. Local displays only (:N or :N.S). Sets DISPLAY and GDK_BACKEND=x11 and unsets WAYLAND_DISPLAY for KiCad, so GTK uses X11 (Xwayland on a Wayland desktop); KiCad takes XAUTHORITY from the server's environment. Linux and other X11 systems only: refused on Windows and macOS."
                    },
                    "wait_ready": {
                        "type": "boolean",
                        "description": "Wait until KiCAD IPC is responsive before returning",
                        "default": true
                    },
                    "timeout_seconds": {
                        "type": "integer",
                        "description": "Maximum wait time in seconds",
                        "default": 30
                    }
                },
                "required": []
            }),
            |args, ctx| async move { handle_launch_kicad_ui(args, ctx).await }
        ),
        tool!(
            "open_board",
            "Open a .kicad_pcb in KiCad's PCB editor (pcbnew) and wait until a KiCad IPC \
             endpoint reports that exact board; when several endpoints answer, the calls \
             that follow use that one. An endpoint already holding the board is used \
             without starting an editor. requested_open is false, with a note, when none \
             reports it within timeout_seconds.",
            json!({
                "type": "object",
                "properties": {
                    "board": { "type": "string", "description": "Path to the .kicad_pcb file to open" },
                    "display": {
                        "type": "string",
                        "description": "X display to open the editor on, as launch_kicad_ui takes it. Linux and other X11 systems only: refused on Windows and macOS."
                    },
                    "timeout_seconds": {
                        "type": "integer",
                        "description": "Maximum wait for the board, in seconds",
                        "minimum": 1,
                        "maximum": 300,
                        "default": 30
                    }
                },
                "required": ["board"]
            }),
            |args, ctx| async move { handle_open_board(args, ctx).await }
        )
        .with_board_access(crate::tools::BoardAccess::EditorLifecycle),
        tool!(
            "copy_routing_pattern",
            "Copy the segments, arcs and vias inside a source region of a board KiCad is not \
             holding, shifted by (dest_x - src_x1, dest_y - src_y1). An item is copied when all \
             of its points (segment start/end, arc start/mid/end, via position) are inside the \
             region, edges included; one that crosses the edge is listed in `excluded_crossing` \
             and not copied. Copies keep every attribute and get fresh UUIDs.",
            json!({
                "type": "object",
                "properties": {
                    "board": { "type": "string", "description": "Path to .kicad_pcb file" },
                    "src_x1": { "type": "number", "description": "Source region bounding box min X" },
                    "src_y1": { "type": "number", "description": "Source region bounding box min Y" },
                    "src_x2": { "type": "number", "description": "Source region bounding box max X" },
                    "src_y2": { "type": "number", "description": "Source region bounding box max Y" },
                    "dest_x": { "type": "number", "description": "Destination anchor X (maps to src_x1)" },
                    "dest_y": { "type": "number", "description": "Destination anchor Y (maps to src_y1)" },
                    "net_map": {
                        "type": "object",
                        "additionalProperties": { "type": "string" },
                        "description": "Optional mapping from source net names to destination net names"
                    }
                },
                "required": ["board", "src_x1", "src_y1", "src_x2", "src_y2", "dest_x", "dest_y"]
            }),
            |args, ctx| async move { handle_copy_routing_pattern(args, ctx).await }
        )
        .with_board_access(crate::tools::BoardAccess::ClosedBoardOnly),
        tool!(
            "set_layer_constraints",
            "Set per-layer design constraints (e.g. min trace width, clearance) in the sibling .kicad_dru custom rules file.",
            json!({
                "type": "object",
                "properties": {
                    "board": { "type": "string", "description": "Path to .kicad_pcb file" },
                    "layer": { "type": "string", "description": "Layer name (e.g. 'F.Cu', 'B.Cu')" },
                    "min_clearance": { "type": "number", "description": "Minimum clearance for this layer in mm" },
                    "min_trace_width": { "type": "number", "description": "Minimum trace width for this layer in mm" }
                },
                "required": ["board", "layer"]
            }),
            |args, ctx| async move { handle_set_layer_constraints(args, ctx).await }
        )
        .with_board_access(crate::tools::BoardAccess::ClosedBoardOnly),
        super::custom_rules::tool(),
        super::custom_rules::delete_tool(),
        super::custom_rules::list_tool(),
        tool!(
            "check_clearance",
            "Measure spacing between two footprints on the requested board. `mode: anchor` \
             (the compatibility default) returns straight-line placement-anchor distance. \
             `mode: courtyard` returns edge-to-edge distance between transformed, authored \
             courtyard bounding boxes for same-side footprints. It refuses missing or \
             malformed courtyard geometry and does not substitute pads or anchors. Neither \
             mode measures pad, trace or copper clearance; use `run_drc` for electrical \
             clearance and `get_component_pads` for pad geometry. Reads the live KiCad board \
             when open, otherwise the saved file ('board_source' selects auto, live, or \
             saved), and reports the source.",
            json!({
                "type": "object",
                "properties": {
                    "board": { "type": "string", "description": "Path to .kicad_pcb file" },
                    "ref1":  { "type": "string", "description": "First component reference (e.g. 'U1')" },
                    "ref2":  { "type": "string", "description": "Second component reference (e.g. 'C1')" },
                    "mode": {
                        "type": "string",
                        "enum": ["anchor", "courtyard"],
                        "default": "anchor",
                        "description": "anchor: placement-origin distance; courtyard: authored courtyard bbox edge distance"
                    },
                    "board_source": super::board_source::board_source_schema()
                },
                "required": ["board", "ref1", "ref2"]
            }),
            |args, ctx| async move { handle_check_clearance(args, ctx).await }
        )
        .with_board_access(crate::tools::BoardAccess::LivePreferredWithFallback),
    ]
}

// ─── Handlers ─────────────────────────────────────────────────────────────────

#[derive(Default, serde::Serialize)]
struct DrcRuleCounts {
    errors: usize,
    warnings: usize,
}

fn drc_rules_filter(args: &Value) -> Result<Option<BTreeSet<String>>, CallToolResult> {
    let Some(value) = args.get("rules") else {
        return Ok(None);
    };
    let Some(values) = value.as_array() else {
        return Err(invalid_arg(
            "rules",
            "expected an array of exact rule-name strings",
        ));
    };
    let mut rules = BTreeSet::new();
    for (index, value) in values.iter().enumerate() {
        let Some(name) = value.as_str() else {
            return Err(invalid_arg(
                &format!("rules[{index}]"),
                "expected a rule-name string",
            ));
        };
        rules.insert(name.to_owned());
    }
    Ok(Some(rules))
}

async fn handle_run_drc(
    args: &serde_json::Value,
    ctx: &ToolContext,
) -> anyhow::Result<CallToolResult> {
    let board = get_path(args, "board")?;
    let severity_filter = args["severity"].as_str().unwrap_or("warning");
    let min_rank = super::drc::drc_severity_rank(severity_filter);
    let limit = match super::response_limit(args, 50) {
        Ok(limit) => limit,
        Err(error) => return Ok(error),
    };

    let rules_filter = match drc_rules_filter(args) {
        Ok(rules) => rules,
        Err(error) => return Ok(error),
    };

    let (report, provenance) = match super::drc::run(ctx, &board, args).await? {
        Ok(result) => result,
        Err(error) => return Ok(error),
    };

    // Optionally write report
    if let Some(out_path) = args["output"].as_str() {
        let json = serde_json::to_string_pretty(&report)?;
        if let Err(error) = write_report(out_path, &json).await {
            return super::drc::report_write_failure(&provenance, &board, out_path, error);
        }
    }

    // Every category, not just `violations`. An unrouted net is reported under
    // `unconnected_items`, which Konnect used to discard — so a board that
    // KiCad called unrouted came back from here clean (#245).
    let filtered: Vec<_> = report
        .all()
        .filter(|v| super::drc::drc_severity_rank(&v.severity) >= min_rank)
        .filter(|v| {
            rules_filter
                .as_ref()
                .is_none_or(|rules| rules.contains(v.rule.as_str()))
        })
        .collect();

    let errors = filtered.iter().filter(|v| v.severity == "error").count();
    let warnings = filtered.iter().filter(|v| v.severity == "warning").count();
    // Aggregate the complete selected set, never the displayed prefix.
    let mut by_rule: BTreeMap<&str, DrcRuleCounts> = BTreeMap::new();
    for violation in &filtered {
        let counts = by_rule.entry(violation.rule.as_str()).or_default();
        match violation.severity.as_str() {
            "error" => counts.errors += 1,
            "warning" => counts.warnings += 1,
            _ => {}
        }
    }
    let bounded = super::BoundedList::new(&filtered, limit);
    let missing = report.missing_categories();

    Ok(CallToolResult::text(
        serde_json::to_string(&json!({
            "total_violations": report.all().count(),
            "source": provenance.source,
            "live_board_synced": provenance.live_board_synced,
            "zones_refilled": provenance.zones_refilled,
            "zone_refill_source": provenance.zone_refill_source,
            "creepage_skipped": report.creepage_skipped,
            "design_rule_violations": report.violations.len(),
            // Null, not zero, when this kicad-cli did not report the category:
            // "none found" and "never asked" are different answers.
            "unconnected_items": report.unconnected_items.as_ref().map(Vec::len),
            "schematic_parity": report.schematic_parity.as_ref().map(Vec::len),
            "categories_not_reported": missing,
            // Present only when parity is null because no schematic sits
            // beside the board — kicad-cli writes an empty array then, which
            // is not a checked zero (#516).
            "schematic_parity_diagnostic": report.schematic_parity_diagnostic,
            // The checks this run had switched off, from KiCad's own report.
            // None of them can appear below, so a count of zero says nothing
            // about them. Null when the report carries no such list.
            "ignored_checks": report.ignored_checks.as_ref().map(|checks| {
                checks.iter().map(|check| check.key.as_str()).collect::<Vec<_>>()
            }),
            "filtered_count": filtered.len(),
            "errors": errors,
            "warnings": warnings,
            "severity_filter": severity_filter,
            "rules_filter": rules_filter,
            "by_rule": by_rule,
            "shown": bounded.shown,
            "truncated": bounded.truncated,
            "violations": bounded.items.iter().copied().map(super::drc::drc_finding_json).collect::<Vec<_>>()
        }))
        .unwrap(),
    ))
}

/// Write a report, creating the directory the caller named.
///
/// A missing parent used to surface as a bare OS "path not found" with nothing
/// naming what was missing — the export tools next door already call
/// `create_dir_all` first.
async fn write_report(out_path: &str, contents: &str) -> anyhow::Result<()> {
    super::drc::write_report(out_path, contents).await
}

// ─── Design rules helpers ────────────────────────────────────────────────────

fn sibling_project_path(board: &Path) -> PathBuf {
    board.with_extension("kicad_pro")
}

pub(super) fn sibling_custom_rules_path(board: &Path) -> PathBuf {
    board.with_extension("kicad_dru")
}

fn project_rules_mut(
    project: &mut serde_json::Value,
) -> anyhow::Result<&mut serde_json::Map<String, serde_json::Value>> {
    let project = project
        .as_object_mut()
        .ok_or_else(|| anyhow::anyhow!("KiCAD project root must be a JSON object"))?;
    let board = project
        .entry("board")
        .or_insert_with(|| json!({}))
        .as_object_mut()
        .ok_or_else(|| anyhow::anyhow!("KiCAD project 'board' must be a JSON object"))?;
    let design_settings = board
        .entry("design_settings")
        .or_insert_with(|| json!({}))
        .as_object_mut()
        .ok_or_else(|| {
            anyhow::anyhow!("KiCAD project 'board.design_settings' must be a JSON object")
        })?;
    design_settings
        .entry("rules")
        .or_insert_with(|| json!({}))
        .as_object_mut()
        .ok_or_else(|| {
            anyhow::anyhow!("KiCAD project 'board.design_settings.rules' must be a JSON object")
        })
}

fn project_rule_value(project: &serde_json::Value, key: &str) -> Option<f64> {
    project["board"]["design_settings"]["rules"][key].as_f64()
}

/// A board-wide minimum in `board.design_settings.rules`.
struct DesignRule {
    /// The tool argument and response key.
    arg: &'static str,
    /// The `.kicad_pro` key.
    key: &'static str,
    /// The range KiCad 10.0.6 loads, in mm. KiCad replaces a value outside
    /// it with its default when it opens the project.
    range_mm: Option<(f64, f64)>,
}

/// A function rather than a `const`, so the schema-usage test sees the
/// handler read each argument.
fn design_rules() -> &'static [DesignRule] {
    &[
        DesignRule {
            arg: "min_clearance",
            key: "min_clearance",
            range_mm: Some((0.0, 25.0)),
        },
        DesignRule {
            arg: "min_trace_width",
            key: "min_track_width",
            range_mm: Some((0.0, 25.0)),
        },
        DesignRule {
            arg: "min_via_drill",
            key: "min_through_hole_diameter",
            range_mm: Some((0.0, 25.0)),
        },
        DesignRule {
            arg: "min_via_size",
            key: "min_via_diameter",
            range_mm: Some((0.0, 25.0)),
        },
        DesignRule {
            arg: "min_hole_to_hole",
            key: "min_hole_to_hole",
            range_mm: Some((0.0, 10.0)),
        },
        DesignRule {
            arg: "min_via_annular_width",
            key: "min_via_annular_width",
            range_mm: Some((0.0, 25.0)),
        },
        DesignRule {
            arg: "min_hole_clearance",
            key: "min_hole_clearance",
            range_mm: Some((0.0, 100.0)),
        },
        DesignRule {
            arg: "min_copper_edge_clearance",
            key: "min_copper_edge_clearance",
            range_mm: Some((-0.01, 25.0)),
        },
        DesignRule {
            arg: "min_text_height",
            key: "min_text_height",
            range_mm: Some((0.0, 100.0)),
        },
        DesignRule {
            arg: "min_text_thickness",
            key: "min_text_thickness",
            range_mm: Some((0.0, 25.0)),
        },
    ]
}

/// The rule's value in mm, if given. A present value that is not a number,
/// or lies outside KiCad's range, is refused rather than skipped.
fn design_rule_arg(args: &Value, rule: &DesignRule) -> Result<Option<f64>, CallToolResult> {
    let value = match &args[rule.arg] {
        Value::Null => return Ok(None),
        value => value
            .as_f64()
            .ok_or_else(|| crate::tools::invalid_arg(rule.arg, "must be a number in mm"))?,
    };
    match rule.range_mm {
        Some((min, max)) if !(min..=max).contains(&value) => Err(crate::tools::invalid_arg(
            rule.arg,
            &format!(
                "must be between {min} and {max} mm, got {value}; KiCad replaces a value \
                 outside that range with its default"
            ),
        )),
        _ => Ok(Some(value)),
    }
}

/// Whether the project holds the rule at `value`, compared in nm as KiCad
/// stores it, so a float that does not round-trip through JSON still matches.
fn rule_holds(project: &Value, rule: &DesignRule, value: f64) -> bool {
    let nm = |mm: f64| (mm * 1e6).round();
    project_rule_value(project, rule.key).is_some_and(|stored| nm(stored) == nm(value))
}

fn named_rule_range(content: &str, name: &str) -> Option<(usize, usize)> {
    let needle = format!("(rule \"{name}\"");
    let start = content.find(&needle)?;
    let mut depth = 0i32;
    let mut in_string = false;
    let mut escaped = false;
    let mut in_comment = false;

    for (offset, character) in content[start..].char_indices() {
        if in_comment {
            if character == '\n' {
                in_comment = false;
            }
            continue;
        }
        if in_string {
            if escaped {
                escaped = false;
            } else if character == '\\' {
                escaped = true;
            } else if character == '"' {
                in_string = false;
            }
            continue;
        }
        match character {
            '#' => in_comment = true,
            '"' => in_string = true,
            '(' => depth += 1,
            ')' => {
                depth -= 1;
                if depth == 0 {
                    return Some((start, start + offset + character.len_utf8()));
                }
            }
            _ => {}
        }
    }
    None
}

fn upsert_named_rule(content: &str, name: &str, rule: &str) -> String {
    if let Some((start, end)) = named_rule_range(content, name) {
        return format!("{}{}{}", &content[..start], rule, &content[end..]);
    }

    let mut result = content.trim_end().to_string();
    if !result.is_empty() {
        result.push_str("\n\n");
    }
    result.push_str(rule);
    result.push('\n');
    result
}

fn layer_rule(name: &str, constraint: &str, value: f64, layer: &str) -> String {
    format!("(rule \"{name}\"\n  (constraint {constraint} (min {value}mm))\n  (layer \"{layer}\"))")
}

async fn handle_set_design_rules(
    args: &serde_json::Value,
    ctx: &ToolContext,
) -> anyhow::Result<CallToolResult> {
    let board = get_path(args, "board")?;
    let mut requested = Vec::new();
    for rule in design_rules() {
        match design_rule_arg(args, rule) {
            Ok(Some(value)) => requested.push((rule, value)),
            Ok(None) => {}
            Err(refusal) => return Ok(refusal),
        }
    }
    // KiCad rewrites the project file from memory when it saves the board, so
    // a rule written here while it holds the board is reverted (#791). Asked
    // before the read too: the file can be older than what KiCad holds.
    if let Some(refusal) =
        crate::tools::pcb_board::refuse_if_board_open_in_kicad(ctx, &board, "design rule").await?
    {
        return Ok(refusal);
    }
    let project_path = sibling_project_path(&board);
    // Eeschema rewrites the project file on save too, and holds no board (#804).
    if let Some(refusal) =
        crate::tools::pcb_board::refuse_if_project_locked_by_kicad(&project_path, "design rule")
    {
        return Ok(refusal);
    }
    let project_content = tokio::fs::read_to_string(&project_path).await?;
    let mut project: serde_json::Value = serde_json::from_str(&project_content)?;

    let mut changed = Vec::new();
    for &(rule, value) in &requested {
        if !rule_holds(&project, rule, value) {
            project_rules_mut(&mut project)?.insert(rule.key.to_string(), json!(value));
            changed.push(format!("{} = {}", rule.key, value));
        }
    }

    if !changed.is_empty() {
        if let Some(refusal) = crate::tools::pcb_board::write_project_if_unchanged(
            &project_path,
            "set_design_rules",
            &project_content,
            &project,
        )? {
            return Ok(refusal);
        }
        // The file is replaced from here on, so a failed re-read is uncertain.
        let reread = tokio::fs::read_to_string(&project_path)
            .await
            .map_err(anyhow::Error::from)
            .and_then(|text| Ok(serde_json::from_str(&text)?));
        project = match reread {
            Ok(project) => project,
            Err(error) => {
                return Ok(crate::tools::file_outcome_uncertain(
                    &project_path,
                    "set_design_rules",
                    format!("the project could not be re-read: {error}"),
                ))
            }
        };
        let missing = rules_not_holding(&project, &requested);
        if !missing.is_empty() {
            return Ok(crate::tools::file_outcome_uncertain(
                &project_path,
                "set_design_rules",
                format!("the re-read project does not hold {}", missing.join(", ")),
            ));
        }
    }

    Ok(CallToolResult::text(
        serde_json::to_string(&json!({
            "success": true,
            "project": project_path,
            "changed": changed,
            "rules": design_rules_json(&project)
        }))
        .unwrap(),
    ))
}

/// The requested rules the project does not hold at the requested value.
fn rules_not_holding(project: &Value, requested: &[(&DesignRule, f64)]) -> Vec<&'static str> {
    requested
        .iter()
        .filter(|(rule, value)| !rule_holds(project, rule, *value))
        .map(|(rule, _)| rule.key)
        .collect()
}

fn design_rules_json(project: &Value) -> Value {
    design_rules()
        .iter()
        .map(|rule| {
            (
                rule.arg.to_string(),
                json!(project_rule_value(project, rule.key)),
            )
        })
        .collect::<serde_json::Map<_, _>>()
        .into()
}

async fn handle_get_design_rules(
    args: &serde_json::Value,
    ctx: &ToolContext,
) -> anyhow::Result<CallToolResult> {
    let board = get_path(args, "board")?;
    let project_path = sibling_project_path(&board);
    let content = tokio::fs::read_to_string(&project_path).await?;
    let project: serde_json::Value = serde_json::from_str(&content)?;

    let mut body = json!({
        "board": board.to_str().unwrap_or(""),
        "project": project_path.to_str().unwrap_or(""),
        "rules": design_rules_json(&project)
    });
    super::board_source::provenance(
        &mut body,
        json!({ "rules": super::board_source::FROM_PROJECT_FILE }),
        super::board_source::saved_project_evidence(ctx, &board),
    );
    Ok(CallToolResult::text(serde_json::to_string(&body).unwrap()))
}

// ─── Pre-defined sizes (Board Setup → Design Rules → Pre-defined Sizes) ───────
//
// KiCad stores the router palette in board.design_settings.track_widths /
// via_dimensions. A leading 0 (track) or {diameter:0, drill:0} (via) is the
// "use netclass values" sentinel the dropdown always shows first. These lists
// are not DRC floors and are not netclass widths.

fn project_design_settings_mut(
    project: &mut Value,
) -> anyhow::Result<&mut serde_json::Map<String, Value>> {
    let project = project
        .as_object_mut()
        .ok_or_else(|| anyhow::anyhow!("KiCAD project root must be a JSON object"))?;
    let board = project
        .entry("board")
        .or_insert_with(|| json!({}))
        .as_object_mut()
        .ok_or_else(|| anyhow::anyhow!("KiCAD project 'board' must be a JSON object"))?;
    board
        .entry("design_settings")
        .or_insert_with(|| json!({}))
        .as_object_mut()
        .ok_or_else(|| {
            anyhow::anyhow!("KiCAD project 'board.design_settings' must be a JSON object")
        })
}

/// The project's path, the text read, and that text parsed.
fn load_sibling_project(
    board: &Path,
) -> anyhow::Result<Result<(PathBuf, String, Value), CallToolResult>> {
    let project_path = sibling_project_path(board);
    if !project_path.exists() {
        return Ok(Err(CallToolResult::error(format!(
            "No project file at {} — Pre-defined Sizes live in the .kicad_pro, \
             and a list written anywhere else is never read. Create the project \
             (KiCad: File > Save a Copy, or place the board inside a project) and retry.",
            project_path.display()
        ))));
    }
    let text = std::fs::read_to_string(&project_path)?;
    let settings: Value = serde_json::from_str(&text)
        .map_err(|e| anyhow::anyhow!("{} is not valid JSON: {e}", project_path.display()))?;
    Ok(Ok((project_path, text, settings)))
}

fn finite_positive(value: f64, field: &str) -> Result<f64, CallToolResult> {
    if value.is_finite() && value > 0.0 {
        Ok(value)
    } else {
        Err(CallToolResult::error(format!(
            "Argument '{field}' is invalid: must be a finite number greater than 0 mm, got {value}"
        )))
    }
}

/// Palette track widths, with the 0 mm netclass sentinel always first.
/// Caller zeros are dropped rather than duplicated.
fn parse_track_widths(raw: &[Value]) -> Result<Vec<Value>, CallToolResult> {
    let mut out = vec![json!(0.0)];
    for (i, item) in raw.iter().enumerate() {
        let width = item.as_f64().ok_or_else(|| {
            CallToolResult::error(format!(
                "Argument 'track_widths[{i}]' is invalid: missing or not a number"
            ))
        })?;
        if width == 0.0 {
            continue;
        }
        let width = finite_positive(width, &format!("track_widths[{i}]"))?;
        let encoded = json!(width);
        if !out.contains(&encoded) {
            out.push(encoded);
        }
    }
    Ok(out)
}

/// Palette vias, with the 0/0 netclass sentinel always first.
fn parse_via_dimensions(raw: &[Value]) -> Result<Vec<Value>, CallToolResult> {
    let mut out = vec![json!({ "diameter": 0.0, "drill": 0.0 })];
    for (i, item) in raw.iter().enumerate() {
        let obj = item.as_object().ok_or_else(|| {
            CallToolResult::error(format!(
                "Argument 'via_dimensions[{i}]' is invalid: missing or not an object"
            ))
        })?;
        let diameter = obj.get("diameter").and_then(Value::as_f64).ok_or_else(|| {
            CallToolResult::error(format!(
                "Argument 'via_dimensions[{i}].diameter' is invalid: missing or not a number"
            ))
        })?;
        let drill = obj.get("drill").and_then(Value::as_f64).ok_or_else(|| {
            CallToolResult::error(format!(
                "Argument 'via_dimensions[{i}].drill' is invalid: missing or not a number"
            ))
        })?;
        if diameter == 0.0 && drill == 0.0 {
            continue;
        }
        let diameter = finite_positive(diameter, &format!("via_dimensions[{i}].diameter"))?;
        let drill = finite_positive(drill, &format!("via_dimensions[{i}].drill"))?;
        if diameter <= drill {
            return Err(CallToolResult::error(format!(
                "Argument 'via_dimensions[{i}]' is invalid: diameter ({diameter} mm) \
                 must be greater than drill ({drill} mm)"
            )));
        }
        let encoded = json!({ "diameter": diameter, "drill": drill });
        if !out.contains(&encoded) {
            out.push(encoded);
        }
    }
    Ok(out)
}

fn current_predefined_sizes(project: &Value) -> (Value, Value) {
    let settings = &project["board"]["design_settings"];
    let track_widths = settings["track_widths"].clone();
    let via_dimensions = settings["via_dimensions"].clone();
    (
        if track_widths.is_array() {
            track_widths
        } else {
            json!([0.0])
        },
        if via_dimensions.is_array() {
            via_dimensions
        } else {
            json!([{ "diameter": 0.0, "drill": 0.0 }])
        },
    )
}

async fn handle_set_predefined_sizes(
    args: &Value,
    ctx: &ToolContext,
) -> anyhow::Result<CallToolResult> {
    let board = get_path(args, "board")?;
    let tracks_arg = match args.get("track_widths") {
        None | Some(Value::Null) => None,
        Some(Value::Array(items)) => match parse_track_widths(items) {
            Ok(v) => Some(v),
            Err(e) => return Ok(e),
        },
        Some(_) => {
            return Ok(CallToolResult::error(
                "Argument 'track_widths' is invalid: missing or not an array".to_string(),
            ))
        }
    };
    let vias_arg = match args.get("via_dimensions") {
        None | Some(Value::Null) => None,
        Some(Value::Array(items)) => match parse_via_dimensions(items) {
            Ok(v) => Some(v),
            Err(e) => return Ok(e),
        },
        Some(_) => {
            return Ok(CallToolResult::error(
                "Argument 'via_dimensions' is invalid: missing or not an array".to_string(),
            ))
        }
    };
    if tracks_arg.is_none() && vias_arg.is_none() {
        return Ok(CallToolResult::error(
            "Argument 'track_widths' is invalid: name at least one of track_widths, via_dimensions"
                .to_string(),
        ));
    }

    if let Some(refusal) = crate::tools::pcb_board::refuse_if_board_open_in_kicad(
        ctx,
        &board,
        "Pre-defined Sizes list",
    )
    .await?
    {
        return Ok(refusal);
    }
    if let Some(refusal) = crate::tools::pcb_board::refuse_if_project_locked_by_kicad(
        &sibling_project_path(&board),
        "Pre-defined Sizes list",
    ) {
        return Ok(refusal);
    }

    let (project_path, project_text, mut project) = match load_sibling_project(&board)? {
        Ok(v) => v,
        Err(refusal) => return Ok(refusal),
    };

    let (mut track_widths, mut via_dimensions) = current_predefined_sizes(&project);
    let mut changed_fields = Vec::new();
    if let Some(next) = tracks_arg {
        let next = Value::Array(next);
        if next != track_widths {
            changed_fields.push("track_widths");
            track_widths = next;
        }
    }
    if let Some(next) = vias_arg {
        let next = Value::Array(next);
        if next != via_dimensions {
            changed_fields.push("via_dimensions");
            via_dimensions = next;
        }
    }

    if !changed_fields.is_empty() {
        let settings = project_design_settings_mut(&mut project)?;
        settings.insert("track_widths".into(), track_widths.clone());
        settings.insert("via_dimensions".into(), via_dimensions.clone());
        if let Some(refusal) = crate::tools::pcb_board::write_project_if_unchanged(
            &project_path,
            "set_predefined_sizes",
            &project_text,
            &project,
        )? {
            return Ok(refusal);
        }
    }

    Ok(CallToolResult::json(&json!({
        "success": true,
        "project": project_path,
        "changed": changed_fields,
        "track_widths": track_widths,
        "via_dimensions": via_dimensions,
        "note": "Pre-defined Sizes live in the project file and fill the Track/Via dropdowns. \
                 They are not DRC limits. KiCad reads the change on next project open."
    })))
}

async fn handle_get_predefined_sizes(
    args: &Value,
    ctx: &ToolContext,
) -> anyhow::Result<CallToolResult> {
    let board = get_path(args, "board")?;
    let (project_path, _, project) = match load_sibling_project(&board)? {
        Ok(v) => v,
        Err(refusal) => return Ok(refusal),
    };
    let (track_widths, via_dimensions) = current_predefined_sizes(&project);
    let mut body = json!({
        "project": project_path,
        "track_widths": track_widths,
        "via_dimensions": via_dimensions
    });
    super::board_source::provenance(
        &mut body,
        json!({
            "track_widths": super::board_source::FROM_PROJECT_FILE,
            "via_dimensions": super::board_source::FROM_PROJECT_FILE,
        }),
        super::board_source::saved_project_evidence(ctx, &board),
    );
    Ok(CallToolResult::json(&body))
}

// ─── KiCAD UI management ──────────────────────────────────────────────────────

const KICAD_GUI_PROCESS_NAMES: &[&str] = &["kicad", "pcbnew", "eeschema"];

fn is_kicad_process_name(name: &str) -> bool {
    let file_name = std::path::Path::new(name.trim_matches('"'))
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or(name)
        .to_ascii_lowercase();
    let stem = file_name.strip_suffix(".exe").unwrap_or(&file_name);
    KICAD_GUI_PROCESS_NAMES.contains(&stem)
}

fn process_list_has_kicad(output: &str) -> bool {
    output.lines().any(|line| {
        line.split_whitespace()
            .next()
            .is_some_and(is_kicad_process_name)
    })
}

/// Check if the KiCad project manager or either standalone editor is running.
fn is_kicad_running() -> bool {
    #[cfg(target_os = "windows")]
    {
        std::process::Command::new("tasklist")
            .output()
            .ok()
            .map(|output| process_list_has_kicad(&String::from_utf8_lossy(&output.stdout)))
            .unwrap_or(false)
    }
    #[cfg(not(target_os = "windows"))]
    {
        std::process::Command::new("ps")
            .args(["-A", "-o", "comm="])
            .output()
            .ok()
            .map(|output| process_list_has_kicad(&String::from_utf8_lossy(&output.stdout)))
            .unwrap_or(false)
    }
}

fn ui_running(process_detected: bool, ipc_responsive: bool) -> bool {
    process_detected || ipc_responsive
}

fn health_timeout_seconds(args: &serde_json::Value) -> Result<u64, CallToolResult> {
    timeout_seconds(args, 5)
}

/// `timeout_seconds`, 1 to 300, `default` when absent.
fn timeout_seconds(args: &serde_json::Value, default: u64) -> Result<u64, CallToolResult> {
    let timeout = match args.get("timeout_seconds") {
        None | Some(serde_json::Value::Null) => default,
        Some(value) => value.as_u64().ok_or_else(|| {
            CallToolResult::error_kind(
                crate::mcp::error::ToolErrorKind::InvalidArgument {
                    field: "timeout_seconds".to_string(),
                    reason: "must be an integer from 1 to 300".to_string(),
                },
                "Argument 'timeout_seconds' must be an integer from 1 to 300",
            )
        })?,
    };
    if !(1..=300).contains(&timeout) {
        return Err(CallToolResult::error_kind(
            crate::mcp::error::ToolErrorKind::InvalidArgument {
                field: "timeout_seconds".to_string(),
                reason: "must be between 1 and 300 seconds".to_string(),
            },
            "Argument 'timeout_seconds' must be between 1 and 300 seconds",
        ));
    }
    Ok(timeout)
}

async fn bounded_health_check<F, T>(
    timeout: std::time::Duration,
    future: F,
) -> Result<T, tokio::time::error::Elapsed>
where
    F: std::future::Future<Output = T>,
{
    tokio::time::timeout(timeout, future).await
}

async fn handle_check_kicad_ui(
    args: &serde_json::Value,
    ctx: &ToolContext,
) -> anyhow::Result<CallToolResult> {
    let timeout_seconds = match health_timeout_seconds(args) {
        Ok(timeout) => timeout,
        Err(error) => return Ok(error),
    };
    let endpoint = ctx.ipc_endpoint();
    let endpoint_for_check = endpoint.clone();
    let started = std::time::Instant::now();
    let check = async move {
        let process_detected = task::spawn_blocking(is_kicad_running).await?;
        let ipc = task::spawn_blocking(move || endpoint_for_check.client().ping_outcome()).await?;
        Ok::<_, tokio::task::JoinError>((process_detected, ipc))
    };

    match bounded_health_check(std::time::Duration::from_secs(timeout_seconds), check).await {
        Ok(result) => {
            let (process_detected, ipc) = result?;
            let ipc_responsive = ipc.is_responsive();
            Ok(CallToolResult::json(&json!({
                "running": ui_running(process_detected, ipc_responsive),
                "process_detected": process_detected,
                "ipc_responsive": ipc_responsive,
                "ipc_failure": crate::tools::ipc_failure_evidence_for_endpoint(&endpoint, &ipc),
                "timed_out": false,
                "timeout_seconds": timeout_seconds,
                "elapsed_ms": started.elapsed().as_millis() as u64
            })))
        }
        Err(_) => Ok(CallToolResult::json(&json!({
            "running": null,
            "process_detected": null,
            "ipc_responsive": false,
            "ipc_failure": null,
            "timed_out": true,
            "timeout_seconds": timeout_seconds,
            "elapsed_ms": started.elapsed().as_millis() as u64,
            "note": "KiCad health check exceeded the requested timeout"
        }))),
    }
}

/// How often the `wait_ready` loop pings KiCad, watching the process between.
const KICAD_IPC_POLL: std::time::Duration = std::time::Duration::from_millis(500);

/// The `display` argument: an X display name, refused where KiCad does not
/// use X11.
fn launch_display(args: &serde_json::Value) -> Result<Option<String>, CallToolResult> {
    let Some(value) = args.get("display").filter(|value| !value.is_null()) else {
        return Ok(None);
    };
    let Some(display) = value.as_str() else {
        return Err(invalid_arg("display", "must be a string"));
    };
    if cfg!(any(target_os = "windows", target_os = "macos")) {
        return Err(invalid_arg(
            "display",
            "applies only to X11 desktops (Linux and other Unix); KiCad on Windows and macOS \
             does not use DISPLAY",
        ));
    }
    // Local displays only: a `host:N` display would hand the design, and
    // the editor's input, to another machine.
    let screen = display
        .strip_prefix("unix:")
        .or_else(|| display.strip_prefix(':'))
        .unwrap_or_default();
    let (number, screen) = screen.split_once('.').unwrap_or((screen, "0"));
    let digits = |part: &str| !part.is_empty() && part.bytes().all(|b| b.is_ascii_digit());
    if !(digits(number) && digits(screen)) {
        return Err(invalid_arg(
            "display",
            "must be a local X display such as \":0\" or \":1.0\"",
        ));
    }
    Ok(Some(display.to_string()))
}

async fn handle_launch_kicad_ui(
    args: &serde_json::Value,
    ctx: &ToolContext,
) -> anyhow::Result<CallToolResult> {
    let wait_ready = args["wait_ready"].as_bool().unwrap_or(true);
    let timeout_secs = args["timeout_seconds"].as_u64().unwrap_or(30);
    let display = match launch_display(args) {
        Ok(display) => display,
        Err(refusal) => return Ok(refusal),
    };
    let binary =
        crate::kicad_install::gui_or_default(&ctx.config.kicad_binary, &ctx.config.kicad_cli);

    let mut cmd = std::process::Command::new(&binary);
    if let Some(project) = args["project"].as_str() {
        cmd.arg(project);
    }
    if let Some(display) = &display {
        // X11 even where GTK would prefer Wayland.
        cmd.env("DISPLAY", display)
            .env("GDK_BACKEND", "x11")
            .env_remove("WAYLAND_DISPLAY");
    }

    let mut kicad = match copperpour_viewer::launch::spawn(
        cmd,
        "KiCAD",
        |line| tracing::warn!(target: "kicad", "{line}"),
    ) {
        Ok(kicad) => kicad,
        Err(e) => {
            return Ok(CallToolResult::error(format!(
                "Failed to launch KiCAD ({}): {}",
                binary, e
            )))
        }
    };
    // Its exit report then says the display is missing; this adds the remedy.
    kicad.on_no_display(
        "Pass display (for example \":0\") to open KiCad on the desktop's X display.",
    );
    let exited = CallToolResult::error;
    // A new KiCad may bind a socket path a dead one used, leaving the candidate
    // set unchanged, so clear any ping backoff for follow-up calls (#32).
    ctx.reset_ipc_probe_backoff();

    if !wait_ready {
        if let Err(message) = kicad.watch(copperpour_viewer::launch::STARTUP_WINDOW).await {
            return Ok(exited(message));
        }
        return Ok(CallToolResult::json(&json!({
            "launched": true,
            "ipc_ready": null
        })));
    }

    // Poll IPC until responsive or timeout. A KiCad that exited will never
    // answer, so its exit ends the wait, even while a ping is in flight.
    // Re-resolve the endpoint on each poll so a server started before KiCad
    // observes the socket created by the process just launched (#32).
    // `kicad.watch(KICAD_IPC_POLL)` already paces this loop at 500 ms, so reset
    // the ping backoff per iteration rather than let it delay a late startup.
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(timeout_secs);
    loop {
        if let Err(message) = kicad.watch(KICAD_IPC_POLL).await {
            return Ok(exited(message));
        }
        ctx.reset_ipc_probe_backoff();
        let endpoint = ctx.ipc_endpoint();
        let ping = task::spawn_blocking(move || endpoint.client().ping_outcome());
        let outcome = tokio::select! {
            outcome = ping => outcome.ok(),
            message = kicad.exited() => return Ok(exited(message)),
        };

        if outcome
            .as_ref()
            .is_some_and(copperpour_ipc::PingOutcome::is_responsive)
        {
            return Ok(CallToolResult::json(&json!({
                "launched": true,
                "ipc_ready": true
            })));
        }
        if std::time::Instant::now() >= deadline {
            // KiCad has been up for the whole timeout, so a modal dialog is
            // the likelier reason it is not ready.
            let note = if outcome
                .as_ref()
                .is_some_and(copperpour_ipc::PingOutcome::kicad_not_ready)
            {
                crate::tools::NOT_READY_NOTE
            } else {
                "KiCAD launched but IPC not yet responsive within timeout"
            };
            return Ok(CallToolResult::json(&json!({
                "launched": true,
                "ipc_ready": false,
                "note": note
            })));
        }
    }
}

/// What one KiCad endpoint said about the board `open_board` asked for.
struct EndpointSeen {
    address: String,
    /// The board is among its open documents, and a read of it succeeded.
    holds_board: bool,
    /// The board is among its open documents.
    listed: bool,
    /// KiCad answered the Ping with `AS_OK`.
    responsive: bool,
    /// KiCad answered the Ping with `AS_NOT_READY`.
    not_ready: bool,
    /// The look ended without KiCad's own answer: a request that timed out,
    /// a reply that did not parse, or a status such as `AS_NOT_READY` to the
    /// Ping.
    unsure: bool,
    /// The look ended before the deadline.
    answered: bool,
    error: Option<String>,
}

impl EndpointSeen {
    fn pending(address: String) -> Self {
        EndpointSeen {
            address,
            holds_board: false,
            listed: false,
            responsive: false,
            not_ready: false,
            unsure: false,
            answered: false,
            error: Some("no answer yet".to_string()),
        }
    }

    /// It may hold the board without having said so: a dialog or a load, a
    /// request unanswered or still running, or the board listed but no read
    /// served, as while KiCad refills zones.
    fn undecided(&self) -> bool {
        self.unsure || !self.answered || (self.listed && !self.holds_board)
    }

    fn report(&self) -> Value {
        json!({
            "ipc_address": copperpour_ipc::redact_endpoint(&self.address),
            "requested_open": self.holds_board,
            "error": self.error,
        })
    }
}

/// Ask the KiCad at `address` for `board` as a board-scoped call does: bind it
/// among the open documents, then read it. A Project Manager answers the Ping
/// and refuses `GetOpenDocuments` as `AS_UNHANDLED`.
fn look_for_board(board: &Path, address: &str) -> EndpointSeen {
    let client = copperpour_ipc::KiCadIpcClient::new(address.to_string());
    let ping = client.ping_outcome();
    // KiCad's own answer settles it: a status, or the documents it names. A
    // reply that timed out or did not parse does not.
    let explicit = |error: &anyhow::Error| {
        error
            .downcast_ref::<copperpour_ipc::BoardTargetError>()
            .is_some()
            || copperpour_ipc::ApiStatusError::from_error(error).is_some()
    };
    let (listed, unsure, found) = match &ping {
        copperpour_ipc::PingOutcome::Responsive => match client.find_open_board(board) {
            Ok(_) => (
                true,
                false,
                client.wait_for_board_ready(std::time::Duration::ZERO),
            ),
            Err(error) => (false, !explicit(&error), Err(error)),
        },
        failed => (
            false,
            matches!(failed, copperpour_ipc::PingOutcome::RequestFailed { .. }),
            Err(anyhow::anyhow!(
                "{}",
                failed.failure_message().unwrap_or_default()
            )),
        ),
    };
    let found = found.map_err(|error| format!("{error:#}"));
    EndpointSeen {
        address: address.to_string(),
        holds_board: found.is_ok(),
        listed,
        responsive: ping.is_responsive(),
        not_ready: ping.kicad_not_ready(),
        unsure,
        answered: true,
        error: found.err(),
    }
}

/// Look at every endpoint at once, so one that takes its whole reply timeout
/// holds up none of the others. Once one holds the board, the rest get a
/// poll's time more, to find a second holder; one with no answer by then, or
/// by `until`, is pending, and its look ends on its own.
async fn look_for_board_at(
    board: &Path,
    addresses: Vec<String>,
    until: std::time::Instant,
) -> Vec<EndpointSeen> {
    let mut looks = tokio::task::JoinSet::new();
    for (index, address) in addresses.iter().cloned().enumerate() {
        let board = board.to_path_buf();
        looks.spawn_blocking(move || (index, look_for_board(&board, &address)));
    }
    let mut seen: Vec<Option<EndpointSeen>> = addresses.iter().map(|_| None).collect();
    let mut until = tokio::time::Instant::from_std(until);
    loop {
        match tokio::time::timeout_at(until, looks.join_next()).await {
            Ok(Some(Ok((index, one)))) => {
                if one.holds_board {
                    until = until.min(tokio::time::Instant::now() + KICAD_IPC_POLL);
                }
                seen[index] = Some(one);
            }
            // A look that panicked leaves its endpoint pending.
            Ok(Some(Err(_))) => {}
            Ok(None) | Err(_) => break,
        }
    }
    looks.detach_all();
    addresses
        .into_iter()
        .zip(seen)
        .map(|(address, one)| one.unwrap_or_else(|| EndpointSeen::pending(address)))
        .collect()
}

/// Where `open_board` looks for the board.
struct Looking {
    endpoints: Box<dyn Fn() -> Vec<String> + Send + Sync>,
    /// Why a PCB editor started now could not be found there, if it could not.
    unreachable_editor: Option<String>,
}

impl Looking {
    /// Every candidate detection chooses among, or the configured endpoint
    /// alone. KiCad binds its socket in its own temporary directory, so an
    /// editor started now can serve a configured endpoint only if it is that.
    fn for_context(ctx: &ToolContext) -> Self {
        if ctx.rediscovers_ipc() {
            return Looking {
                endpoints: Box::new(copperpour_ipc::candidate_endpoints),
                unreachable_editor: None,
            };
        }
        let configured = ctx.config.ipc_address.trim().to_string();
        let defaults = copperpour_ipc::candidate_socket_paths();
        let kicad_binds = configured.strip_prefix("ipc://").is_some_and(|path| {
            defaults
                .iter()
                .any(|default| same_socket_path(Path::new(path), default))
        });
        let unreachable_editor = if configured.is_empty() {
            Some("no KiCad endpoint is configured".to_string())
        } else {
            (!kicad_binds).then(|| {
                format!(
                    "the configured KiCad endpoint {} is not the socket KiCad binds ({}), so \
                     a PCB editor started now could not be found there",
                    copperpour_ipc::redact_endpoint(&configured),
                    defaults
                        .iter()
                        .map(|path| format!("ipc://{}", path.display()))
                        .collect::<Vec<_>>()
                        .join(" or ")
                )
            })
        };
        Looking {
            endpoints: Box::new(move || {
                Some(configured.clone())
                    .filter(|address| !address.is_empty())
                    .into_iter()
                    .collect()
            }),
            unreachable_editor,
        }
    }

    /// Each endpoint gets until `deadline`, and a look begun near or after it
    /// one poll's time, so its answer is still seen.
    async fn look(&self, board: &Path, deadline: std::time::Instant) -> Vec<EndpointSeen> {
        let until = deadline.max(std::time::Instant::now() + KICAD_IPC_POLL);
        look_for_board_at(board, (self.endpoints)(), until).await
    }
}

fn same_socket_path(a: &Path, b: &Path) -> bool {
    same_socket_path_for(a, b, cfg!(windows))
}

fn same_socket_path_for(a: &Path, b: &Path, case_insensitive: bool) -> bool {
    if !case_insensitive {
        return a == b;
    }
    let normalize = |path: &Path| {
        let s = path.as_os_str().to_string_lossy().replace('/', "\\");
        let mut parts: Vec<String> = Vec::new();
        for part in s.split('\\') {
            if part.is_empty() && !parts.is_empty() {
                continue;
            }
            parts.push(part.to_ascii_lowercase());
        }
        parts
    };
    normalize(a) == normalize(b)
}

/// What `open_board` reports about itself, whatever the outcome.
struct OpenBoardCall<'a> {
    board: &'a Path,
    shown: &'a str,
    timeout_seconds: u64,
    started: std::time::Instant,
}

impl OpenBoardCall<'_> {
    /// The answer once exactly one endpoint holds the board: recorded as a
    /// live observation and used by the calls that follow.
    fn found(
        &self,
        ctx: &ToolContext,
        seen: &[EndpointSeen],
        launched: bool,
    ) -> Option<CallToolResult> {
        let holders: Vec<&EndpointSeen> = seen.iter().filter(|e| e.holds_board).collect();
        match holders.as_slice() {
            [] => None,
            [holder] => {
                ctx.board_session.observe_live(self.board);
                ctx.select_ipc_endpoint(holder.address.clone());
                Some(CallToolResult::json(&json!({
                    "requested_board": self.shown,
                    "requested_open": true,
                    "launched": launched,
                    "ipc_address": copperpour_ipc::redact_endpoint(&holder.address),
                    "timed_out": false,
                    "timeout_seconds": self.timeout_seconds,
                    "elapsed_ms": self.started.elapsed().as_millis() as u64,
                    "endpoints": seen.iter().map(EndpointSeen::report).collect::<Vec<_>>(),
                })))
            }
            several => {
                let candidates: Vec<String> = several
                    .iter()
                    .map(|e| copperpour_ipc::redact_endpoint(&e.address))
                    .collect();
                Some(CallToolResult::error_kind(
                    ToolErrorKind::AmbiguousTarget {
                        target: self.shown.to_string(),
                        candidates: candidates.clone(),
                    },
                    format!(
                        "{} KiCad endpoints hold {}: {}. None was chosen; close the board in \
                         all but one and call open_board again.",
                        several.len(),
                        self.shown,
                        candidates.join(", ")
                    ),
                ))
            }
        }
    }

    /// The refusal while KiCad's lock beside the board is present, or could
    /// not be checked, and no endpoint in `seen` reports the board.
    fn locked(
        &self,
        seen: &[EndpointSeen],
        observed: super::live_board::EditorLock,
    ) -> Option<CallToolResult> {
        let endpoints = seen
            .iter()
            .map(|e| copperpour_ipc::redact_endpoint(&e.address))
            .collect::<Vec<_>>()
            .join(", ");
        let shown = self.shown;
        let (lock, message) = match observed {
            super::live_board::EditorLock::Absent => return None,
            super::live_board::EditorLock::Present(lock) => {
                let lock = copperpour_sexp::paths::display_path(&lock);
                let message = format!(
                    "KiCad's lock {lock} says {shown} is open, and no endpoint looked at reports \
                     it ({endpoints}). No PCB editor was started: a second one would stop at \
                     KiCad's \"already open\" dialog. If no KiCad has the board open, the lock \
                     is left from one that did not close it; remove it and call open_board again."
                );
                (lock, message)
            }
            super::live_board::EditorLock::Unreadable(lock, error) => {
                let lock = copperpour_sexp::paths::display_path(&lock);
                let message = format!(
                    "KiCad's lock {lock} could not be checked ({error}), and no endpoint looked \
                     at reports {shown} ({endpoints}). No PCB editor was started: a KiCad \
                     holding the board cannot be ruled out, and a second one would stop at \
                     KiCad's \"already open\" dialog. Make the lock's path checkable, then call \
                     open_board again."
                );
                (lock, message)
            }
        };
        Some(CallToolResult::error_kind(
            ToolErrorKind::Conflict { paths: vec![lock] },
            message,
        ))
    }

    /// The answer when no endpoint reported the board by the deadline.
    fn timed_out(&self, seen: &[EndpointSeen], launched: bool, note: &str) -> CallToolResult {
        CallToolResult::json(&json!({
            "requested_board": self.shown,
            "requested_open": false,
            "launched": launched,
            "ipc_address": null,
            "timed_out": true,
            "timeout_seconds": self.timeout_seconds,
            "elapsed_ms": self.started.elapsed().as_millis() as u64,
            "endpoints": seen.iter().map(EndpointSeen::report).collect::<Vec<_>>(),
            "note": note
        }))
    }
}

async fn handle_open_board(
    args: &serde_json::Value,
    ctx: &ToolContext,
) -> anyhow::Result<CallToolResult> {
    open_board(args, ctx, Looking::for_context(ctx)).await
}

async fn open_board(
    args: &serde_json::Value,
    ctx: &ToolContext,
    looking: Looking,
) -> anyhow::Result<CallToolResult> {
    let requested = match require_str(args, "board") {
        Ok(board) => PathBuf::from(board),
        Err(refusal) => return Ok(refusal),
    };
    if !requested
        .extension()
        .is_some_and(|ext| ext.eq_ignore_ascii_case("kicad_pcb"))
    {
        return Ok(invalid_arg("board", "must be a .kicad_pcb file"));
    }
    let board = match std::fs::canonicalize(&requested) {
        Ok(board) if board.is_file() => board,
        Ok(_) => return Ok(invalid_arg("board", "is not a file")),
        Err(_) => {
            return Ok(CallToolResult::error_kind(
                ToolErrorKind::file_not_found(&requested),
                format!("No board at {}.", requested.display()),
            ))
        }
    };
    let timeout_seconds = match timeout_seconds(args, 30) {
        Ok(timeout) => timeout,
        Err(refusal) => return Ok(refusal),
    };
    let display = match launch_display(args) {
        Ok(display) => display,
        Err(refusal) => return Ok(refusal),
    };
    let shown = copperpour_sexp::paths::display_path(&board);
    let call = OpenBoardCall {
        board: &board,
        shown: &shown,
        timeout_seconds,
        started: std::time::Instant::now(),
    };
    let deadline = call.started + std::time::Duration::from_secs(timeout_seconds);
    // The wait for another call counts against this one's timeout too.
    let Ok(_one_at_a_time) = tokio::time::timeout_at(
        tokio::time::Instant::from_std(deadline),
        ctx.opening_board.lock(),
    )
    .await
    else {
        return Ok(call.timed_out(
            &[],
            false,
            "Another open_board call was still running at timeout_seconds.",
        ));
    };

    // An endpoint that has not said may hold the board, and an editor started
    // on it now would stop at KiCad's "already open" dialog: wait for it.
    let mut seen = looking.look(&board, deadline).await;
    loop {
        if let Some(answer) = call.found(ctx, &seen, false) {
            return Ok(answer);
        }
        if !seen.iter().any(EndpointSeen::undecided) {
            break;
        }
        if std::time::Instant::now() >= deadline {
            let note = if seen.iter().any(|e| e.not_ready) {
                crate::tools::NOT_READY_NOTE
            } else if seen.iter().any(|e| e.listed) {
                "An endpoint lists the board but served no read of it within \
                 timeout_seconds, as while KiCad refills zones, so no PCB editor was started."
            } else {
                "An endpoint gave no clear answer within timeout_seconds, so no PCB editor \
                 was started: a busy KiCad may hold the board."
            };
            return Ok(call.timed_out(&seen, false, note));
        }
        tokio::time::sleep(KICAD_IPC_POLL).await;
        seen = looking.look(&board, deadline).await;
    }
    if let Some(refusal) = call.locked(&seen, super::live_board::editor_lock(&board)) {
        return Ok(refusal);
    }
    // A KiCad answering at a configured endpoint keeps it, so an editor
    // started now would serve another, which the calls never use.
    if !ctx.rediscovers_ipc() {
        if let Some(serving) = seen.iter().find(|e| e.responsive) {
            let endpoint = copperpour_ipc::redact_endpoint(&serving.address);
            return Ok(CallToolResult::error_kind(
                ToolErrorKind::EditorUnavailable {
                    editor: "pcb".to_string(),
                    reason: format!(
                        "the configured KiCad endpoint {endpoint} answers without {shown} open"
                    ),
                },
                format!(
                    "The configured KiCad endpoint {endpoint} answers without {shown} open, and \
                     a PCB editor started now would serve a socket of its own, which a \
                     configured ipc_address or KICAD_API_SOCKET is never replaced by. Open the \
                     board in the KiCad at that endpoint, then call open_board again."
                ),
            ));
        }
    }
    if let Some(reason) = &looking.unreachable_editor {
        return Ok(CallToolResult::error_kind(
            ToolErrorKind::EditorUnavailable {
                editor: "pcb".to_string(),
                reason: reason.clone(),
            },
            format!(
                "No PCB editor was started: {reason}. Open the board in the KiCad that serves \
                 it, or leave ipc_address and KICAD_API_SOCKET unset so the endpoint is \
                 detected, then call open_board again."
            ),
        ));
    }

    // `kicad -f pcb` would be the launcher's own road, but KiCad 10.0.8 exits
    // on a segmentation fault there: its PCB frame registers an API handler
    // before the launcher creates the API server (kicad/kicad.cpp:274, :298).
    let binary = crate::kicad_install::pcb_editor_or_default(
        &ctx.config.kicad_binary,
        &ctx.config.kicad_cli,
    );
    let mut cmd = std::process::Command::new(&binary);
    // The resolved path, as the lock checks resolve it; on Windows its display
    // form, without the `\\?\` prefix `canonicalize` adds.
    if cfg!(windows) {
        cmd.arg(&shown);
    } else {
        cmd.arg(&board);
    }
    if let Some(display) = &display {
        cmd.env("DISPLAY", display)
            .env("GDK_BACKEND", "x11")
            .env_remove("WAYLAND_DISPLAY");
    }
    let mut editor = match copperpour_viewer::launch::spawn(
        cmd,
        "KiCad's PCB editor",
        |line| tracing::warn!(target: "kicad", "{line}"),
    ) {
        Ok(editor) => editor,
        Err(e) => {
            return Ok(CallToolResult::error(format!(
                "Failed to launch KiCad's PCB editor ({binary}): {e}"
            )))
        }
    };
    editor.on_no_display(
        "Pass display (for example \":0\") to open the editor on the desktop's X display.",
    );

    loop {
        let remaining = deadline.saturating_duration_since(std::time::Instant::now());
        if let Err(message) = editor.watch(KICAD_IPC_POLL.min(remaining)).await {
            return Ok(CallToolResult::error(message));
        }
        let seen = tokio::select! {
            seen = looking.look(&board, deadline) => seen,
            message = editor.exited() => return Ok(CallToolResult::error(message)),
        };
        if let Some(answer) = call.found(ctx, &seen, true) {
            return Ok(answer);
        }
        if std::time::Instant::now() >= deadline {
            let note = if seen.iter().any(|e| e.not_ready) {
                crate::tools::NOT_READY_NOTE
            } else if cfg!(windows) && seen.iter().any(|e| e.responsive && !e.holds_board) {
                "No KiCad endpoint reported the board within timeout_seconds, and the PCB \
                 editor started for it is still running. On Windows, KiCad 10 does not bind \
                 a fallback api-<pid>.sock pipe when another KiCad (such as the Project \
                 Manager) already holds api.sock. Open the board from the running KiCad \
                 Project Manager, or close the other KiCad instance and call open_board again."
            } else {
                "No KiCad endpoint reported the board within timeout_seconds, and the PCB \
                 editor started for it is still running. Check that KiCad's IPC API server \
                 (api.enable_server in kicad_common.json) is enabled and no dialog is waiting; \
                 beside another KiCad it serves its own api-<pid>.sock."
            };
            return Ok(call.timed_out(&seen, true, note));
        }
    }
}

// ─── Copy routing pattern ─────────────────────────────────────────────────────

async fn handle_copy_routing_pattern(
    args: &serde_json::Value,
    ctx: &ToolContext,
) -> anyhow::Result<CallToolResult> {
    let board = get_path(args, "board")?;
    // All six are schema-required and each defaulted to 0.0. Omitting them all
    // was harmless — the source box collapsed to a point and matched nothing —
    // but a *partial* omission was not: drop only `dest_x`/`dest_y` and the
    // whole source region is duplicated onto the board origin and written to
    // the .kicad_pcb, reported as `{"copied": N}` (#218).
    let mut coords = [0.0f64; 6];
    for (slot, key) in coords
        .iter_mut()
        .zip(["src_x1", "src_y1", "src_x2", "src_y2", "dest_x", "dest_y"])
    {
        match require_f64(args, key) {
            Ok(v) => *slot = v,
            Err(e) => return Ok(e),
        }
    }
    let [src_x1, src_y1, src_x2, src_y2, dest_x, dest_y] = coords;
    // An inverted box used to select nothing and report success.
    for (field, low, high) in [("src_x2", src_x1, src_x2), ("src_y2", src_y1, src_y2)] {
        if high < low {
            return Ok(invalid_arg(
                field,
                &format!("{high} is less than the region's minimum {low}"),
            ));
        }
    }

    let mut net_map = HashMap::new();
    if let Some(obj) = args["net_map"].as_object() {
        for (from, to) in obj {
            let Some(to) = to.as_str() else {
                return Ok(invalid_arg(
                    &format!("net_map.{from}"),
                    "must be a destination net name",
                ));
            };
            net_map.insert(from.clone(), to.to_string());
        }
    }

    let copy = RoutingCopy {
        region: [src_x1, src_y1, src_x2, src_y2],
        dx: round6(dest_x - src_x1),
        dy: round6(dest_y - src_y1),
        net_map,
    };
    if copy.dx == 0.0 && copy.dy == 0.0 {
        return Ok(invalid_arg(
            "dest_x",
            "the destination is the source anchor (src_x1, src_y1), so every copy would land \
             on its original",
        ));
    }
    // A copy written to the file while KiCad holds the board would be
    // discarded by its next save. Asked before the read: the file can be
    // older than what KiCad holds.
    if let Some(refusal) =
        crate::tools::pcb_board::refuse_if_board_open_in_kicad(ctx, &board, "routing copy").await?
    {
        return Ok(refusal);
    }
    task::spawn_blocking(move || {
        let content = read_consistent(&board)?;
        match plan_routing_copy(&content, &copy) {
            Ok(planned) => commit_routing_copy(&board, &content, &copy, &planned),
            Err(refusal) => Ok(refusal),
        }
    })
    .await?
}

/// What one `copy_routing_pattern` call asks for.
pub(crate) struct RoutingCopy {
    /// The source region `[x1, y1, x2, y2]`, inclusive, with `x1 ≤ x2` and
    /// `y1 ≤ y2`.
    pub(crate) region: [f64; 4],
    pub(crate) dx: f64,
    pub(crate) dy: f64,
    pub(crate) net_map: HashMap<String, String>,
}

/// The copies to insert, and what was left out.
pub(crate) struct PlannedCopy {
    /// The board with the copies inserted; `None` when nothing is copied.
    content: Option<String>,
    uuids: Vec<String>,
    /// Copies per entry of [`ROUTING_KINDS`].
    counts: [usize; 3],
    excluded_crossing: Vec<Value>,
}

const ROUTING_KINDS: [&str; 3] = ["segment", "arc", "via"];

/// The points that place a routing item, as KiCad writes it.
fn defining_points(kind: &str) -> &'static [&'static str] {
    match kind {
        "segment" => &["start", "end"],
        "arc" => &["start", "mid", "end"],
        _ => &["at"],
    }
}

/// An item's identity child: `uuid` since KiCad 8, `tstamp` before it.
fn identity_of(node: &SexpNode) -> Option<(&'static str, &str)> {
    ["uuid", "tstamp"]
        .into_iter()
        .find_map(|tag| Some((tag, node.find_str(tag)?)))
}

fn unreadable(reason: String) -> CallToolResult {
    CallToolResult::error(format!(
        "Refusing to copy routing: {reason}. The board file was not modified."
    ))
}

/// Plan the copy of every top-level `segment`, `arc` and `via` whose defining
/// points all lie in the region. An item with only some of them inside is
/// left out and named in `excluded_crossing`, so a copy is never a silent
/// subset of a track. An item whose points cannot be read refuses the whole
/// copy, wherever it is: without them, whether it is in the region is unknown.
pub(crate) fn plan_routing_copy(
    content: &str,
    copy: &RoutingCopy,
) -> Result<PlannedCopy, CallToolResult> {
    crate::tools::pcb_components::check_single_board_form(content).map_err(unreadable)?;
    if !copy.net_map.is_empty() {
        // A destination net with no pads is a typo, not a net: KiCad would
        // create it and DRC would report the copy unconnected.
        let tree = parse_sexp(content)
            .map_err(|e| unreadable(format!("the board does not parse: {e}")))?;
        if !names_nets_in_place(&tree) {
            return Err(invalid_arg(
                "net_map",
                "this board references nets by number, as boards before KiCad 10 do, and \
                 net_map renames by name; resave the board in KiCad 10 or omit net_map",
            ));
        }
        let nets = collect_net_keys(&tree);
        let mut map: Vec<_> = copy.net_map.iter().collect();
        map.sort();
        if let Some((from, to)) = map.into_iter().find(|(_, to)| !nets.contains(*to)) {
            return Err(invalid_arg(
                &format!("net_map.{from}"),
                &format!("net '{to}' is not on this board"),
            ));
        }
    }
    let [x1, y1, x2, y2] = copy.region;
    let inside = |&(x, y): &(f64, f64)| x >= x1 && x <= x2 && y >= y1 && y <= y2;
    let eol = if content.contains("\r\n") {
        "\r\n"
    } else {
        "\n"
    };

    let mut insertion = String::new();
    let mut insert_at = content.len();
    let mut uuids = Vec::new();
    let mut counts = [0usize; 3];
    let mut excluded_crossing = Vec::new();
    for (start, end) in find_direct_child_blocks(content, "kicad_pcb") {
        let block = &content[start..end];
        let Some(kind_index) =
            block_tag(block).and_then(|tag| ROUTING_KINDS.iter().position(|k| *k == tag))
        else {
            continue;
        };
        let kind = ROUTING_KINDS[kind_index];
        // Copies go after the last routing item, where KiCad keeps them.
        insert_at = end;
        let node =
            parse_sexp(block).map_err(|e| unreadable(format!("a {kind} does not parse: {e}")))?;
        let (identity_tag, id) =
            identity_of(&node).ok_or_else(|| unreadable(format!("a {kind} has no uuid")))?;
        let points = defining_points(kind);
        let xys: Vec<(f64, f64)> = points
            .iter()
            .map(|p| match node.find_all(p).as_slice() {
                [one] => exact_coordinate_pair(one),
                _ => None,
            })
            .collect::<Option<_>>()
            .ok_or_else(|| {
                unreadable(format!(
                    "{kind} {id} does not have exactly one readable ({}) point each",
                    points.join(", ")
                ))
            })?;
        if !xys.iter().any(inside) {
            continue;
        }
        if !xys.iter().all(inside) {
            excluded_crossing.push(json!({ "kind": kind, "uuid": id }));
            continue;
        }

        let new_uuid = new_uuid();
        let mut edits = Vec::new();
        for (child_start, child_end) in find_direct_child_blocks(block, kind) {
            let child = &block[child_start..child_end];
            let replacement = match block_tag(child) {
                Some(tag) if points.contains(&tag) => {
                    let (x, y) = xys[points.iter().position(|p| *p == tag).expect("guarded")];
                    format!("({tag} {} {})", round6(x + copy.dx), round6(y + copy.dy))
                }
                Some(tag) if tag == identity_tag => format!("({tag} \"{new_uuid}\")"),
                Some("net") => match node
                    .find("net")
                    .and_then(net_name)
                    .and_then(|name| copy.net_map.get(name))
                {
                    Some(to) => format!("(net \"{}\")", escape_sexp_string(to)),
                    None => continue,
                },
                _ => continue,
            };
            edits.push(SexpEdit::replace(child_start, child_end, replacement));
        }
        let line_start = content[..start].rfind('\n').map_or(0, |i| i + 1);
        let indent: String = content[line_start..start]
            .chars()
            .take_while(|c| c.is_whitespace())
            .collect();
        insertion.push_str(eol);
        insertion.push_str(&indent);
        insertion.push_str(&apply_edits(block.to_string(), edits));
        uuids.push(new_uuid);
        counts[kind_index] += 1;
    }

    let content = (!uuids.is_empty()).then(|| {
        apply_edits(
            content.to_string(),
            vec![SexpEdit::insert(insert_at, insertion)],
        )
    });
    Ok(PlannedCopy {
        content,
        uuids,
        counts,
        excluded_crossing,
    })
}

/// Write the planned copies if the board still holds `expected`.
pub(crate) fn commit_routing_copy(
    board: &Path,
    expected: &str,
    copy: &RoutingCopy,
    planned: &PlannedCopy,
) -> anyhow::Result<CallToolResult> {
    let reply = |counts: [usize; 3]| {
        let by_kind: serde_json::Map<String, Value> = ROUTING_KINDS
            .iter()
            .zip(counts)
            .map(|(kind, n)| (kind.to_string(), json!(n)))
            .collect();
        json!({
            "copied": counts.iter().sum::<usize>(),
            "copied_by_kind": by_kind,
            "dx": copy.dx,
            "dy": copy.dy,
            "uuids": planned.uuids,
            "excluded_crossing": planned.excluded_crossing,
        })
    };
    let Some(next) = &planned.content else {
        let mut reply = reply([0; 3]);
        reply["note"] = json!(
            "No routing item lies wholly inside the source region; the board was not modified"
        );
        return Ok(CallToolResult::json(&reply));
    };

    // Under the lock, this rereads the file after the rename and compares it
    // with `next` byte for byte, so the planned counts are what the board now
    // holds.
    match write_atomic_if_unchanged(board, expected, next) {
        Ok(()) => {}
        Err(SexpError::Conflict { .. }) => {
            return Ok(CallToolResult::error_kind(
                ToolErrorKind::Conflict {
                    paths: vec![board.display().to_string()],
                },
                "No routing was copied because the board changed after it was read. Read it \
                 again and retry.",
            ))
        }
        Err(error) => return Err(error.into()),
    }

    let mut reply = reply(planned.counts);
    reply["warning"] = json!(crate::tools::pcb_board::FILE_ONLY_EDIT_WARNING);
    Ok(CallToolResult::json(&reply))
}
// ─── Symbol info ──────────────────────────────────────────────────────────────

// ─── Layer constraints ───────────────────────────────────────────────────────

async fn handle_set_layer_constraints(
    args: &serde_json::Value,
    _ctx: &ToolContext,
) -> anyhow::Result<CallToolResult> {
    let board = get_path(args, "board")?;
    let layer = match require_str(args, "layer") {
        Ok(v) => v.to_string(),
        Err(e) => return Ok(e),
    };
    anyhow::ensure!(
        !layer.is_empty()
            && layer
                .chars()
                .all(|character| character.is_ascii_alphanumeric()
                    || character == '.'
                    || character == '_'),
        "Layer name contains unsupported characters"
    );
    let rules_path = sibling_custom_rules_path(&board);
    // `None` when there is no rules file yet: the write must then create one,
    // never replace a file that appeared in the meantime.
    let read = match tokio::fs::read_to_string(&rules_path).await {
        Ok(content) => Some(content),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
        Err(error) => return Err(error.into()),
    };
    let mut content = read.as_deref().unwrap_or("(version 1)\n").to_owned();
    let mut changed = Vec::new();

    if let Some(clearance) = args["min_clearance"].as_f64() {
        let rule_name = format!("konnect:{layer}:clearance");
        content = upsert_named_rule(
            &content,
            &rule_name,
            &layer_rule(&rule_name, "clearance", clearance, &layer),
        );
        changed.push(format!("clearance = {} on {}", clearance, layer));
    }

    if let Some(trace_width) = args["min_trace_width"].as_f64() {
        let rule_name = format!("konnect:{layer}:track_width");
        content = upsert_named_rule(
            &content,
            &rule_name,
            &layer_rule(&rule_name, "track_width", trace_width, &layer),
        );
        changed.push(format!("min_trace_width = {} on {}", trace_width, layer));
    }

    if !changed.is_empty() {
        let refusal = crate::tools::write_if_unchanged(
            &rules_path,
            "custom rules file",
            "set_layer_constraints",
            read.as_deref(),
            content,
        )?;
        if let Some(refusal) = refusal {
            return Ok(refusal);
        }
    }

    Ok(CallToolResult::text(
        serde_json::to_string(&json!({
            "success": true,
            "layer": layer,
            "rules_file": rules_path,
            "changed": changed
        }))
        .unwrap(),
    ))
}

// ─── Check clearance ─────────────────────────────────────────────────────────

async fn handle_check_clearance(
    args: &serde_json::Value,
    ctx: &ToolContext,
) -> anyhow::Result<CallToolResult> {
    let board = get_path(args, "board")?;
    let source_selector = match super::board_source::BoardSource::from_args(args) {
        Ok(source) => source,
        Err(refusal) => return Ok(refusal),
    };
    let ref1 = match require_str(args, "ref1") {
        Ok(v) => v.to_string(),
        Err(e) => return Ok(e),
    };
    let ref2 = match require_str(args, "ref2") {
        Ok(v) => v.to_string(),
        Err(e) => return Ok(e),
    };
    let mode = opt_str(args, "mode").unwrap_or("anchor");
    if !matches!(mode, "anchor" | "courtyard") {
        return Ok(CallToolResult::error(format!(
            "Invalid mode '{mode}'; expected 'anchor' or 'courtyard'"
        )));
    }

    // Prefer the exact unsaved board KiCad owns. Falling back only when the
    // requested board is not open gives both modes the same live/file
    // semantics and prevents a plausible answer from stale placement data.
    let super::placement::ObservedBoardSnapshot {
        content,
        legacy_source: source,
        domain_source,
        evidence,
    } = match super::placement::live_or_saved_board(
        ctx,
        &board,
        source_selector,
        "clearance measurement",
    )
    .await?
    {
        Ok(read) => read,
        Err(result) => return Ok(result),
    };
    let tree = copperpour_sexp::parser::parse_sexp(&content)?;

    if mode == "courtyard" {
        return courtyard_clearance_result(&tree, &ref1, &ref2, source, domain_source, evidence);
    }

    let pos1 = find_footprint_position(&tree, &ref1)?;
    let pos2 = find_footprint_position(&tree, &ref2)?;

    let dx = pos2.0 - pos1.0;
    let dy = pos2.1 - pos1.1;
    let distance = (dx * dx + dy * dy).sqrt();
    let anchor_distance_mm = (distance * 1000.0).round() / 1000.0;

    // The number has always been the distance between the two placement
    // anchors. Under a description that said "physical clearance" it was read
    // as copper-to-copper spacing and a 21 mm answer stood in for a 3 mm
    // courtyard gap (#410). Stage 1 names what the number is; `distance_mm`
    // stays as a deprecated alias so no consumer breaks before stage 2 adds
    // real geometry.
    let mut body = json!({
        "ref1": ref1,
        "ref2": ref2,
        "mode": "anchor",
        "source": source,
        "pos1": { "x": pos1.0, "y": pos1.1 },
        "pos2": { "x": pos2.0, "y": pos2.1 },
        "measurement": MEASUREMENT_ANCHOR_TO_ANCHOR,
        "anchor_distance_mm": anchor_distance_mm,
        "distance_mm": anchor_distance_mm,
        // A collection takes a self-describing plural noun
        // (docs/NAMING_CONVENTIONS.md): what it holds is field names.
        "deprecated_fields": ["distance_mm"],
        "note": "anchor-to-anchor distance only; not pad, trace or courtyard clearance — \
                 footprint size and shape are not considered. Use run_drc for clearance."
    });
    super::board_source::provenance(&mut body, json!({ "clearance": domain_source }), evidence);
    Ok(CallToolResult::json(&body))
}

/// Compatibility measurement retained by the default `anchor` mode. The
/// courtyard mode has its own explicitly named result; this value never
/// changes meaning.
const MEASUREMENT_ANCHOR_TO_ANCHOR: &str = "anchor_to_anchor";
const MEASUREMENT_COURTYARD_BBOX_EDGE: &str = "courtyard_bbox_edge_to_edge";

fn footprint_reference(fp: &copperpour_sexp::parser::SexpNode) -> Option<&str> {
    fp.find_all("property")
        .iter()
        .find_map(|property| {
            (property.get(1).and_then(|n| n.as_str()) == Some("Reference"))
                .then(|| property.get(2).and_then(|n| n.as_str()))
                .flatten()
        })
        .or_else(|| {
            fp.find_all("fp_text").iter().find_map(|text| {
                (text.get(1).and_then(|n| n.as_str()) == Some("reference"))
                    .then(|| text.get(2).and_then(|n| n.as_str()))
                    .flatten()
            })
        })
}

fn authored_courtyard(
    tree: &copperpour_sexp::parser::SexpNode,
    reference: &str,
) -> anyhow::Result<Result<copperpour_sexp::board::FootprintCourtyard, &'static str>> {
    let footprint_count = tree
        .find_all("footprint")
        .into_iter()
        .filter(|fp| footprint_reference(fp) == Some(reference))
        .count();
    match footprint_count {
        0 => anyhow::bail!("Footprint '{}' not found on board", reference),
        1 => {}
        count => anyhow::bail!(
            "Footprint reference '{}' is ambiguous: found {} instances",
            reference,
            count
        ),
    }

    let scan = copperpour_sexp::board::footprint_courtyards(tree);
    let Some(courtyard) = scan
        .items
        .iter()
        .find(|item| item.reference.as_deref() == Some(reference))
    else {
        return Ok(Err("courtyard_geometry_unreadable"));
    };
    if courtyard.bbox_source != copperpour_sexp::board::CourtyardSource::Courtyard {
        return Ok(Err("authored_courtyard_missing"));
    }
    Ok(Ok(courtyard.clone()))
}

fn side_name(side: copperpour_sexp::board::Side) -> &'static str {
    match side {
        copperpour_sexp::board::Side::Front => "front",
        copperpour_sexp::board::Side::Back => "back",
    }
}

fn bbox_json(bbox: (f64, f64, f64, f64)) -> serde_json::Value {
    json!({
        "min_x": bbox.0,
        "min_y": bbox.1,
        "max_x": bbox.2,
        "max_y": bbox.3
    })
}

fn courtyard_bbox_spacing(a: (f64, f64, f64, f64), b: (f64, f64, f64, f64)) -> (f64, bool) {
    let dx = (a.0 - b.2).max(b.0 - a.2).max(0.0);
    let dy = (a.1 - b.3).max(b.1 - a.3).max(0.0);
    let clearance = ((dx * dx + dy * dy).sqrt() * 1000.0).round() / 1000.0;
    let overlaps = a.0 < b.2 && b.0 < a.2 && a.1 < b.3 && b.1 < a.3;
    (clearance, overlaps)
}

fn courtyard_clearance_result(
    tree: &copperpour_sexp::parser::SexpNode,
    ref1: &str,
    ref2: &str,
    source: &str,
    domain_source: &str,
    evidence: serde_json::Value,
) -> anyhow::Result<CallToolResult> {
    let first = authored_courtyard(tree, ref1)?;
    let second = authored_courtyard(tree, ref2)?;
    if let Err(reason_code) = first {
        return Ok(courtyard_unavailable(
            ref1,
            ref2,
            source,
            domain_source,
            evidence,
            ref1,
            reason_code,
        ));
    }
    if let Err(reason_code) = second {
        return Ok(courtyard_unavailable(
            ref1,
            ref2,
            source,
            domain_source,
            evidence,
            ref2,
            reason_code,
        ));
    }
    let first = first.expect("checked above");
    let second = second.expect("checked above");
    let side1 = side_name(first.layer_side);
    let side2 = side_name(second.layer_side);
    if first.layer_side != second.layer_side {
        let mut body = json!({
            "ref1": ref1,
            "ref2": ref2,
            "mode": "courtyard",
            "source": source,
            "measurement": MEASUREMENT_COURTYARD_BBOX_EDGE,
            "available": false,
            "applicable": false,
            "courtyard_clearance_mm": serde_json::Value::Null,
            "overlaps": serde_json::Value::Null,
            "side1": side1,
            "side2": side2,
            "reason_code": "opposite_board_sides",
            "note": "Opposite-side courtyard spacing is not a same-side placement collision. Use run_drc for through-board and copper clearance."
        });
        super::board_source::provenance(&mut body, json!({ "clearance": domain_source }), evidence);
        return Ok(CallToolResult::json(&body));
    }

    let a = first.bbox;
    let b = second.bbox;
    let (clearance, overlaps) = courtyard_bbox_spacing(a, b);
    let mut body = json!({
        "ref1": ref1,
        "ref2": ref2,
        "mode": "courtyard",
        "source": source,
        "measurement": MEASUREMENT_COURTYARD_BBOX_EDGE,
        "available": true,
        "applicable": true,
        "courtyard_clearance_mm": clearance,
        "overlaps": overlaps,
        "side1": side1,
        "side2": side2,
        "geometry1": {
            "source": "authored_courtyard_bbox",
            "bbox": bbox_json(a),
            "rotation_deg": first.rotation_deg
        },
        "geometry2": {
            "source": "authored_courtyard_bbox",
            "bbox": bbox_json(b),
            "rotation_deg": second.rotation_deg
        },
        "note": "Axis-aligned board-space hull distance between authored courtyards; zero with overlaps=true means the hulls overlap. This is not copper clearance; use run_drc for electrical clearance."
    });
    super::board_source::provenance(&mut body, json!({ "clearance": domain_source }), evidence);
    Ok(CallToolResult::json(&body))
}

fn courtyard_unavailable(
    ref1: &str,
    ref2: &str,
    source: &str,
    domain_source: &str,
    evidence: serde_json::Value,
    unavailable_reference: &str,
    reason_code: &str,
) -> CallToolResult {
    let mut body = json!({
        "ref1": ref1,
        "ref2": ref2,
        "mode": "courtyard",
        "source": source,
        "measurement": MEASUREMENT_COURTYARD_BBOX_EDGE,
        "available": false,
        "applicable": serde_json::Value::Null,
        "courtyard_clearance_mm": serde_json::Value::Null,
        "overlaps": serde_json::Value::Null,
        "unavailable_reference": unavailable_reference,
        "reason_code": reason_code,
        "note": "An authored, fully readable courtyard is required for each footprint; pads and anchors are not substituted."
    });
    super::board_source::provenance(&mut body, json!({ "clearance": domain_source }), evidence);
    CallToolResult::json(&body)
}

/// Look up the board-space (x, y) position of a footprint by its reference designator.
fn find_footprint_position(
    tree: &copperpour_sexp::parser::SexpNode,
    reference: &str,
) -> anyhow::Result<(f64, f64)> {
    let matches: Vec<_> = tree
        .find_all("footprint")
        .into_iter()
        .filter(|fp| footprint_reference(fp) == Some(reference))
        .collect();
    let fp_node = match matches.as_slice() {
        [] => anyhow::bail!("Footprint '{}' not found on board", reference),
        [fp] => *fp,
        many => anyhow::bail!(
            "Footprint reference '{}' is ambiguous: found {} instances",
            reference,
            many.len()
        ),
    };

    let fp_at = fp_node
        .find("at")
        .ok_or_else(|| anyhow::anyhow!("Footprint '{}' has no placement anchor", reference))?;
    let fp_x = fp_at
        .get_f64(1)
        .filter(|value| value.is_finite())
        .ok_or_else(|| anyhow::anyhow!("Footprint '{}' has an invalid X anchor", reference))?;
    let fp_y = fp_at
        .get_f64(2)
        .filter(|value| value.is_finite())
        .ok_or_else(|| anyhow::anyhow!("Footprint '{}' has an invalid Y anchor", reference))?;

    Ok((fp_x, fp_y))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::router::ToolRouter;
    use crate::tools::ServerConfig;
    use std::sync::Arc;

    fn test_ctx() -> ToolContext {
        ToolContext::new(
            ServerConfig {
                kicad_cli: String::new(),
                kicad_binary: String::new(),
                ipc_address: String::new(),
                project_dir: None,
                jlcpcb_db_path: None,
                auto_load_toolsets: false,
                eager_toolsets: false,
            },
            Arc::new(ToolRouter::new()),
        )
    }

    #[test]
    fn health_timeout_is_bounded_and_typed() {
        assert_eq!(health_timeout_seconds(&json!({})).unwrap(), 5);
        assert_eq!(
            health_timeout_seconds(&json!({ "timeout_seconds": 17 })).unwrap(),
            17
        );
        for invalid in [json!(0), json!(301), json!(1.5), json!("5")] {
            let error = health_timeout_seconds(&json!({ "timeout_seconds": invalid }))
                .expect_err("out-of-range or non-integer timeout must be refused");
            assert!(error.is_error);
        }
    }

    #[test]
    fn standalone_editors_count_as_kicad_ui_processes() {
        for name in ["kicad", "pcbnew", "eeschema", "PCBNEW.EXE"] {
            assert!(is_kicad_process_name(name), "did not recognize {name}");
        }
        assert!(!is_kicad_process_name("kicad-cli"));
        assert!(!is_kicad_process_name("freerouting"));
        assert!(process_list_has_kicad(
            "/usr/bin/Finder\n/Applications/KiCad/pcbnew\n"
        ));
    }

    #[test]
    fn responsive_ipc_is_sufficient_running_evidence() {
        assert!(ui_running(false, true));
        assert!(ui_running(true, false));
        assert!(!ui_running(false, false));
    }

    #[tokio::test]
    async fn check_kicad_ui_names_why_ipc_did_not_answer() {
        let dir = tempfile::tempdir().unwrap();
        let mut ctx = test_ctx();
        ctx.config.ipc_address =
            format!("ipc://{}", dir.path().join("no-kicad-here.sock").display());

        let result = handle_check_kicad_ui(&json!({ "timeout_seconds": 60 }), &ctx)
            .await
            .unwrap();
        let text = match &result.content[0] {
            crate::mcp::protocol::ToolContent::Text { text } => text.clone(),
            _ => panic!("expected text content"),
        };
        let response: serde_json::Value = serde_json::from_str(&text).unwrap();

        assert_eq!(response["timed_out"], false, "{response}");
        assert_eq!(response["ipc_responsive"], false, "{response}");
        assert_eq!(response["ipc_failure"]["kind"], "no_listener", "{response}");
        assert!(
            response["ipc_failure"]["message"]
                .as_str()
                .is_some_and(|message| message.contains("Nothing is listening there")
                    && message.contains("api.enable_server")),
            "{response}"
        );
        assert!(response["ipc_failure"].get("note").is_none(), "{response}");
    }

    /// The first-run wizard holds KiCad at `AS_NOT_READY` for as long as it
    /// is open (#225).
    #[tokio::test]
    async fn check_kicad_ui_says_a_dialog_may_hold_a_kicad_that_is_not_ready() {
        let kicad = crate::test_support::MockIpcServer::not_ready("check-kicad-ui");
        let mut ctx = test_ctx();
        ctx.config.ipc_address = kicad.address().to_string();

        let result = handle_check_kicad_ui(&json!({ "timeout_seconds": 60 }), &ctx)
            .await
            .unwrap();
        let response: serde_json::Value = serde_json::from_str(&text_of(&result)).unwrap();

        assert_eq!(
            response["ipc_failure"]["kind"], "request_failed",
            "{response}"
        );
        assert!(
            response["ipc_failure"]["note"]
                .as_str()
                .is_some_and(|note| note.contains("modal dialog")),
            "{response}"
        );
    }

    #[tokio::test]
    async fn health_deadline_returns_without_waiting_for_the_inner_future() {
        let timed_out = bounded_health_check(std::time::Duration::from_millis(1), async {
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
            1
        })
        .await;
        assert!(timed_out.is_err());

        let completed = bounded_health_check(std::time::Duration::from_secs(1), async { 2 })
            .await
            .unwrap();
        assert_eq!(completed, 2);
    }

    fn blank_board() -> &'static str {
        "(kicad_pcb\n  (version 20250610)\n  (generator \"test\")\n  (general (thickness 1.6))\n  (paper \"A4\")\n  (layers\n    (0 \"F.Cu\" signal)\n    (31 \"B.Cu\" signal)\n    (44 \"Edge.Cuts\" user)\n  )\n  (setup (pad_to_mask_clearance 0))\n  (net 0 \"\")\n)\n"
    }

    fn blank_project() -> &'static str {
        "{\n  \"meta\": {\"filename\": \"board.kicad_pro\", \"version\": 1},\n  \"board\": {\"design_settings\": {}},\n  \"schematic\": {}\n}\n"
    }

    /// The readback names a requested rule the project does not hold, here
    /// KiCad's own file before any write.
    #[test]
    fn readback_names_a_rule_the_project_does_not_hold() {
        let project: Value = serde_json::from_str(include_str!(
            "../../../copperpour-sexp/tests/fixtures/placement/placement_fixture.kicad_pro"
        ))
        .unwrap();
        let rule = |arg| design_rules().iter().find(|rule| rule.arg == arg).unwrap();
        let held = (rule("min_text_height"), 0.8);
        let edge = (rule("min_copper_edge_clearance"), 0.2);
        assert_eq!(
            rules_not_holding(&project, &[held, edge]),
            ["min_copper_edge_clearance"]
        );
        assert!(rules_not_holding(&project, &[held]).is_empty());
        // A float one ULP off what the file holds is the same nm value.
        let ulp = (held.0, f64::from_bits(0.8f64.to_bits() + 1));
        assert!(rules_not_holding(&project, &[ulp]).is_empty());
    }

    /// The handler holds KiCad's ranges and the number type itself, for a call
    /// that skips dispatch.
    #[tokio::test]
    async fn set_design_rules_refuses_out_of_range_without_dispatch() {
        use crate::tools::design_rules_tests::{assert_untouched, backdate, RANGES};
        let dir = tempfile::tempdir().unwrap();
        let board = dir.path().join("board.kicad_pcb");
        let project = dir.path().join("board.kicad_pro");
        std::fs::write(&board, blank_board()).unwrap();
        std::fs::write(&project, blank_project()).unwrap();
        let bytes = std::fs::read(&project).unwrap();
        let mtime = backdate(&project);
        let ctx = test_ctx();
        for (field, min, max) in RANGES {
            for value in [json!(min - 0.01), json!(max + 0.01), json!("1.0")] {
                let result =
                    handle_set_design_rules(&json!({ "board": board, field: value }), &ctx)
                        .await
                        .unwrap();
                assert!(result.is_error, "{field} = {value} was accepted");
                let body: Value = serde_json::from_str(&text_of(&result)).unwrap();
                assert_eq!(body["error"]["field"], field);
                assert_untouched(&project, &bytes, mtime, &format!("{field} = {value}"));
            }
        }
    }

    #[tokio::test]
    async fn set_design_rules_updates_project_json_without_touching_board() {
        let dir = tempfile::tempdir().unwrap();
        let board = dir.path().join("board.kicad_pcb");
        let project = dir.path().join("board.kicad_pro");
        tokio::fs::write(&board, blank_board()).await.unwrap();
        tokio::fs::write(&project, blank_project()).await.unwrap();
        let original_board = tokio::fs::read(&board).await.unwrap();

        let args = json!({
            "board": board,
            "min_clearance": 0.25,
            "min_trace_width": 0.25,
            "min_via_drill": 0.30,
            "min_via_size": 0.70,
            "min_hole_to_hole": 0.45
        });
        let result = handle_set_design_rules(&args, &test_ctx()).await.unwrap();
        assert!(!result.is_error);

        assert_eq!(tokio::fs::read(&board).await.unwrap(), original_board);
        let project_json: serde_json::Value =
            serde_json::from_slice(&tokio::fs::read(&project).await.unwrap()).unwrap();
        let rules = &project_json["board"]["design_settings"]["rules"];
        assert_eq!(rules["min_clearance"], 0.25);
        assert_eq!(rules["min_track_width"], 0.25);
        assert_eq!(rules["min_through_hole_diameter"], 0.30);
        assert_eq!(rules["min_via_diameter"], 0.70);
        assert_eq!(rules["min_hole_to_hole"], 0.45);
    }

    fn text_of(result: &CallToolResult) -> String {
        match result.content.first() {
            Some(crate::mcp::protocol::ToolContent::Text { text }) => text.clone(),
            other => panic!("expected text, got {other:?}"),
        }
    }

    fn project_json(project: &std::path::Path) -> serde_json::Value {
        serde_json::from_slice(&std::fs::read(project).unwrap()).unwrap()
    }

    #[tokio::test]
    async fn set_predefined_sizes_writes_palette_and_leaves_the_board_alone() {
        let dir = tempfile::tempdir().unwrap();
        let board = dir.path().join("board.kicad_pcb");
        let project = dir.path().join("board.kicad_pro");
        tokio::fs::write(&board, blank_board()).await.unwrap();
        tokio::fs::write(&project, blank_project()).await.unwrap();
        let original_board = tokio::fs::read(&board).await.unwrap();

        let result = handle_set_predefined_sizes(
            &json!({
                "board": board,
                "track_widths": [0.2, 0.5, 0.8],
                "via_dimensions": [
                    { "diameter": 0.6, "drill": 0.3 },
                    { "diameter": 0.8, "drill": 0.4 }
                ]
            }),
            &test_ctx(),
        )
        .await
        .unwrap();
        assert!(!result.is_error, "{}", text_of(&result));
        assert_eq!(tokio::fs::read(&board).await.unwrap(), original_board);

        let stored = project_json(&project);
        assert_eq!(
            stored["board"]["design_settings"]["track_widths"],
            json!([0.0, 0.2, 0.5, 0.8])
        );
        assert_eq!(
            stored["board"]["design_settings"]["via_dimensions"],
            json!([
                { "diameter": 0.0, "drill": 0.0 },
                { "diameter": 0.6, "drill": 0.3 },
                { "diameter": 0.8, "drill": 0.4 }
            ])
        );
        assert_eq!(stored["meta"]["filename"], json!("board.kicad_pro"));
    }

    #[tokio::test]
    async fn get_predefined_sizes_reads_what_set_wrote() {
        let dir = tempfile::tempdir().unwrap();
        let board = dir.path().join("board.kicad_pcb");
        let project = dir.path().join("board.kicad_pro");
        tokio::fs::write(&board, blank_board()).await.unwrap();
        tokio::fs::write(&project, blank_project()).await.unwrap();
        handle_set_predefined_sizes(
            &json!({ "board": board, "track_widths": [0.2] }),
            &test_ctx(),
        )
        .await
        .unwrap();

        let result = handle_get_predefined_sizes(&json!({ "board": board }), &test_ctx())
            .await
            .unwrap();
        assert!(!result.is_error, "{}", text_of(&result));
        let body: serde_json::Value = serde_json::from_str(&text_of(&result)).unwrap();
        assert_eq!(body["track_widths"], json!([0.0, 0.2]));
        assert_eq!(
            body["via_dimensions"],
            json!([{ "diameter": 0.0, "drill": 0.0 }])
        );
    }

    #[tokio::test]
    async fn set_predefined_sizes_without_a_project_file_refuses_and_writes_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let board = dir.path().join("board.kicad_pcb");
        tokio::fs::write(&board, blank_board()).await.unwrap();
        let original_board = tokio::fs::read(&board).await.unwrap();

        let result = handle_set_predefined_sizes(
            &json!({ "board": board, "track_widths": [0.2] }),
            &test_ctx(),
        )
        .await
        .unwrap();
        assert!(result.is_error, "{}", text_of(&result));
        assert!(
            text_of(&result).contains("kicad_pro"),
            "{}",
            text_of(&result)
        );
        assert_eq!(tokio::fs::read(&board).await.unwrap(), original_board);
        assert!(!dir.path().join("board.kicad_pro").exists());
    }

    #[tokio::test]
    async fn set_predefined_sizes_refuses_a_via_with_no_annular_ring() {
        let dir = tempfile::tempdir().unwrap();
        let board = dir.path().join("board.kicad_pcb");
        let project = dir.path().join("board.kicad_pro");
        tokio::fs::write(&board, blank_board()).await.unwrap();
        tokio::fs::write(&project, blank_project()).await.unwrap();
        let original_project = tokio::fs::read(&project).await.unwrap();

        let result = handle_set_predefined_sizes(
            &json!({
                "board": board,
                "via_dimensions": [{ "diameter": 0.3, "drill": 0.3 }]
            }),
            &test_ctx(),
        )
        .await
        .unwrap();
        assert!(result.is_error, "{}", text_of(&result));
        assert!(
            text_of(&result).contains("greater than drill"),
            "{}",
            text_of(&result)
        );
        assert_eq!(tokio::fs::read(&project).await.unwrap(), original_project);
    }

    #[tokio::test]
    async fn set_predefined_sizes_omitting_vias_leaves_existing_vias_alone() {
        let dir = tempfile::tempdir().unwrap();
        let board = dir.path().join("board.kicad_pcb");
        let project = dir.path().join("board.kicad_pro");
        tokio::fs::write(&board, blank_board()).await.unwrap();
        tokio::fs::write(&project, blank_project()).await.unwrap();
        handle_set_predefined_sizes(
            &json!({
                "board": board,
                "track_widths": [0.2],
                "via_dimensions": [{ "diameter": 0.6, "drill": 0.3 }]
            }),
            &test_ctx(),
        )
        .await
        .unwrap();

        let result = handle_set_predefined_sizes(
            &json!({ "board": board, "track_widths": [0.2, 0.5] }),
            &test_ctx(),
        )
        .await
        .unwrap();
        assert!(!result.is_error, "{}", text_of(&result));
        let stored = project_json(&project);
        assert_eq!(
            stored["board"]["design_settings"]["track_widths"],
            json!([0.0, 0.2, 0.5])
        );
        assert_eq!(
            stored["board"]["design_settings"]["via_dimensions"],
            json!([
                { "diameter": 0.0, "drill": 0.0 },
                { "diameter": 0.6, "drill": 0.3 }
            ])
        );
    }

    #[tokio::test]
    async fn set_layer_constraints_writes_idempotent_custom_rules_file() {
        let dir = tempfile::tempdir().unwrap();
        let board = dir.path().join("board.kicad_pcb");
        tokio::fs::write(&board, blank_board()).await.unwrap();
        let original_board = tokio::fs::read(&board).await.unwrap();
        let args = json!({
            "board": board,
            "layer": "F.Cu",
            "min_clearance": 0.25,
            "min_trace_width": 0.25
        });

        for _ in 0..2 {
            let result = handle_set_layer_constraints(&args, &test_ctx())
                .await
                .unwrap();
            assert!(!result.is_error);
        }

        assert_eq!(tokio::fs::read(&board).await.unwrap(), original_board);
        let rules = tokio::fs::read_to_string(dir.path().join("board.kicad_dru"))
            .await
            .unwrap();
        assert!(rules.starts_with("(version 1)"));
        assert_eq!(rules.matches("(rule \"konnect:F.Cu:clearance\"").count(), 1);
        assert_eq!(
            rules.matches("(rule \"konnect:F.Cu:track_width\"").count(),
            1
        );
        assert!(rules.contains("(constraint clearance (min 0.25mm))"));
        assert!(rules.contains("(constraint track_width (min 0.25mm))"));
        assert!(rules.contains("(layer \"F.Cu\")"));
    }

    /// #410 stage 1: the number is the distance between two placement
    /// anchors, and the response now says so. Real KiCad-saved board
    /// (`specctra_two_resistors.kicad_pcb`, see its README).
    #[tokio::test]
    async fn check_clearance_names_its_measurement_and_keeps_the_alias() {
        let board = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/specctra_two_resistors.kicad_pcb");
        let result = handle_check_clearance(
            &json!({ "board": board.to_str().unwrap(), "ref1": "R1", "ref2": "R2" }),
            &test_ctx(),
        )
        .await
        .unwrap();
        assert!(!result.is_error, "{}", text_of(&result));
        let value: serde_json::Value = serde_json::from_str(&text_of(&result)).unwrap();

        assert_eq!(value["measurement"], MEASUREMENT_ANCHOR_TO_ANCHOR);
        assert_eq!(value["mode"], "anchor");
        assert_eq!(value["source"], "saved_file");
        // The alias is the same number, not a second measurement.
        assert_eq!(value["anchor_distance_mm"], value["distance_mm"]);
        assert_eq!(value["deprecated_fields"], json!(["distance_mm"]));
        assert!(value.get("deprecated").is_none());
        // And the number is exactly the anchor-to-anchor distance of the two
        // positions the response itself reports.
        let dx = value["pos2"]["x"].as_f64().unwrap() - value["pos1"]["x"].as_f64().unwrap();
        let dy = value["pos2"]["y"].as_f64().unwrap() - value["pos1"]["y"].as_f64().unwrap();
        let expected = ((dx * dx + dy * dy).sqrt() * 1000.0).round() / 1000.0;
        assert_eq!(value["anchor_distance_mm"].as_f64().unwrap(), expected);
        assert!(value["note"]
            .as_str()
            .unwrap()
            .contains("not pad, trace or courtyard clearance"));
    }

    fn sexp_fixture(name: &str) -> std::path::PathBuf {
        std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../copperpour-sexp/tests/fixtures")
            .join(name)
    }

    /// #410 stage 2: values are pinned independently in the copperpour-sexp
    /// fixture tests from KiCad-authored geometry. C1 and R1 are both rotated;
    /// their transformed courtyard hulls have a 4.455 mm vertical gap.
    /// Anchor distance is deliberately much larger, which is the negative
    /// control against restoring the original implementation.
    #[tokio::test]
    async fn check_clearance_measures_rotated_authored_courtyards_not_anchors() {
        let board = sexp_fixture("ecc83-pp.kicad_pcb");
        let courtyard = handle_check_clearance(
            &json!({
                "board": board,
                "ref1": "C1",
                "ref2": "R1",
                "mode": "courtyard"
            }),
            &test_ctx(),
        )
        .await
        .unwrap();
        assert!(!courtyard.is_error, "{}", text_of(&courtyard));
        let courtyard: serde_json::Value = serde_json::from_str(&text_of(&courtyard)).unwrap();
        assert_eq!(courtyard["measurement"], MEASUREMENT_COURTYARD_BBOX_EDGE);
        assert_eq!(courtyard["available"], true);
        assert_eq!(courtyard["applicable"], true);
        assert_eq!(courtyard["courtyard_clearance_mm"], 4.455);
        assert_eq!(courtyard["overlaps"], false);
        assert_eq!(courtyard["geometry1"]["rotation_deg"], 90.0);
        assert_eq!(courtyard["geometry2"]["rotation_deg"], -90.0);

        let anchor = handle_check_clearance(
            &json!({ "board": board, "ref1": "C1", "ref2": "R1" }),
            &test_ctx(),
        )
        .await
        .unwrap();
        let anchor: serde_json::Value = serde_json::from_str(&text_of(&anchor)).unwrap();
        assert!(anchor["anchor_distance_mm"].as_f64().unwrap() > 8.0);
        assert_ne!(
            anchor["anchor_distance_mm"],
            courtyard["courtyard_clearance_mm"]
        );
    }

    #[tokio::test]
    async fn check_clearance_refuses_pad_or_anchor_fallback_as_courtyard() {
        let result = handle_check_clearance(
            &json!({
                "board": sexp_fixture("RoyalBlue54L-NFC-Antenna.kicad_pcb"),
                "ref1": "J1",
                "ref2": "REF**",
                "mode": "courtyard"
            }),
            &test_ctx(),
        )
        .await
        .unwrap();
        assert!(!result.is_error, "{}", text_of(&result));
        let value: serde_json::Value = serde_json::from_str(&text_of(&result)).unwrap();
        assert_eq!(value["available"], false);
        assert_eq!(value["courtyard_clearance_mm"], serde_json::Value::Null);
        assert_eq!(value["unavailable_reference"], "J1");
        assert_eq!(value["reason_code"], "authored_courtyard_missing");
    }

    #[tokio::test]
    async fn check_clearance_reports_malformed_courtyard_as_unavailable() {
        let original = std::fs::read_to_string(sexp_fixture("ecc83-pp.kicad_pcb")).unwrap();
        let malformed = original.replacen("(end 7.75 0)", "(end unreadable 0)", 1);
        assert_ne!(malformed, original, "fixture premise changed");
        let dir = tempfile::tempdir().unwrap();
        let board = dir.path().join("malformed.kicad_pcb");
        std::fs::write(&board, malformed).unwrap();

        let result = handle_check_clearance(
            &json!({
                "board": board,
                "ref1": "C1",
                "ref2": "R1",
                "mode": "courtyard"
            }),
            &test_ctx(),
        )
        .await
        .unwrap();
        assert!(!result.is_error, "{}", text_of(&result));
        let value: serde_json::Value = serde_json::from_str(&text_of(&result)).unwrap();
        assert_eq!(value["available"], false);
        assert_eq!(value["unavailable_reference"], "C1");
        assert_eq!(value["reason_code"], "courtyard_geometry_unreadable");
    }

    #[tokio::test]
    async fn check_clearance_reports_same_side_courtyard_overlap_explicitly() {
        let original = std::fs::read_to_string(sexp_fixture("ecc83-pp.kicad_pcb")).unwrap();
        let overlapping =
            original.replacen("(at 136.271 107.95 -90)", "(at 141.605 99.695 -90)", 1);
        assert_ne!(overlapping, original, "fixture premise changed");
        let dir = tempfile::tempdir().unwrap();
        let board = dir.path().join("overlap.kicad_pcb");
        std::fs::write(&board, overlapping).unwrap();

        let result = handle_check_clearance(
            &json!({
                "board": board,
                "ref1": "C1",
                "ref2": "R1",
                "mode": "courtyard"
            }),
            &test_ctx(),
        )
        .await
        .unwrap();
        assert!(!result.is_error, "{}", text_of(&result));
        let value: serde_json::Value = serde_json::from_str(&text_of(&result)).unwrap();
        assert_eq!(value["available"], true);
        assert_eq!(value["courtyard_clearance_mm"], 0.0);
        assert_eq!(value["overlaps"], true);
    }

    #[tokio::test]
    async fn check_clearance_marks_opposite_board_sides_not_applicable() {
        let result = handle_check_clearance(
            &json!({
                "board": sexp_fixture("pic_programmer.kicad_pcb"),
                "ref1": "C1",
                "ref2": "JP1",
                "mode": "courtyard"
            }),
            &test_ctx(),
        )
        .await
        .unwrap();
        assert!(!result.is_error, "{}", text_of(&result));
        let value: serde_json::Value = serde_json::from_str(&text_of(&result)).unwrap();
        assert_eq!(value["available"], false);
        assert_eq!(value["applicable"], false);
        assert_eq!(value["reason_code"], "opposite_board_sides");
        assert_eq!(value["side1"], "front");
        assert_eq!(value["side2"], "back");
    }

    #[test]
    fn courtyard_bbox_spacing_distinguishes_overlap_from_touching() {
        assert_eq!(
            courtyard_bbox_spacing((0.0, 0.0, 2.0, 2.0), (1.0, 1.0, 3.0, 3.0)),
            (0.0, true)
        );
        assert_eq!(
            courtyard_bbox_spacing((0.0, 0.0, 2.0, 2.0), (2.0, 0.0, 4.0, 2.0)),
            (0.0, false)
        );
    }

    /// The description is the contract an LLM reads before calling. It must
    /// not claim clearance again; this pins the wording, not just the code.
    #[test]
    fn check_clearance_schema_and_description_name_both_measurements() {
        let tool = tools()
            .into_iter()
            .find(|tool| tool.name == "check_clearance")
            .expect("check_clearance is registered");
        let description = tool.description.to_lowercase();
        assert!(description.contains("anchor"), "{description}");
        assert!(description.contains("courtyard"), "{description}");
        assert!(
            description.contains("neither mode measures pad, trace or copper clearance"),
            "{description}"
        );
        let modes = &tool.input_schema["properties"]["mode"]["enum"];
        assert_eq!(modes, &json!(["anchor", "courtyard"]));
        assert_eq!(tool.input_schema["properties"]["mode"]["default"], "anchor");
    }
}

/// `copy_routing_pattern` declares all six coordinates required and defaulted
/// each to 0.0. Omitting all six was harmless — the source box collapsed to a
/// point and matched nothing — but omitting only the destination silently
/// duplicated the whole source region onto the board origin and wrote it,
/// reporting `{"copied": N}` (#218).
#[cfg(test)]
mod required_coordinate_tests {
    use super::*;
    use crate::tools::ServerConfig;
    use serde_json::json;
    use std::sync::Arc;

    fn ctx() -> Arc<ToolContext> {
        Arc::new(ToolContext::new(
            ServerConfig {
                kicad_cli: String::new(),
                kicad_binary: String::new(),
                ipc_address: String::new(),
                project_dir: None,
                jlcpcb_db_path: None,
                auto_load_toolsets: false,
                eager_toolsets: false,
            },
            Arc::new(crate::router::ToolRouter::new()),
        ))
    }

    #[tokio::test]
    async fn every_missing_coordinate_is_refused_by_name_without_writing() {
        let dir = tempfile::tempdir().unwrap();
        let board = dir.path().join("b.kicad_pcb");
        let original = "(kicad_pcb\n  (version 20250610)\n  (generator \"konnect\")\n  \
                        (paper \"A4\")\n  (net 0 \"\")\n)\n";
        std::fs::write(&board, original).unwrap();

        let all = [
            ("src_x1", 1.0),
            ("src_y1", 2.0),
            ("src_x2", 3.0),
            ("src_y2", 4.0),
            ("dest_x", 5.0),
            ("dest_y", 6.0),
        ];
        let def = tools()
            .into_iter()
            .find(|t| t.name == "copy_routing_pattern")
            .expect("registered");

        // Leave out exactly one each time: the partial omission is the case
        // that used to write.
        for (omitted, _) in all {
            let mut args = json!({ "board": board.display().to_string() });
            for (key, value) in all {
                if key != omitted {
                    args[key] = json!(value);
                }
            }
            let result = (def.handler)(&args, ctx()).await.expect("no anyhow");
            assert!(result.is_error, "omitting {omitted} must be refused");

            let text = match result.content.first() {
                Some(crate::mcp::protocol::ToolContent::Text { text }) => text.clone(),
                other => panic!("expected text, got {other:?}"),
            };
            let parsed: serde_json::Value = serde_json::from_str(&text).expect("json");
            assert_eq!(parsed["error"]["kind"], "invalid_argument", "{omitted}");
            assert_eq!(
                parsed["error"]["field"], omitted,
                "the refusal must name the coordinate that is missing"
            );
            assert_eq!(
                std::fs::read_to_string(&board).unwrap(),
                original,
                "a refused copy must not touch the board"
            );
        }
    }
}

/// `launch_kicad_ui` used to report `launched: true` for any successful
/// `spawn()`, including a KiCad that exited at once for lack of a display
/// (#764). `/bin/sh` stands in for KiCad and runs the `project` argument as
/// its script, so these run on Unix only.
#[cfg(all(test, unix))]
mod kicad_launch_tests {
    use crate::{mcp::handler::McpHandler, test_support::MockIpcServer, tools::ServerConfig};
    use copperpour_ipc::gen::kiapi;
    use serde_json::{json, Value};
    use std::time::{Duration, Instant};

    /// What KiCad 10.0.6 prints, and its exit code, when started with
    /// `DISPLAY` and `WAYLAND_DISPLAY` unset (the issue's reproduction).
    const NO_DISPLAY: &str =
        "echo '00:41:31: Error: Unable to initialize GTK+, is DISPLAY set properly?' >&2; exit 255\n";

    /// Calls `launch_kicad_ui` through the served dispatch and returns the
    /// result's `isError` and text.
    async fn launch(script: &str, ipc_address: &str, arguments: Value) -> (bool, String) {
        let dir = tempfile::tempdir().unwrap();
        let fake_kicad = dir.path().join("kicad.sh");
        std::fs::write(&fake_kicad, script).unwrap();
        let handler = McpHandler::new(ServerConfig {
            kicad_cli: String::new(),
            kicad_binary: "/bin/sh".into(),
            ipc_address: ipc_address.into(),
            project_dir: None,
            jlcpcb_db_path: None,
            auto_load_toolsets: true,
            eager_toolsets: false,
        })
        .await
        .unwrap();
        let mut args = json!({"project": fake_kicad.display().to_string()});
        args.as_object_mut()
            .unwrap()
            .extend(arguments.as_object().unwrap().clone());
        let result = handler
            .handle_message(json!({"jsonrpc":"2.0", "id":764,
            "method":"tools/call", "params":{"name":"launch_kicad_ui","arguments":args}}))
            .await
            .unwrap()
            .result
            .unwrap();
        (
            result["isError"].as_bool().unwrap_or(false),
            result["content"][0]["text"].as_str().unwrap().to_string(),
        )
    }

    /// A KiCad endpoint that answers every request after `delay`.
    fn ping_answered(delay: Duration) -> MockIpcServer {
        MockIpcServer::spawn("launch-kicad", move |_| {
            std::thread::sleep(delay);
            kiapi::common::ApiResponse {
                status: Some(kiapi::common::ApiResponseStatus {
                    status: kiapi::common::ApiStatusCode::AsOk as i32,
                    error_message: String::new(),
                }),
                header: None,
                message: None,
            }
        })
    }

    const NOBODY: &str = "inproc://copperpour-core-764-nobody-listens";

    fn assert_reports_the_exit(text: &str) {
        assert!(
            text.starts_with("KiCAD exited during startup")
                && text.contains("255")
                // The server's own lack of a display may follow it.
                && text.contains("Error: Unable to initialize GTK+, is DISPLAY set properly?"),
            "{text}"
        );
    }

    #[tokio::test]
    async fn kicad_that_exits_on_startup_is_not_launched() {
        let (is_error, text) = launch(NO_DISPLAY, NOBODY, json!({"wait_ready": false})).await;
        assert!(is_error, "{text}");
        assert_reports_the_exit(&text);
    }

    /// The wait used to run to its timeout and then suggest a retry.
    #[tokio::test]
    async fn waiting_for_ipc_ends_when_kicad_exits() {
        let started = Instant::now();
        let (is_error, text) = launch(
            NO_DISPLAY,
            NOBODY,
            json!({"wait_ready": true, "timeout_seconds": 30}),
        )
        .await;
        assert!(is_error, "{text}");
        assert_reports_the_exit(&text);
        assert!(
            started.elapsed() < Duration::from_secs(10),
            "waited out the timeout"
        );
    }

    /// The first ping goes out at 0.5 s and is answered at 2.5 s; KiCad
    /// exits at 1 s, while that ping is still in flight.
    #[tokio::test]
    async fn kicad_that_exits_during_a_ping_is_not_ready() {
        let slow = ping_answered(Duration::from_secs(2));
        let (is_error, text) = launch(
            "sleep 1; echo 'Segmentation fault' >&2; exit 139\n",
            slow.address(),
            json!({"wait_ready": true, "timeout_seconds": 30}),
        )
        .await;
        assert!(is_error, "{text}");
        assert!(
            text.starts_with("KiCAD exited during startup")
                && text.contains("139")
                && text.contains("Segmentation fault"),
            "{text}"
        );
        // Let the abandoned ping finish, or the runtime waits out its receive
        // timeout on shutdown.
        tokio::time::sleep(Duration::from_secs(2)).await;
        drop(slow);
    }

    #[tokio::test]
    async fn kicad_still_running_after_the_window_is_launched() {
        let (is_error, text) = launch("sleep 3\n", NOBODY, json!({"wait_ready": false})).await;
        assert!(!is_error, "{text}");
        assert_eq!(
            serde_json::from_str::<Value>(&text).unwrap(),
            json!({"launched": true, "ipc_ready": null})
        );
    }

    /// Issue #32: starting Copperpour before KiCad leaves the startup endpoint
    /// unresolved. `launch_kicad_ui(wait_ready: true)` must reset any prior
    /// unresolved-probe backoff and re-probe during its poll loop so it
    /// observes the socket created by the launched KiCad.
    #[tokio::test]
    async fn launch_kicad_ui_wait_ready_observes_endpoint_created_after_launch() {
        let dir = tempfile::tempdir().unwrap();
        let kicad_dir = dir.path().join("kicad");
        std::fs::create_dir_all(&kicad_dir).unwrap();
        let fake_kicad = dir.path().join("kicad.sh");
        std::fs::write(&fake_kicad, "sleep 10\n").unwrap();

        let handler = McpHandler::new(ServerConfig {
            kicad_cli: String::new(),
            kicad_binary: "/bin/sh".into(),
            ipc_address: String::new(),
            project_dir: None,
            jlcpcb_db_path: None,
            auto_load_toolsets: true,
            eager_toolsets: false,
        })
        .await
        .unwrap();
        handler.enable_ipc_rediscovery();
        handler.set_ipc_kicad_dir_for_test(kicad_dir.clone());

        // KiCad is not running yet: the endpoint is unresolved before launch.
        let pre = handler
            .handle_message(json!({
                "jsonrpc": "2.0",
                "id": 31,
                "method": "tools/call",
                "params": {"name": "check_kicad_ui", "arguments": {}}
            }))
            .await
            .unwrap()
            .result
            .unwrap();
        let pre_body: Value =
            serde_json::from_str(pre["content"][0]["text"].as_str().unwrap()).unwrap();
        assert_eq!(pre_body["ipc_responsive"], false, "{pre_body}");

        let socket_addr = format!("ipc://{}", kicad_dir.join("api.sock").display());
        // Bind after the first 500 ms poll misses so the test proves the loop
        // re-probes across iterations rather than succeeding on the first tick.
        let server_task = tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(700)).await;
            MockIpcServer::spawn_at(socket_addr, |_| kiapi::common::ApiResponse {
                status: Some(kiapi::common::ApiResponseStatus {
                    status: kiapi::common::ApiStatusCode::AsOk as i32,
                    error_message: String::new(),
                }),
                header: None,
                message: None,
            })
        });

        let result = handler
            .handle_message(json!({
                "jsonrpc": "2.0",
                "id": 32,
                "method": "tools/call",
                "params": {
                    "name": "launch_kicad_ui",
                    "arguments": {
                        "project": fake_kicad.display().to_string(),
                        "wait_ready": true,
                        "timeout_seconds": 3
                    }
                }
            }))
            .await
            .unwrap()
            .result
            .unwrap();
        let is_error = result["isError"].as_bool().unwrap_or(false);
        let text = result["content"][0]["text"].as_str().unwrap();
        let server = server_task.await.unwrap();
        drop(server);

        assert!(!is_error, "{text}");
        let body: Value = serde_json::from_str(text).unwrap();
        assert_eq!(body["launched"], true, "{body}");
        assert_eq!(body["ipc_ready"], true, "{body}");
    }

    /// Explicit `ipc_address` stays authoritative in `launch_kicad_ui`: with
    /// rediscovery off, a live socket in the discovery directory is never
    /// substituted for the configured endpoint (#32).
    #[tokio::test]
    async fn launch_kicad_ui_wait_ready_preserves_explicit_ipc_address() {
        let dir = tempfile::tempdir().unwrap();
        let kicad_dir = dir.path().join("kicad");
        std::fs::create_dir_all(&kicad_dir).unwrap();
        let fake_kicad = dir.path().join("kicad.sh");
        std::fs::write(&fake_kicad, "sleep 5\n").unwrap();

        let socket_addr = format!("ipc://{}", kicad_dir.join("api.sock").display());
        let _discovered = MockIpcServer::spawn_at(socket_addr, |_| kiapi::common::ApiResponse {
            status: Some(kiapi::common::ApiResponseStatus {
                status: kiapi::common::ApiStatusCode::AsOk as i32,
                error_message: String::new(),
            }),
            header: None,
            message: None,
        });

        let handler = McpHandler::new(ServerConfig {
            kicad_cli: String::new(),
            kicad_binary: "/bin/sh".into(),
            ipc_address: NOBODY.into(),
            project_dir: None,
            jlcpcb_db_path: None,
            auto_load_toolsets: true,
            eager_toolsets: false,
        })
        .await
        .unwrap();
        handler.set_ipc_kicad_dir_for_test(kicad_dir);

        let result = handler
            .handle_message(json!({
                "jsonrpc": "2.0",
                "id": 33,
                "method": "tools/call",
                "params": {
                    "name": "launch_kicad_ui",
                    "arguments": {
                        "project": fake_kicad.display().to_string(),
                        "wait_ready": true,
                        "timeout_seconds": 1
                    }
                }
            }))
            .await
            .unwrap()
            .result
            .unwrap();
        let is_error = result["isError"].as_bool().unwrap_or(false);
        let text = result["content"][0]["text"].as_str().unwrap();
        assert!(!is_error, "{text}");
        let body: Value = serde_json::from_str(text).unwrap();
        assert_eq!(body["launched"], true, "{body}");
        assert_eq!(body["ipc_ready"], false, "{body}");
    }

    /// KiCad up for the whole wait and still `AS_NOT_READY` is most likely
    /// held by a dialog; one nobody answers for is not (#225).
    #[tokio::test]
    async fn only_a_kicad_not_ready_for_the_whole_wait_is_blamed_on_a_dialog() {
        let not_ready = MockIpcServer::not_ready("launch-kicad-ui");
        for (address, note) in [
            (not_ready.address(), crate::tools::NOT_READY_NOTE),
            (
                NOBODY,
                "KiCAD launched but IPC not yet responsive within timeout",
            ),
        ] {
            let (is_error, text) = launch(
                "sleep 3\n",
                address,
                json!({"wait_ready": true, "timeout_seconds": 1}),
            )
            .await;
            assert!(!is_error, "{text}");
            let body: Value = serde_json::from_str(&text).unwrap();
            assert_eq!(body["ipc_ready"], false, "{body}");
            assert_eq!(body["note"], note, "{body}");
        }
    }

    /// The display reaches KiCad, and the no-display hint is left out.
    /// macOS refuses `display`.
    #[cfg(not(target_os = "macos"))]
    #[tokio::test]
    async fn kicad_starts_on_the_requested_display() {
        let (is_error, text) = launch(
            "echo \"DISPLAY=$DISPLAY\" >&2; exit 1\n",
            NOBODY,
            json!({"wait_ready": false, "display": ":42"}),
        )
        .await;
        assert!(is_error, "{text}");
        assert!(text.ends_with("DISPLAY=:42"), "{text}");
    }

    #[tokio::test]
    async fn a_display_that_is_not_an_x_display_name_is_refused() {
        for display in [
            "",
            "wayland-0",
            ":",
            ":0.",
            "localhost:0",
            "evil.example:0",
            "unix:",
        ] {
            let (is_error, text) = launch(
                "echo started >&2; exit 1\n",
                NOBODY,
                json!({"wait_ready": false, "display": display}),
            )
            .await;
            assert!(is_error, "{text}");
            let body: Value = serde_json::from_str(&text).unwrap();
            assert_eq!(body["error"]["kind"], "invalid_argument", "{body}");
            assert_eq!(body["error"]["field"], "display", "{body}");
        }
    }

    #[tokio::test]
    async fn kicad_that_answers_ipc_is_ready() {
        let kicad = ping_answered(Duration::ZERO);
        let (is_error, text) = launch(
            "sleep 3\n",
            kicad.address(),
            json!({"wait_ready": true, "timeout_seconds": 10}),
        )
        .await;
        assert!(!is_error, "{text}");
        assert_eq!(
            serde_json::from_str::<Value>(&text).unwrap(),
            json!({"launched": true, "ipc_ready": true})
        );
    }
}

/// `open_board` (#23). A `sh` script stands in for KiCad's PCB editor: it
/// marks the board `.started` and runs until the test marks it `.done`.
#[cfg(all(test, unix))]
mod open_board_tests {
    use super::{
        handle_open_board, look_for_board, open_board, EndpointSeen, Looking, OpenBoardCall,
    };
    use crate::router::ToolRouter;
    use crate::test_support::{resolved_dir, MockIpcServer};
    use crate::tools::pcb_board::board_mock::{
        board_document, spawn_kicad_holding_board, spawn_kicad_holding_board_refusing,
    };
    use crate::tools::{ServerConfig, ToolContext};
    use crate::write_executable_script::write_executable_script;
    use copperpour_ipc::gen::kiapi;
    use serde_json::{json, Value};
    use std::path::{Path, PathBuf};
    use std::sync::Arc;
    use std::time::Duration;

    /// It writes the path it was given to `<board>.started`, a line per start,
    /// and ends when the test marks the board done or removes it.
    const EDITOR: &str = "#!/bin/sh\nprintf '%s\\n' \"$1\" >> \"$1.started\"\n\
                          while [ -e \"$1\" ] && [ ! -e \"$1.done\" ]; do sleep 0.05; done\n";

    /// A board in a directory of its own, the KiCad install beside it: a
    /// `kicad` to resolve and the PCB editor `script`.
    struct Fixture {
        _dir: tempfile::TempDir,
        board: PathBuf,
        kicad: PathBuf,
    }

    impl Fixture {
        fn new(script: &str) -> Self {
            let dir = tempfile::tempdir().unwrap();
            let root = resolved_dir(dir.path());
            let board = root.join("my board.kicad_pcb");
            std::fs::write(&board, "(kicad_pcb)\n").unwrap();
            let bin = root.join("bin");
            std::fs::create_dir(&bin).unwrap();
            let kicad = bin.join("kicad");
            std::fs::write(&kicad, "").unwrap();
            write_executable_script(&bin.join("pcbnew"), script);
            Fixture {
                _dir: dir,
                board,
                kicad,
            }
        }

        fn ctx(&self, ipc_address: &str) -> ToolContext {
            ToolContext::new(
                ServerConfig {
                    kicad_binary: self.kicad.display().to_string(),
                    ipc_address: ipc_address.to_string(),
                    ..ServerConfig::default()
                },
                Arc::new(ToolRouter::new()),
            )
        }

        fn marker(&self, suffix: &str) -> PathBuf {
            PathBuf::from(format!("{}.{suffix}", self.board.display()))
        }

        fn editor_started(&self) -> bool {
            self.marker("started").exists()
        }

        /// How many editors were started on the board.
        fn editors_started(&self) -> usize {
            std::fs::read_to_string(self.marker("started"))
                .map(|text| text.lines().count())
                .unwrap_or(0)
        }

        async fn open(&self, ctx: &ToolContext, arguments: Value) -> (bool, String) {
            let mut args = json!({ "board": self.board });
            args.as_object_mut()
                .unwrap()
                .extend(arguments.as_object().unwrap().clone());
            let result = handle_open_board(&args, ctx).await.unwrap();
            (result.is_error, text_of(&result))
        }

        fn rediscovering_ctx(&self) -> ToolContext {
            let ctx = self.ctx("");
            ctx.enable_ipc_rediscovery();
            ctx
        }

        /// As the served tool does with rediscovery on, looking at
        /// `addresses` instead of the sockets on this machine.
        async fn open_at(&self, addresses: Vec<String>, arguments: Value) -> (bool, String) {
            self.open_with(&self.rediscovering_ctx(), addresses, arguments)
                .await
        }

        async fn open_with(
            &self,
            ctx: &ToolContext,
            addresses: Vec<String>,
            arguments: Value,
        ) -> (bool, String) {
            self.open_looking(ctx, move || addresses.clone(), arguments)
                .await
        }

        /// As [`Self::open_with`], with the endpoints of each look from
        /// `endpoints`.
        async fn open_looking(
            &self,
            ctx: &ToolContext,
            endpoints: impl Fn() -> Vec<String> + Send + Sync + 'static,
            arguments: Value,
        ) -> (bool, String) {
            let mut args = json!({ "board": self.board });
            args.as_object_mut()
                .unwrap()
                .extend(arguments.as_object().unwrap().clone());
            let looking = Looking {
                endpoints: Box::new(endpoints),
                unreachable_editor: None,
            };
            let result = open_board(&args, ctx, looking).await.unwrap();
            (result.is_error, text_of(&result))
        }
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = std::fs::write(self.marker("done"), "");
        }
    }

    fn reply(
        status: kiapi::common::ApiStatusCode,
        error: &str,
        message: Option<prost_types::Any>,
    ) -> kiapi::common::ApiResponse {
        kiapi::common::ApiResponse {
            status: Some(kiapi::common::ApiResponseStatus {
                status: status as i32,
                error_message: error.to_string(),
            }),
            header: None,
            message,
        }
    }

    /// A KiCad Project Manager: it answers the Ping and has no handler for
    /// `GetOpenDocuments`, as KiCad 10.0.6 answers on its `api.sock` (#87).
    fn project_manager() -> MockIpcServer {
        MockIpcServer::spawn("open-board-manager", |request| {
            let command = request.message.expect("a command");
            if command.type_url.ends_with("GetOpenDocuments") {
                reply(
                    kiapi::common::ApiStatusCode::AsUnhandled,
                    "no handler available for request of type \
                     kiapi.common.commands.GetOpenDocuments",
                    None,
                )
            } else {
                reply(kiapi::common::ApiStatusCode::AsOk, "", None)
            }
        })
    }

    /// A PCB editor holding `board`, at `address`.
    fn editor_at(address: String, board: &Path) -> MockIpcServer {
        let documents = vec![board_document(&board.to_string_lossy())];
        MockIpcServer::spawn_at(address, move |request| {
            let command = request.message.expect("a command");
            let message = command.type_url.ends_with("GetOpenDocuments").then(|| {
                copperpour_ipc::builders::pack_any(
                    &kiapi::common::commands::GetOpenDocumentsResponse {
                        documents: documents.clone(),
                    },
                    "kiapi.common.commands.GetOpenDocumentsResponse",
                )
            });
            reply(kiapi::common::ApiStatusCode::AsOk, "", message)
        })
    }

    fn unused_address(purpose: &str) -> String {
        format!(
            "inproc://copperpour-core-open-board-{}-{purpose}",
            std::process::id()
        )
    }

    fn body(text: &str) -> Value {
        serde_json::from_str(text).unwrap_or_else(|_| panic!("not JSON: {text}"))
    }

    fn text_of(result: &crate::mcp::protocol::CallToolResult) -> String {
        match &result.content[0] {
            crate::mcp::protocol::ToolContent::Text { text } => text.clone(),
            other => panic!("not text: {other:?}"),
        }
    }

    #[tokio::test]
    async fn a_path_that_is_not_a_board_is_refused_before_anything_starts() {
        let fixture = Fixture::new(EDITOR);
        let ctx = fixture.ctx("");
        let schematic = fixture.board.with_extension("kicad_sch");
        std::fs::write(&schematic, "").unwrap();
        let directory = fixture.board.with_file_name("folder.kicad_pcb");
        std::fs::create_dir(&directory).unwrap();
        let missing = fixture.board.with_file_name("missing.kicad_pcb");
        for (path, kind) in [
            (&schematic, "invalid_argument"),
            (&directory, "invalid_argument"),
            (&missing, "file_not_found"),
        ] {
            let result = handle_open_board(&json!({ "board": path }), &ctx)
                .await
                .unwrap();
            assert!(result.is_error);
            let text = text_of(&result);
            assert_eq!(body(&text)["error"]["kind"], kind, "{text}");
        }
        assert!(!fixture.editor_started());
    }

    #[tokio::test]
    async fn an_endpoint_already_holding_the_board_is_used_and_no_editor_starts() {
        let fixture = Fixture::new(EDITOR);
        let kicad = spawn_kicad_holding_board(&fixture.board, |_| None);
        let ctx = fixture.ctx(kicad.address());
        let (is_error, text) = fixture.open(&ctx, json!({})).await;
        assert!(!is_error, "{text}");
        let answer = body(&text);
        assert_eq!(answer["requested_open"], true, "{answer}");
        assert_eq!(answer["launched"], false, "{answer}");
        assert_eq!(answer["ipc_address"], kicad.address(), "{answer}");
        assert!(!fixture.editor_started());
        assert!(ctx.board_session.was_observed_live(&fixture.board));
    }

    /// An editor started now would serve a socket of its own, which a
    /// configured address never follows.
    #[tokio::test]
    async fn a_configured_endpoint_answering_without_the_board_starts_no_editor() {
        let fixture = Fixture::new(EDITOR);
        let other = fixture.board.with_file_name("other.kicad_pcb");
        std::fs::write(&other, "").unwrap();
        let other_editor = spawn_kicad_holding_board(&other, |_| None);
        let manager = project_manager();
        for kicad in [&manager, &other_editor] {
            let ctx = fixture.ctx(kicad.address());
            let (is_error, text) = fixture.open(&ctx, json!({})).await;
            assert!(is_error, "{text}");
            let refusal = body(&text);
            assert_eq!(refusal["error"]["kind"], "editor_unavailable", "{refusal}");
            assert!(
                refusal["message"]
                    .as_str()
                    .unwrap()
                    .contains("answers without"),
                "{refusal}"
            );
            assert!(!fixture.editor_started(), "{text}");
        }
    }

    /// A configured endpoint other than KiCad's own socket is one no editor
    /// started now would serve.
    #[tokio::test]
    async fn a_configured_endpoint_kicad_never_binds_starts_no_editor() {
        let fixture = Fixture::new(EDITOR);
        let ctx = fixture.ctx(&unused_address("configured-elsewhere"));
        let (is_error, text) = fixture.open(&ctx, json!({})).await;
        assert!(is_error, "{text}");
        let refusal = body(&text);
        assert_eq!(refusal["error"]["kind"], "editor_unavailable", "{refusal}");
        assert!(
            refusal["message"]
                .as_str()
                .unwrap()
                .contains("is not the socket KiCad binds"),
            "{refusal}"
        );
        assert!(!fixture.editor_started(), "{text}");
    }

    /// AS_NOT_READY is a KiCad behind a dialog or still loading, which may
    /// hold the board.
    #[tokio::test]
    async fn a_kicad_that_is_not_ready_starts_no_editor() {
        let fixture = Fixture::new(EDITOR);
        let kicad = MockIpcServer::not_ready("open-board-not-ready");
        let (is_error, text) = fixture
            .open_at(
                vec![kicad.address().to_string()],
                json!({ "timeout_seconds": 1 }),
            )
            .await;
        assert!(!is_error, "{text}");
        let answer = body(&text);
        assert_eq!(answer["timed_out"], true, "{answer}");
        assert_eq!(answer["launched"], false, "{answer}");
        assert_eq!(answer["note"], crate::tools::NOT_READY_NOTE, "{answer}");
        assert!(!fixture.editor_started());
    }

    /// KiCad's lock with no endpoint reporting the board is a KiCad out of
    /// reach holding it, or a lock left behind.
    #[tokio::test]
    async fn a_locked_board_no_endpoint_reports_starts_no_editor() {
        let fixture = Fixture::new(EDITOR);
        let lock = copperpour_sexp::writer::kicad_editor_lock_path(&fixture.board).unwrap();
        std::fs::write(&lock, "").unwrap();
        let (is_error, text) = fixture
            .open_at(vec![unused_address("locked")], json!({}))
            .await;
        assert!(is_error, "{text}");
        let refusal = body(&text);
        assert_eq!(refusal["error"]["kind"], "conflict", "{refusal}");
        assert_eq!(
            refusal["error"]["paths"],
            json!([lock.display().to_string()]),
            "{refusal}"
        );
        assert!(!fixture.editor_started(), "{text}");
    }

    /// A lock that cannot be checked may be a KiCad's, so it refuses as one
    /// that is present does.
    #[test]
    fn a_lock_that_cannot_be_checked_refuses_as_a_present_one() {
        let board = Path::new("/boards/a.kicad_pcb");
        let lock = copperpour_sexp::writer::kicad_editor_lock_path(board).unwrap();
        let observed = crate::tools::live_board::editor_lock_with(board, |_| {
            Err(std::io::Error::new(
                std::io::ErrorKind::PermissionDenied,
                "denied",
            ))
        });
        let refusal = call(board, "/boards/a.kicad_pcb")
            .locked(&[], observed)
            .expect("a lock that cannot be checked refuses");
        assert!(refusal.is_error);
        let refusal = body(&text_of(&refusal));
        assert_eq!(refusal["error"]["kind"], "conflict", "{refusal}");
        assert_eq!(
            refusal["error"]["paths"],
            json!([lock.display().to_string()]),
            "{refusal}"
        );
        assert!(
            refusal["message"]
                .as_str()
                .unwrap()
                .contains("could not be checked (denied)"),
            "{refusal}"
        );
    }

    #[tokio::test]
    async fn a_started_editor_is_used_once_it_reports_the_board() {
        let fixture = Fixture::new(EDITOR);
        let address = unused_address("started-editor");
        let started = fixture.marker("started");
        let board = fixture.board.clone();
        let editor = std::thread::spawn({
            let address = address.clone();
            move || {
                let given_up = std::time::Instant::now() + Duration::from_secs(30);
                while !started.exists() && std::time::Instant::now() < given_up {
                    std::thread::sleep(Duration::from_millis(20));
                }
                editor_at(address, &board)
            }
        });
        let (is_error, text) = fixture
            .open_at(vec![address.clone()], json!({ "timeout_seconds": 20 }))
            .await;
        drop(editor.join().unwrap());
        assert!(!is_error, "{text}");
        let answer = body(&text);
        assert_eq!(answer["requested_open"], true, "{answer}");
        assert_eq!(answer["launched"], true, "{answer}");
        assert_eq!(answer["ipc_address"], address, "{answer}");
        assert_eq!(answer["timed_out"], false, "{answer}");
    }

    #[tokio::test]
    async fn an_editor_that_exits_during_startup_is_reported() {
        let fixture = Fixture::new("#!/bin/sh\necho 'pcbnew: cannot open the board' >&2\nexit 3\n");
        let (is_error, text) = fixture
            .open_at(vec![unused_address("exits")], json!({}))
            .await;
        assert!(is_error, "{text}");
        assert!(
            text.starts_with("KiCad's PCB editor exited during startup")
                && text.contains("pcbnew: cannot open the board"),
            "{text}"
        );
    }

    /// An editor that exits while a look waits on an endpoint is reported
    /// then, with its stderr, not as a timeout at the deadline.
    #[tokio::test]
    async fn an_editor_that_exits_during_a_look_is_reported() {
        use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
        // It outlives the first watch, so it exits during the look after it.
        let fixture = Fixture::new(
            "#!/bin/sh\nprintf '%s\\n' \"$1\" >> \"$1.started\"\nsleep 1\n\
             echo 'pcbnew: lost the display' >&2\nexit 3\n",
        );
        let answered = Arc::new(AtomicBool::new(false));
        let slow = MockIpcServer::spawn("open-board-slow-look", {
            let answered = Arc::clone(&answered);
            move |_| {
                std::thread::sleep(Duration::from_secs(4));
                answered.store(true, Ordering::SeqCst);
                reply(kiapi::common::ApiStatusCode::AsNotReady, "busy", None)
            }
        });
        // Nothing answers before the editor starts; after, an endpoint
        // answers only past the deadline.
        let looks = AtomicUsize::new(0);
        let before = unused_address("exits-during-a-look");
        let after = slow.address().to_string();
        let (is_error, text) = fixture
            .open_looking(
                &fixture.rediscovering_ctx(),
                move || match looks.fetch_add(1, Ordering::SeqCst) {
                    0 => vec![before.clone()],
                    _ => vec![after.clone()],
                },
                json!({ "timeout_seconds": 3 }),
            )
            .await;
        assert!(is_error, "{text}");
        assert!(
            text.starts_with("KiCad's PCB editor exited during startup")
                && text.contains("pcbnew: lost the display"),
            "{text}"
        );
        while !answered.load(Ordering::SeqCst) {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
        drop(slow);
    }

    #[tokio::test]
    async fn no_endpoint_reporting_the_board_in_time_is_a_timeout_with_a_note() {
        let fixture = Fixture::new(EDITOR);
        let (is_error, text) = fixture
            .open_at(
                vec![unused_address("nobody")],
                json!({ "timeout_seconds": 1 }),
            )
            .await;
        assert!(!is_error, "{text}");
        let answer = body(&text);
        assert_eq!(answer["requested_open"], false, "{answer}");
        assert_eq!(answer["launched"], true, "{answer}");
        assert_eq!(answer["timed_out"], true, "{answer}");
        assert!(answer["note"].as_str().unwrap().contains("api-<pid>.sock"));
        assert!(fixture.editor_started());
    }

    /// A KiCad slower than the deadline may hold the board, so no editor is
    /// started on it.
    #[tokio::test]
    async fn an_endpoint_slower_than_the_deadline_starts_no_editor() {
        use std::sync::atomic::{AtomicBool, Ordering};
        let fixture = Fixture::new(EDITOR);
        let answered = Arc::new(AtomicBool::new(false));
        let busy = MockIpcServer::spawn("open-board-busy", {
            let answered = Arc::clone(&answered);
            move |_| {
                std::thread::sleep(Duration::from_secs(2));
                answered.store(true, Ordering::SeqCst);
                reply(kiapi::common::ApiStatusCode::AsNotReady, "busy", None)
            }
        });
        let ctx = fixture.ctx(busy.address());
        let (is_error, text) = fixture.open(&ctx, json!({ "timeout_seconds": 1 })).await;
        assert!(!is_error, "{text}");
        let answer = body(&text);
        assert_eq!(answer["timed_out"], true, "{answer}");
        assert_eq!(answer["launched"], false, "{answer}");
        assert!(answer["note"].as_str().unwrap().contains("no clear answer"));
        assert!(!fixture.editor_started());
        // The look still waits on its Ping; a mock gone before it answers
        // would hold the runtime's shutdown for the client's reply timeout.
        while !answered.load(Ordering::SeqCst) {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
        drop(busy);
    }

    /// The endpoints are looked at together: one still answering at the
    /// deadline does not keep the one holding the board from being found.
    #[tokio::test]
    async fn an_endpoint_still_answering_does_not_hide_the_holder() {
        use std::sync::atomic::{AtomicBool, Ordering};
        let fixture = Fixture::new(EDITOR);
        let answered = Arc::new(AtomicBool::new(false));
        let busy = MockIpcServer::spawn("open-board-slow", {
            let answered = Arc::clone(&answered);
            move |_| {
                std::thread::sleep(Duration::from_secs(4));
                answered.store(true, Ordering::SeqCst);
                reply(kiapi::common::ApiStatusCode::AsNotReady, "busy", None)
            }
        });
        let holder = spawn_kicad_holding_board(&fixture.board, |_| None);
        let (is_error, text) = fixture
            .open_at(
                vec![busy.address().to_string(), holder.address().to_string()],
                json!({ "timeout_seconds": 10 }),
            )
            .await;
        assert!(!is_error, "{text}");
        let answer = body(&text);
        assert_eq!(answer["requested_open"], true, "{answer}");
        assert_eq!(answer["ipc_address"], holder.address(), "{answer}");
        assert_eq!(answer["endpoints"][0]["error"], "no answer yet", "{answer}");
        // A poll after the holder answered, not the slow one's 4 s.
        assert!(answer["elapsed_ms"].as_u64().unwrap() < 3000, "{answer}");
        assert!(!fixture.editor_started());
        while !answered.load(Ordering::SeqCst) {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
        drop(busy);
    }

    /// A Ping answered with an error status, as a timed-out or malformed
    /// reply, is no answer about the board: no editor is started.
    #[tokio::test]
    async fn a_ping_answered_with_an_error_starts_no_editor() {
        let fixture = Fixture::new(EDITOR);
        let kicad = MockIpcServer::spawn("open-board-busy-ping", |_| {
            reply(kiapi::common::ApiStatusCode::AsBusy, "busy", None)
        });
        let (is_error, text) = fixture
            .open_at(
                vec![kicad.address().to_string()],
                json!({ "timeout_seconds": 1 }),
            )
            .await;
        assert!(!is_error, "{text}");
        let answer = body(&text);
        assert_eq!(answer["timed_out"], true, "{answer}");
        assert_eq!(answer["launched"], false, "{answer}");
        assert!(answer["note"].as_str().unwrap().contains("no clear answer"));
        assert!(!fixture.editor_started());
    }

    /// An open-document list that does not parse is no answer either.
    #[tokio::test]
    async fn a_document_list_that_does_not_parse_starts_no_editor() {
        let fixture = Fixture::new(EDITOR);
        let kicad = MockIpcServer::spawn("open-board-garbled", |request| {
            let command = request.message.expect("a command");
            let message =
                command
                    .type_url
                    .ends_with("GetOpenDocuments")
                    .then(|| prost_types::Any {
                        type_url: "type.googleapis.com/kiapi.common.commands.Unknown".to_string(),
                        value: vec![1, 2, 3],
                    });
            reply(kiapi::common::ApiStatusCode::AsOk, "", message)
        });
        let (is_error, text) = fixture
            .open_at(
                vec![kicad.address().to_string()],
                json!({ "timeout_seconds": 1 }),
            )
            .await;
        assert!(!is_error, "{text}");
        let answer = body(&text);
        assert_eq!(answer["timed_out"], true, "{answer}");
        assert_eq!(answer["launched"], false, "{answer}");
        assert!(!fixture.editor_started());
    }

    /// The editor gets the resolved path, as the lock checks resolve it.
    #[tokio::test]
    async fn the_editor_is_given_the_resolved_path() {
        let fixture = Fixture::new(EDITOR);
        let spelled = fixture
            .board
            .parent()
            .unwrap()
            .join("bin")
            .join("..")
            .join(fixture.board.file_name().unwrap());
        let (_, text) = fixture
            .open_at(
                vec![unused_address("resolved")],
                json!({ "board": spelled, "timeout_seconds": 1 }),
            )
            .await;
        let given = std::fs::read_to_string(fixture.marker("started")).unwrap();
        assert_eq!(
            given.trim_end(),
            fixture.board.display().to_string(),
            "{text}"
        );
    }

    /// The board in the list is not enough to use an endpoint: a read of it
    /// must succeed too. Nor is it nothing: KiCad has the board, so no second
    /// editor is started while the read is refused, as during a zone refill.
    #[tokio::test]
    async fn an_endpoint_listing_the_board_but_serving_no_read_starts_no_editor() {
        let fixture = Fixture::new(EDITOR);
        let kicad = spawn_kicad_holding_board_refusing(
            &fixture.board,
            vec![kiapi::common::types::KiCadObjectType::KotPcbDimension],
            |_| None,
        );
        let board = fixture.board.clone();
        let address = kicad.address().to_string();
        let seen = tokio::task::spawn_blocking(move || look_for_board(&board, &address))
            .await
            .unwrap();
        assert!(seen.responsive && seen.listed && !seen.holds_board);
        assert!(seen.undecided());

        let (is_error, text) = fixture
            .open_at(
                vec![kicad.address().to_string()],
                json!({ "timeout_seconds": 1 }),
            )
            .await;
        assert!(!is_error, "{text}");
        let answer = body(&text);
        assert_eq!(answer["timed_out"], true, "{answer}");
        assert_eq!(answer["launched"], false, "{answer}");
        assert!(answer["note"].as_str().unwrap().contains("lists the board"));
        assert!(!fixture.editor_started());
    }

    /// Two calls at once: the second waits for the first, then finds the
    /// editor it started, so only one is started.
    #[tokio::test]
    async fn two_calls_at_once_start_one_editor() {
        let fixture = Fixture::new(EDITOR);
        let address = unused_address("two-calls");
        let started = fixture.marker("started");
        let board = fixture.board.clone();
        let editor = std::thread::spawn({
            let address = address.clone();
            move || {
                let given_up = std::time::Instant::now() + Duration::from_secs(30);
                while !started.exists() && std::time::Instant::now() < given_up {
                    std::thread::sleep(Duration::from_millis(20));
                }
                editor_at(address, &board)
            }
        });
        let ctx = fixture.rediscovering_ctx();
        let arguments = json!({ "timeout_seconds": 20 });
        let (first, second) = tokio::join!(
            fixture.open_with(&ctx, vec![address.clone()], arguments.clone()),
            fixture.open_with(&ctx, vec![address.clone()], arguments.clone()),
        );
        drop(editor.join().unwrap());
        let launched: Vec<bool> = [first, second]
            .iter()
            .map(|(is_error, text)| {
                assert!(!is_error, "{text}");
                let answer = body(text);
                assert_eq!(answer["requested_open"], true, "{answer}");
                answer["launched"].as_bool().unwrap()
            })
            .collect();
        assert_eq!(launched.iter().filter(|launched| **launched).count(), 1);
        assert_eq!(fixture.editors_started(), 1);
    }

    /// KiCad's own socket, however the configuration spells it, is one an
    /// editor started now would serve.
    #[test]
    fn a_configured_kicad_socket_is_known_however_it_is_spelled() {
        let default = copperpour_ipc::candidate_socket_paths()[0].clone();
        let spelled = if cfg!(windows) {
            format!("ipc://{}", default.display())
                .replace('\\', "/")
                .to_ascii_lowercase()
        } else {
            format!("ipc://{}", default.display()).replacen("/kicad/", "//kicad/", 1)
        };
        let configured = |address: &str| {
            ToolContext::new(
                ServerConfig {
                    ipc_address: address.to_string(),
                    ..ServerConfig::default()
                },
                Arc::new(ToolRouter::new()),
            )
        };
        assert!(Looking::for_context(&configured(&spelled))
            .unreachable_editor
            .is_none());
        assert!(
            Looking::for_context(&configured("ipc:///elsewhere/api.sock"))
                .unreachable_editor
                .is_some()
        );
    }

    #[test]
    fn same_socket_path_is_case_insensitive_only_on_windows() {
        let canonical = Path::new(r"C:\Users\Runner\AppData\Local\Temp\kicad\api.sock");
        let mixed = Path::new(r"c:/users/runner/appdata/local/temp//kicad/API.SOCK");
        let elsewhere = Path::new(r"C:\elsewhere\kicad\api.sock");

        assert!(super::same_socket_path_for(canonical, mixed, true));
        assert!(!super::same_socket_path_for(canonical, elsewhere, true));
        assert!(!super::same_socket_path_for(
            Path::new("/tmp/kicad/api.sock"),
            Path::new("/tmp/KiCad/api.sock"),
            false,
        ));
    }

    /// Through the served dispatch, as a client calls it.
    #[tokio::test]
    async fn the_served_tool_uses_an_endpoint_holding_the_board() {
        let fixture = Fixture::new(EDITOR);
        let kicad = spawn_kicad_holding_board(&fixture.board, |_| None);
        let handler = crate::mcp::handler::McpHandler::new(ServerConfig {
            kicad_binary: fixture.kicad.display().to_string(),
            ipc_address: kicad.address().to_string(),
            auto_load_toolsets: true,
            ..ServerConfig::default()
        })
        .await
        .unwrap();
        let result = handler
            .handle_message(json!({"jsonrpc":"2.0", "id":23, "method":"tools/call",
                "params":{"name":"open_board","arguments":{"board": fixture.board}}}))
            .await
            .unwrap()
            .result
            .unwrap();
        assert_ne!(result["isError"], true, "{result}");
        let answer = body(result["content"][0]["text"].as_str().unwrap());
        assert_eq!(answer["requested_open"], true, "{answer}");
        assert_eq!(answer["launched"], false, "{answer}");
        assert!(!fixture.editor_started());
    }

    fn call<'a>(board: &'a Path, shown: &'a str) -> OpenBoardCall<'a> {
        OpenBoardCall {
            board,
            shown,
            timeout_seconds: 30,
            started: std::time::Instant::now(),
        }
    }

    fn selected(ctx: &ToolContext) -> Option<String> {
        ctx.selected_ipc_endpoint.lock().unwrap().clone()
    }

    /// The Project Manager and an editor holding another board both answer
    /// the Ping, which is all detection can tell them apart by; the board
    /// each reports is what chooses.
    #[tokio::test]
    async fn the_endpoint_holding_the_board_is_chosen_among_those_that_answer() {
        let fixture = Fixture::new(EDITOR);
        let other = fixture.board.with_file_name("other.kicad_pcb");
        std::fs::write(&other, "").unwrap();
        let manager = project_manager();
        let other_editor = spawn_kicad_holding_board(&other, |_| None);
        let holder = spawn_kicad_holding_board(&fixture.board, |_| None);
        let board = fixture.board.clone();
        let addresses: Vec<String> = [&manager, &other_editor, &holder]
            .iter()
            .map(|kicad| kicad.address().to_string())
            .collect();
        let seen: Vec<EndpointSeen> = tokio::task::spawn_blocking(move || {
            addresses
                .iter()
                .map(|address| look_for_board(&board, address))
                .collect()
        })
        .await
        .unwrap();
        assert!(seen[0].responsive && !seen[0].holds_board);
        assert!(seen[0].error.as_deref().unwrap().contains("AS_UNHANDLED"));
        assert!(seen[1].responsive && !seen[1].holds_board);
        assert!(seen[2].holds_board);

        let ctx = fixture.ctx("");
        ctx.enable_ipc_rediscovery();
        let shown = fixture.board.display().to_string();
        let answer = call(&fixture.board, &shown)
            .found(&ctx, &seen, false)
            .unwrap();
        let answer = body(&text_of(&answer));
        assert_eq!(answer["ipc_address"], holder.address(), "{answer}");
        assert_eq!(answer["endpoints"].as_array().unwrap().len(), 3);
        assert_eq!(selected(&ctx).as_deref(), Some(holder.address()));
    }

    #[test]
    fn two_endpoints_holding_the_board_choose_neither() {
        let board = Path::new("/boards/a.kicad_pcb");
        let holder = |address: &str| EndpointSeen {
            address: address.to_string(),
            holds_board: true,
            listed: true,
            responsive: true,
            not_ready: false,
            unsure: false,
            answered: true,
            error: None,
        };
        let seen = [holder("ipc:///k/api.sock"), holder("ipc:///k/api-42.sock")];
        let ctx = ToolContext::new(ServerConfig::default(), Arc::new(ToolRouter::new()));
        ctx.enable_ipc_rediscovery();
        let answer = call(board, "/boards/a.kicad_pcb")
            .found(&ctx, &seen, true)
            .unwrap();
        assert!(answer.is_error);
        let refusal = body(&text_of(&answer));
        assert_eq!(refusal["error"]["kind"], "ambiguous_target", "{refusal}");
        assert_eq!(
            refusal["error"]["target"], "/boards/a.kicad_pcb",
            "{refusal}"
        );
        assert_eq!(
            refusal["error"]["candidates"],
            json!(["ipc:///k/api.sock", "ipc:///k/api-42.sock"]),
            "{refusal}"
        );
        assert_eq!(selected(&ctx), None);
    }
}

#[cfg(test)]
mod drc_rule_filter_tests {
    use super::*;

    #[test]
    fn drc_rules_filter_distinguishes_omitted_empty_and_deduplicated_names() {
        assert_eq!(drc_rules_filter(&json!({})).unwrap(), None);
        assert_eq!(
            drc_rules_filter(&json!({"rules":[]})).unwrap(),
            Some(BTreeSet::new())
        );
        assert_eq!(
            drc_rules_filter(&json!({"rules":["z","a","z"]})).unwrap(),
            Some(BTreeSet::from(["a".into(), "z".into()]))
        );
    }

    #[test]
    fn drc_rules_filter_refuses_non_array_present_values_in_direct_calls() {
        for value in [json!(null), json!("clearance"), json!(false), json!(1)] {
            let error = drc_rules_filter(&json!({"rules":value}))
                .expect_err("malformed rule list must refuse");
            assert_eq!(
                crate::mcp::error::extract_error_kind(&error).as_deref(),
                Some("invalid_argument")
            );
        }
    }

    #[test]
    fn drc_rules_filter_refuses_non_string_entries_in_direct_calls() {
        for value in [json!(null), json!(1), json!(false), json!({})] {
            let error = drc_rules_filter(&json!({"rules":["clearance",value]}))
                .expect_err("malformed rule name must refuse");
            let crate::mcp::protocol::ToolContent::Text { text } = &error.content[0] else {
                panic!()
            };
            let body: Value = serde_json::from_str(text).unwrap();
            assert_eq!(body["error"]["kind"], "invalid_argument");
            assert_eq!(body["error"]["field"], "rules[1]");
        }
    }
}
