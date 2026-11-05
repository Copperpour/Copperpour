//! `manufacturing` toolset — Design-to-fab pipeline: export packages, cost estimation, validation.
//!
//! Orchestrates gerber export, BOM generation, and pick-and-place file creation
//! into a single manufacturing-ready package for a specific fab house.

use crate::mcp::protocol::CallToolResult;
use crate::tool;
use crate::tools::{get_path, ToolContext, ToolDef};
use anyhow::Context;
use serde_json::json;
use std::path::PathBuf;
use tracing::{debug, error, info};

use super::{cli, pcb_export};

// ─── Tool definitions ─────────────────────────────────────────────────────────

pub fn tools() -> Vec<ToolDef> {
    vec![
        tool!(
            "export_manufacturing_package",
            "Generate ALL files needed for PCB fabrication and assembly in one call: \
             Gerbers, drill files, BOM (fab-house format), and pick-and-place positions. \
             Targets a specific fab house (JLCPCB, PCBWay, etc.). JLCPCB assembly requires the exact board open in KiCad with IPC enabled; native pad midpoints and board exports use one stable live snapshot.",
            json!({
                "type": "object",
                "properties": {
                    "board": { "type": "string", "description": "Path to .kicad_pcb file" },
                    "schematic": { "type": "string", "description": "Path to .kicad_sch file (for BOM generation)" },
                    "output_dir": { "type": "string", "description": "Directory to write all output files" },
                    "fab_house": {
                        "type": "string",
                        "description": "Target manufacturer: 'jlcpcb' (default), 'pcbway', 'oshpark', 'generic'",
                        "default": "jlcpcb"
                    },
                    "include_assembly": {
                        "type": "boolean",
                        "description": "Include BOM + pick-and-place files for SMT assembly",
                        "default": true
                    },
                    "bom_fields": {
                        "type": "string",
                        "description": "Ordered, comma-separated BOM columns, e.g. 'Reference,Value,Footprint,MPN,${QUANTITY}'. Any schematic field name works — this is how MPN/LCSC columns reach the fab. Omit for KiCAD's default Reference,Value,Footprint,QUANTITY,DNP."
                    },
                    "bom_labels": {
                        "type": "string",
                        "description": "Ordered, comma-separated BOM column headings matching 'bom_fields'. Omit to label each column with its field name."
                    },
                    "bom_group_by": {
                        "type": "string",
                        "description": "Comma-separated fields whose matching references collapse into one BOM row, e.g. 'Value,Footprint'."
                    },
                    "gerber_layers": {
                        "type": "array",
                        "items": { "type": "string" },
                        "description": "Exact Gerber layers. Omit or pass [] to auto-select enabled copper, F/B.Mask, F/B.SilkS, and Edge.Cuts while excluding documentation layers."
                    },
                    "position_side": {
                        "type": "string",
                        "enum": ["front", "back", "both"],
                        "description": "Board side(s) in the assembly position file.",
                        "default": "both"
                    },
                    "position_units": {
                        "type": "string",
                        "enum": ["mm", "in"],
                        "description": "Coordinate units in the assembly position file. JLCPCB assembly requires 'mm'.",
                        "default": "mm"
                    },
                    "jlcpcb_cpl_corrections_path": {
                        "type": "string",
                        "description": "Optional path to a versioned JLCPCB CPL correction-policy JSON file. Project footprint rules override built-in rules, and exact designator overrides take highest precedence. Used only when fab_house is 'jlcpcb'."
                    }
                },
                "required": ["board", "output_dir"]
            }),
            |args, ctx| async move { handle_export_manufacturing_package(args, ctx).await }
        ),
        tool!(
            "validate_for_manufacturing",
            "Pre-flight check before ordering: verifies the design is ready for the target \
             fab house. Runs KiCad's DRC and checks board outline, design rules, footprints, \
             and routing evidence. Returns NOT READY — never READY — if \
             DRC reports errors or could not be run, so a READY verdict always rests on \
             evidence rather than on an absence of findings.",
            json!({
                "type": "object",
                "properties": {
                    "board": { "type": "string", "description": "Path to .kicad_pcb file" },
                    "fab_house": {
                        "type": "string",
                        "description": "Target manufacturer: 'jlcpcb', 'pcbway', 'oshpark'",
                        "default": "jlcpcb"
                    }
                },
                "required": ["board"]
            }),
            |args, ctx| async move { handle_validate_for_manufacturing(args, ctx).await }
        ),
        tool!(
            "estimate_cost",
            "Estimate the total manufacturing cost for PCB fabrication and assembly at a given fab house. \
             Counts the parts the board's BOM lists and returns an itemized rough estimate: \
             PCB, components, assembly, and total. For jlcpcb, parts with an LCSC field are priced \
             from the local JLCPCB catalog at the price break for the order's quantity, plus the \
             fee per unique Extended part; the response lists which parts were priced from the \
             catalog and which at an average.",
            json!({
                "type": "object",
                "properties": {
                    "board": { "type": "string", "description": "Path to .kicad_pcb file" },
                    "fab_house": {
                        "type": "string",
                        "description": "'jlcpcb' (default), 'pcbway'",
                        "default": "jlcpcb"
                    },
                    "quantity": {
                        "type": "integer",
                        "description": "Number of boards to manufacture",
                        "default": 5
                    },
                    "layers": {
                        "type": "integer",
                        "description": "Copper layer count to quote at (2, 4, 6). Defaults to the count the board file declares; when given and different, the response reports both under board.copper_layers / board.board_copper_layers and warns."
                    }
                },
                "required": ["board"]
            }),
            |args, ctx| async move { handle_estimate_cost(args, ctx).await }
        ),
    ]
}

// ─── Handlers ─────────────────────────────────────────────────────────────────

async fn handle_export_manufacturing_package(
    args: &serde_json::Value,
    ctx: &ToolContext,
) -> anyhow::Result<CallToolResult> {
    let board = get_path(args, "board")?;
    let output_dir = get_path(args, "output_dir")?;
    let fab_house = args["fab_house"].as_str().unwrap_or("jlcpcb");
    let include_assembly = args["include_assembly"].as_bool().unwrap_or(true);
    let assembly = crate::fab::profile(fab_house).assembly;
    let schematic = args["schematic"].as_str().map(PathBuf::from);
    let requested_gerber_layers = match pcb_export::optional_string_array(args, "gerber_layers") {
        Ok(layers) => layers,
        Err(error) => return Ok(error),
    };
    let position_side = args["position_side"].as_str().unwrap_or("both");
    let position_units = args["position_units"].as_str().unwrap_or("mm");
    let jlcpcb_cpl_corrections_path = if args.get("jlcpcb_cpl_corrections_path").is_some() {
        Some(get_path(args, "jlcpcb_cpl_corrections_path")?)
    } else {
        None
    };
    if let Err((field, reason)) =
        pcb_export::validate_position_values("csv", position_side, position_units)
    {
        let public_field = match field {
            "side" => "position_side",
            "units" => "position_units",
            other => other,
        };
        return Ok(invalid_manufacturing_argument(public_field, reason));
    }
    if let Err((field, reason)) = assembly.check_arguments(
        include_assembly,
        position_units,
        jlcpcb_cpl_corrections_path.is_some(),
    ) {
        return Ok(invalid_manufacturing_argument(field, reason));
    }

    // Establish native geometry before creating/publishing any output. Keep the
    // staging owner alive for every CLI board export, not just CPL.
    let snapshot = match assembly.capture(ctx, &board, include_assembly).await? {
        Ok(snapshot) => snapshot,
        Err(error) => return Ok(midpoint_failure(error)),
    };
    let gerber_layers = if requested_gerber_layers.is_empty() {
        let board_source = match &snapshot {
            Some(snapshot) => snapshot.contents.clone(),
            None => tokio::fs::read_to_string(&board).await?,
        };
        pcb_export::standard_gerber_layers(&board_source)?
    } else {
        requested_gerber_layers
    };
    let snapshot_directory = if snapshot.is_some() {
        Some(tempfile::tempdir()?)
    } else {
        None
    };
    let export_board = if let (Some(snapshot), Some(directory)) = (&snapshot, &snapshot_directory) {
        let staged = directory
            .path()
            .join(board.file_name().context("board missing filename")?);
        tokio::fs::write(&staged, &snapshot.contents).await?;
        let project = board.with_extension("kicad_pro");
        if project.try_exists()? {
            tokio::fs::copy(project, staged.with_extension("kicad_pro")).await?;
        }
        staged
    } else {
        board.clone()
    };

    info!(
        board = %board.display(),
        output_dir = %output_dir.display(),
        fab_house = %fab_house,
        include_assembly = include_assembly,
        "[BETA] Generating manufacturing package"
    );

    tokio::fs::create_dir_all(&output_dir).await?;

    let cli_path = &ctx.config.kicad_cli;
    let mut files_generated = Vec::new();
    let mut verified_paths = Vec::new();
    let mut warnings = Vec::new();
    let mut cpl_designators = None;
    let mut bom_designators = None;
    let mut cpl_orientation_evidence = None;

    // 1. Export Gerbers
    let gerber_dir = output_dir.join("gerbers");
    tokio::fs::create_dir_all(&gerber_dir).await?;
    let gerber_layer_refs = gerber_layers.iter().map(String::as_str).collect::<Vec<_>>();
    match cli::export_gerber(cli_path, &export_board, &gerber_dir, &gerber_layer_refs).await {
        Ok(gerber_files) => {
            info!(files = gerber_files.len(), "[BETA] Gerber export succeeded");
            verified_paths.extend(gerber_files.iter().cloned());
            files_generated.push(json!({
                "type": "gerber",
                "path": gerber_dir.to_str().unwrap_or(""),
                "layers": gerber_layers.clone(),
                "files": gerber_files.iter().map(|path| path.to_str().unwrap_or("")).collect::<Vec<_>>()
            }));
        }
        Err(e) => {
            error!(error = %e, "[BETA] Gerber export failed");
            warnings.push(format!("Gerber export failed: {}", e));
        }
    }

    // 2. Export drill files, into the gerber directory so a fab receives the
    //    plated and non-plated Excellon files alongside the layers they belong
    //    to. `--output` is a directory; the old `output_dir.join("drill.drl")`
    //    made KiCad create a directory named drill.drl, so the package
    //    advertised a "drill" file that was really an empty-looking folder and
    //    the real Excellon output never appeared in the file list at all.
    match cli::export_drill(cli_path, &export_board, &gerber_dir).await {
        Ok(drill_files) => {
            info!(files = drill_files.len(), "[BETA] Drill export succeeded");
            verified_paths.extend(drill_files.iter().cloned());
            for file in &drill_files {
                files_generated.push(json!({
                    "type": "drill",
                    "path": file.to_str().unwrap_or("")
                }));
            }
        }
        Err(e) => {
            error!(error = %e, "[BETA] Drill export failed");
            warnings.push(format!("Drill export failed: {e}"));
        }
    }

    // 3. Assembly files (BOM + pick-and-place)
    if include_assembly {
        // Pick-and-place (position file). KiCad's native CSV is retained for
        // generic callers. JLCPCB receives a structurally parsed CPL with the
        // exact vendor column contract instead of a header text replacement.
        let project_name = board
            .file_stem()
            .and_then(|stem| stem.to_str())
            .unwrap_or("board");
        let pos_path = output_dir.join(assembly.positions_file_name(project_name));
        let position_result = assembly
            .export_positions(
                cli_path,
                &board,
                &export_board,
                &pos_path,
                position_side,
                position_units,
                jlcpcb_cpl_corrections_path.as_deref(),
                snapshot.as_ref(),
            )
            .await;
        match position_result {
            Ok(export) => {
                info!("[BETA] Position file export succeeded");
                if let Some(export) = export {
                    cpl_designators = Some(export.designators);
                    cpl_orientation_evidence = Some(export.orientation_evidence.clone());
                }
                verified_paths.push(pos_path.clone());
                let mut generated = json!({
                    "type": "pick_and_place",
                    "path": pos_path.to_str().unwrap_or(""),
                    "format": "csv",
                    "units": position_units,
                    "side": position_side
                });
                if let Some(evidence) = &cpl_orientation_evidence {
                    generated["placement_orientation"] = json!(evidence);
                }
                files_generated.push(generated);
            }
            Err(e) => {
                error!(error = %e, "[BETA] Position file export failed");
                warnings.push(format!("Position file export failed: {e:#}"));
            }
        }

        // BOM
        if let Some(ref sch) = schematic {
            let bom_path = output_dir.join(assembly.bom_file_name(project_name));
            // Without bom_fields the package gets kicad-cli's fixed
            // Reference,Value,Footprint,QUANTITY,DNP set — no MPN, no supplier
            // part number, nothing a fab can source a part from.
            let bom_options = cli::BomOptions {
                fields: args["bom_fields"].as_str(),
                labels: args["bom_labels"].as_str(),
                group_by: args["bom_group_by"].as_str(),
                exclude_dnp: assembly.bom_excludes_dnp(),
            };
            let bom_result = assembly
                .export_bom(cli_path, sch, &bom_path, &bom_options)
                .await;
            match bom_result {
                Ok(()) => {
                    let parsed = assembly
                        .bom_designators(&bom_path, cpl_designators.as_ref())
                        .await?;
                    match parsed {
                        Ok(designators) => {
                            info!("[BETA] BOM export succeeded");
                            bom_designators = designators;
                            verified_paths.push(bom_path.clone());
                            files_generated.push(json!({
                                "type": "bom",
                                "path": bom_path.to_str().unwrap_or(""),
                                "format": "csv",
                                "fields": bom_options.fields
                            }));
                        }
                        Err(e) => {
                            error!(error = %e, "[BETA] JLCPCB BOM validation failed");
                            warnings.push(format!("JLCPCB BOM validation failed: {e:#}"));
                        }
                    }
                }
                Err(e) => {
                    error!(error = %e, "[BETA] BOM export failed");
                    warnings.push(format!("BOM export failed: {}", e));
                }
            }
        } else {
            warnings.push("No schematic provided — BOM not generated. Pass 'schematic' for full assembly package.".to_string());
        }

        if let (Some(cpl), Some(bom)) = (&cpl_designators, &bom_designators) {
            if let Some(mismatch) = assembly.designator_mismatch(cpl, bom) {
                warnings.push(mismatch);
            }
        }
    }

    // Derive the public file list only from artifacts the CLI boundary already
    // verified as regular and non-empty. A stale or empty directory entry can
    // no longer make an incomplete package look successful (#252).
    let mut all_files = verified_paths
        .iter()
        .map(|path| {
            path.strip_prefix(&output_dir)
                .unwrap_or(path)
                .to_string_lossy()
                .replace('\\', "/")
        })
        .collect::<Vec<_>>();
    all_files.sort();
    all_files.dedup();

    let complete = warnings.is_empty();

    let summary = format!(
        "{} for {}. {} verified non-empty files. {}",
        if complete {
            "Complete package"
        } else {
            "INCOMPLETE package"
        },
        fab_house.to_uppercase(),
        all_files.len(),
        if warnings.is_empty() {
            "No warnings.".to_string()
        } else {
            format!("{} warnings.", warnings.len())
        }
    );

    info!(
        complete = complete,
        files = all_files.len(),
        warnings = warnings.len(),
        "[BETA] Manufacturing package finished"
    );

    let next_steps = if complete {
        let upload = format!(
                "Upload only the verified paths listed in `files` from {} to {}'s order page. Gerbers go in the PCB order; BOM and CPL/positions go in the assembly order.",
                output_dir.display(),
                fab_house.to_uppercase()
            );
        match assembly.after_upload().filter(|_| include_assembly) {
            Some(then) => format!("{upload} {then}"),
            None => upload,
        }
    } else {
        "Do not upload this package. Resolve every warning and export again.".to_string()
    };
    let body = serde_json::to_string(&json!({
        "complete": complete,
        "fab_house": fab_house,
        "output_dir": output_dir.to_str().unwrap_or(""),
        "files": all_files,
        "files_generated": files_generated,
        "gerber_layers": gerber_layers,
        "position_units": if include_assembly { Some(position_units) } else { None },
        "position_side": if include_assembly { Some(position_side) } else { None },
        "placement_orientation": cpl_orientation_evidence,
        "board_source": if snapshot.is_some() { "stable_live_ipc_snapshot" } else { "saved_file" },
        "project_settings_source": "saved_kicad_pro_if_present",
        "warnings": warnings,
        "summary": summary,
        "next_steps": next_steps
    }))
    .unwrap();
    Ok(if complete {
        CallToolResult::text(body)
    } else {
        CallToolResult::error(body)
    })
}

fn midpoint_failure(error: copperpour_ipc::IpcFailure) -> CallToolResult {
    use crate::mcp::error::ToolErrorKind;
    use copperpour_ipc::IpcFailure;
    let message = format!("JLCPCB assembly requires native midpoint geometry from the exact board open in KiCad with IPC enabled; no package was exported. {error}");
    match error {
        IpcFailure::Target { error, .. } => crate::tools::ipc_target_error_result(&error),
        IpcFailure::Unreachable(reason) => CallToolResult::error_kind(
            ToolErrorKind::EditorUnavailable {
                editor: "pcbnew".into(),
                reason,
            },
            message,
        ),
        // This path only reads; an unanswered read does not imply a mutation.
        other => CallToolResult::error_kind(
            ToolErrorKind::HandlerError {
                reason: format!("native_midpoint_unavailable: {other}"),
            },
            message,
        ),
    }
}

fn invalid_manufacturing_argument(field: &str, reason: impl Into<String>) -> CallToolResult {
    let reason = reason.into();
    CallToolResult::error_kind(
        crate::mcp::error::ToolErrorKind::InvalidArgument {
            field: field.to_string(),
            reason: reason.clone(),
        },
        format!("Argument '{field}' is invalid: {reason}"),
    )
}

async fn handle_validate_for_manufacturing(
    args: &serde_json::Value,
    ctx: &ToolContext,
) -> anyhow::Result<CallToolResult> {
    let board = get_path(args, "board")?;
    let fab_house = args["fab_house"].as_str().unwrap_or("jlcpcb");

    info!(
        board = %board.display(),
        fab_house = %fab_house,
        "[BETA] Running manufacturing validation"
    );

    let content = tokio::fs::read_to_string(&board).await?;
    let tree = copperpour_sexp::parser::parse_sexp(&content)?;

    let mut issues = Vec::new();

    // Check board outline
    let has_outline = content.contains("Edge.Cuts");
    if !has_outline {
        issues.push(json!({
            "severity": "error",
            "issue": "No board outline found on Edge.Cuts layer",
            "fix": "Add a board outline using add_board_outline before ordering"
        }));
    }

    // Check that footprints exist
    let fp_count = tree.find_all("footprint").len();
    if fp_count == 0 {
        issues.push(json!({
            "severity": "error",
            "issue": "No footprints found on the board",
            "fix": "Open the PCB in KiCAD and run Tools > Update PCB from Schematic (kicad-cli has no 'pcb sync' command)"
        }));
    }

    // Check layer count — structurally, from the `(layers …)` table, through
    // the same function `get_board_info` uses (#461). Counting the substring
    // `signal)` missed every `power`/`mixed`/`jumper` copper layer.
    let copper_layers = copperpour_sexp::layers::copper_layer_count(&tree);
    debug!(
        copper_layers = copper_layers,
        "[BETA] Detected copper layers"
    );

    // Fab-specific checks
    let min_trace = crate::fab::profile(fab_house).min_trace_mm;

    // Check design rules
    if let Some(min_tw) = find_setup_value(&content, "min_trace_width") {
        if min_tw < min_trace {
            issues.push(json!({
                "severity": "error",
                "issue": format!("Trace width {:.3}mm is below {}'s minimum ({:.3}mm)", min_tw, fab_house, min_trace),
                "fix": format!("Increase minimum trace width to {:.3}mm in design rules", min_trace)
            }));
        }
    }

    // Check for unrouted nets (ratsnest)
    let (net_count, track_count) = count_nets_and_tracks(&tree);
    if net_count > 3 && track_count == 0 {
        issues.push(json!({
            "severity": "error",
            "issue": format!("{} nets defined but no traces routed", net_count),
            "fix": "Route traces using route_trace before manufacturing"
        }));
    }

    // That heuristic only fires on a board with *zero* tracks, so a board
    // routed except for one net sailed past it — and nothing here had ever
    // consulted DRC, which is the only thing that actually knows. This tool
    // returned READY on a board with 25 DRC errors and an unrouted item
    // (#247). A readiness verdict now requires the evidence.
    let drc = cli::run_drc(&ctx.config.kicad_cli, &board, false).await;
    let drc_summary = match &drc {
        Ok(report) => {
            for violation in report.all().filter(|v| v.severity == "error") {
                issues.push(json!({
                    "severity": "error",
                    "issue": format!("DRC [{}]: {}", violation.rule, violation.description),
                    "fix": "Fix in the PCB editor, or waive the rule deliberately; \
                            run_drc lists every violation with its location"
                }));
            }
            for missing in report.missing_categories() {
                // Two different fixes hide behind a null parity count: an
                // older kicad-cli, or no schematic beside the board (#516).
                let (issue, fix) = match (missing, &report.schematic_parity_diagnostic) {
                    ("schematic_parity", Some(reason)) => (
                        format!("DRC schematic parity was not checked: {reason}"),
                        "Readiness cannot be established without it; save the project's \
                         schematic beside the board (same file stem) so kicad-cli can \
                         compare them, then re-run",
                    ),
                    _ => (
                        format!("kicad-cli did not report DRC '{missing}'"),
                        "Readiness cannot be established without it; check the \
                         kicad-cli version",
                    ),
                };
                issues.push(json!({
                    "severity": "error",
                    "issue": issue,
                    "fix": fix
                }));
            }
            json!({
                "errors": report.error_count(),
                "design_rule_violations": report.violations.len(),
                "unconnected_items": report.unconnected_items.as_ref().map(Vec::len),
                "schematic_parity": report.schematic_parity.as_ref().map(Vec::len),
            })
        }
        Err(error) => {
            issues.push(json!({
                "severity": "error",
                "issue": format!("DRC could not run: {error:#}"),
                "fix": "Without DRC this tool cannot tell a clean board from a \
                        broken one; fix the kicad-cli path and re-run"
            }));
            serde_json::Value::Null
        }
    };

    let verdict = if issues.iter().any(|i| i["severity"] == "error") {
        "NOT READY"
    } else if !issues.is_empty() {
        "NEEDS REVIEW"
    } else {
        "READY"
    };

    info!(
        verdict = verdict,
        issues = issues.len(),
        "[BETA] Manufacturing validation complete"
    );

    Ok(CallToolResult::text(
        serde_json::to_string(&json!({
            "verdict": verdict,
            "fab_house": fab_house,
            "board_info": {
                "footprint_count": fp_count,
                "copper_layers": copper_layers,
                "net_count": net_count,
                "track_count": track_count
            },
            // Null means DRC did not run, and an issue above says so. Never a
            // zeroed-out object: this tool must not be able to imply a clean
            // board it never checked.
            "drc": drc_summary,
            "issues": issues,
            "summary": format!(
                "{}: {} issues found. {} footprints, {} copper layers.",
                verdict, issues.len(), fp_count, copper_layers
            )
        }))
        .unwrap(),
    ))
}

/// The board's footprints that a BOM lists, and the references of those it
/// leaves out (`exclude_from_bom` or `dnp`), which are not bought.
fn bom_footprints(
    tree: &copperpour_sexp::SexpNode,
) -> (Vec<crate::fab::BomFootprint>, Vec<String>) {
    let property = |footprint: &copperpour_sexp::SexpNode, name: &str| {
        footprint
            .find_all("property")
            .into_iter()
            .find(|property| {
                property.get(1).and_then(copperpour_sexp::SexpNode::as_str) == Some(name)
            })
            .and_then(|property| property.get(2))
            .and_then(copperpour_sexp::SexpNode::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty() && *value != "~")
            .map(str::to_string)
    };
    let mut listed = Vec::new();
    let mut excluded = Vec::new();
    for footprint in tree.find_all("footprint") {
        // KiCad 7 and older keep the reference in `fp_text reference`.
        let reference = property(footprint, "Reference")
            .or_else(|| {
                footprint
                    .find_all("fp_text")
                    .into_iter()
                    .find(|text| {
                        text.get(1).and_then(copperpour_sexp::SexpNode::as_str) == Some("reference")
                    })
                    .and_then(|text| text.get(2))
                    .and_then(copperpour_sexp::SexpNode::as_str)
                    .map(str::to_string)
            })
            .unwrap_or_else(|| "?".to_string());
        let left_out = footprint.find("attr").is_some_and(|attr| {
            attr.children()
                .unwrap_or(&[])
                .iter()
                .any(|flag| matches!(flag.as_str(), Some("exclude_from_bom" | "dnp")))
        });
        if left_out {
            excluded.push(reference);
        } else {
            listed.push(crate::fab::BomFootprint {
                reference,
                lcsc: property(footprint, "LCSC"),
            });
        }
    }
    (listed, excluded)
}

async fn handle_estimate_cost(
    args: &serde_json::Value,
    ctx: &ToolContext,
) -> anyhow::Result<CallToolResult> {
    let board = get_path(args, "board")?;
    let fab_house = args["fab_house"].as_str().unwrap_or("jlcpcb");
    let quantity = args["quantity"].as_u64().unwrap_or(5) as usize;

    info!(
        board = %board.display(),
        fab_house = %fab_house,
        quantity = quantity,
        "[BETA] Estimating manufacturing cost"
    );

    let content = tokio::fs::read_to_string(&board).await?;
    let tree = copperpour_sexp::parser::parse_sexp(&content)?;

    // Count components
    let fps = tree.find_all("footprint");
    let component_count = fps.len();

    // The board's own copper count comes from its `(layers …)` table, through
    // the same function `get_board_info` uses (#461). A caller may still quote
    // at a different count, but the quote then says so instead of presenting
    // the requested number as the board's.
    let board_copper_layers = copperpour_sexp::layers::copper_layer_count(&tree);
    let requested_layers = args["layers"].as_u64().map(|n| n as usize);
    let copper_layers = requested_layers.unwrap_or(board_copper_layers);
    let mut warnings: Vec<String> = Vec::new();
    match requested_layers {
        Some(requested) if requested != board_copper_layers => warnings.push(format!(
            "quoted at {requested} copper layers, but the board file declares \
             {board_copper_layers}; this estimate does not describe the board as saved"
        )),
        _ => {}
    }
    if board_copper_layers == 0 {
        warnings.push(
            "the board file declares no copper layers (no `(layers …)` table was read); \
             the layer count could not be taken from the board"
                .to_string(),
        );
    }

    // Estimate board dimensions from Edge.Cuts
    let (width_mm, height_mm) = estimate_board_dimensions(&content);

    let (bom_parts, excluded_from_bom) = bom_footprints(&tree);
    // Only the parts the BOM lists are bought and placed; a mounting hole or a
    // `dnp` part is not.
    let placed_count = bom_parts.len();
    let cost_model = crate::fab::profile(fab_house).cost_model;
    let catalog_pricing = match cost_model.components {
        crate::fab::ComponentPrice::JlcpcbCatalog {
            average,
            extended_part_fee,
            fee_source,
        } => Some((
            crate::fab::jlcpcb::pricing::price_jlcpcb_components(
                args,
                ctx,
                &bom_parts,
                average,
                quantity as u64,
            )
            .await,
            average,
            extended_part_fee,
            fee_source,
        )),
        _ => None,
    };

    // Rough cost estimation based on fab house pricing models
    let pcb_cost = cost_model.pcb.cost(copper_layers, quantity);
    let assembly_cost = cost_model
        .assembly
        .as_ref()
        .map_or(0.0, |assembly| assembly.cost(placed_count, quantity));
    let component_est = match (&cost_model.components, &catalog_pricing) {
        (_, Some((pricing, ..))) => pricing.per_board * quantity as f64,
        (crate::fab::ComponentPrice::PerPart(price), None) => {
            placed_count as f64 * price * quantity as f64
        }
        _ => 0.0,
    };

    // Charged once per order for each unique Extended part, not per board.
    let extended_parts_fee = catalog_pricing
        .as_ref()
        .map_or(0.0, |(pricing, _, fee, _)| {
            pricing.extended_unique_parts as f64 * fee
        });
    let total = pcb_cost + assembly_cost + component_est + extended_parts_fee;

    let mut notes = vec![
        "Estimates are approximate — actual cost depends on board size, finish, and specific components"
            .to_string(),
    ];
    let approximate = cost_model.source.is_none();
    match cost_model.source {
        Some(source) => notes.push(source.to_string()),
        None => notes.push(format!(
            "{}'s PCB and assembly prices here are rough figures with no cited source",
            fab_house.to_uppercase()
        )),
    }
    let components = match &catalog_pricing {
        Some((pricing, average, _, fee_source)) => {
            notes.push(format!(
                "Parts with an LCSC field are priced from the local JLCPCB catalog, at the price break \
                 for the order's quantity of each; other parts at an average of \
                 ${average:.2}. components lists which is which; search_jlcpcb_parts \
                 finds an LCSC number for a part."
            ));
            notes.push(fee_source.to_string());
            json!({
                "catalog": pricing.catalog,
                "priced_from_catalog": pricing.priced_from_catalog,
                "estimated": pricing.estimated,
                "average_unit_price": average,
                "extended_unique_parts": pricing.extended_unique_parts,
                "excluded_from_bom": excluded_from_bom
            })
        }
        None => {
            notes.push(format!(
                "Component costs are rough averages per footprint; catalog prices are used only for jlcpcb, not {fab_house}"
            ));
            json!({ "priced_from_catalog": [], "excluded_from_bom": excluded_from_bom })
        }
    };
    notes.push(format!(
        "Based on {} quantity from {}",
        quantity,
        fab_house.to_uppercase()
    ));

    debug!(
        pcb_cost = pcb_cost,
        assembly_cost = assembly_cost,
        component_est = component_est,
        total = total,
        "[BETA] Cost estimate calculated"
    );

    Ok(CallToolResult::text(
        serde_json::to_string(&json!({
            "fab_house": fab_house,
            "quantity": quantity,
            "board": {
                "width_mm": width_mm,
                "height_mm": height_mm,
                // What this estimate was priced at, and what the board itself
                // declares. They differ only when the caller passed `layers`.
                "copper_layers": copper_layers,
                "board_copper_layers": board_copper_layers,
                "component_count": component_count
            },
            "warnings": warnings,
            "components": components,
            "cost_estimate": {
                "pcb_fabrication": format!("${:.2}", pcb_cost),
                "smt_assembly": format!("${:.2}", assembly_cost),
                "components_estimate": format!("${:.2}", component_est),
                "extended_parts_fee": format!("${:.2}", extended_parts_fee),
                "total_estimate": format!("${:.2}", total),
                "per_board": format!("${:.2}", total / quantity as f64),
                // The PCB and assembly prices have no cited source (#209).
                "approximate": approximate
            },
            "notes": notes,
            "disclaimer": "BETA: Cost estimates are indicative only. Always confirm with the fab house's online quoting tool."
        }))
        .unwrap(),
    ))
}

#[cfg(test)]
mod package_export_option_tests {
    use super::*;

    fn result_json(result: &CallToolResult) -> serde_json::Value {
        match result.content.first() {
            Some(crate::mcp::protocol::ToolContent::Text { text }) => {
                serde_json::from_str(text).unwrap()
            }
            other => panic!("expected text result, got {other:?}"),
        }
    }

    #[test]
    fn package_schema_exposes_applied_gerber_and_position_options() {
        let package = tools()
            .into_iter()
            .find(|tool| tool.name == "export_manufacturing_package")
            .unwrap();
        let properties = &package.input_schema["properties"];
        assert_eq!(properties["gerber_layers"]["items"]["type"], "string");
        assert_eq!(properties["position_units"]["enum"], json!(["mm", "in"]));
        assert_eq!(
            properties["position_side"]["enum"],
            json!(["front", "back", "both"])
        );
        assert_eq!(properties["jlcpcb_cpl_corrections_path"]["type"], "string");
    }

    #[tokio::test]
    async fn package_is_an_error_when_cli_success_produces_no_artifacts() {
        let dir = tempfile::tempdir().unwrap();
        let board = dir.path().join("live_ipc.kicad_pcb");
        // Real KiCad 9 output, as required for tests that parse board layers.
        std::fs::write(
            &board,
            include_str!("../../../copperpour-ipc/tests/fixtures/live_ipc.kicad_pcb"),
        )
        .unwrap();
        let cli = crate::tools::cli::test_support::noop_cli(dir.path());
        let ctx = ToolContext::new(
            crate::tools::ServerConfig {
                kicad_cli: cli.display().to_string(),
                kicad_binary: String::new(),
                ipc_address: String::new(),
                project_dir: None,
                jlcpcb_db_path: None,
                auto_load_toolsets: false,
                eager_toolsets: false,
            },
            std::sync::Arc::new(crate::router::ToolRouter::new()),
        );

        let result = handle_export_manufacturing_package(
            &json!({
                "board": board.display().to_string(),
                "output_dir": dir.path().join("package").display().to_string(),
                "include_assembly": false
            }),
            &ctx,
        )
        .await
        .unwrap();
        assert!(result.is_error, "incomplete package must fail closed");
        let body = result_json(&result);
        assert_eq!(body["complete"], false);
        assert_eq!(body["files"], json!([]));
        assert!(body["warnings"].as_array().unwrap().len() >= 2, "{body}");
        assert!(body["next_steps"]
            .as_str()
            .unwrap()
            .starts_with("Do not upload"));
    }

    #[tokio::test]
    async fn jlcpcb_assembly_rejects_non_metric_positions_before_writing_output() {
        let dir = tempfile::tempdir().unwrap();
        let board = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/specctra_two_resistors_locked.kicad_pcb");
        let output = dir.path().join("package");
        let ctx = ToolContext::new(
            crate::tools::ServerConfig {
                kicad_cli: String::new(),
                kicad_binary: String::new(),
                ipc_address: String::new(),
                project_dir: None,
                jlcpcb_db_path: None,
                auto_load_toolsets: false,
                eager_toolsets: false,
            },
            std::sync::Arc::new(crate::router::ToolRouter::new()),
        );

        let result = handle_export_manufacturing_package(
            &json!({
                "board": board,
                "output_dir": output,
                "fab_house": "jlcpcb",
                "include_assembly": true,
                "position_units": "in"
            }),
            &ctx,
        )
        .await
        .unwrap();

        assert!(result.is_error);
        assert!(!output.exists(), "invalid request must not create output");
        let text = match result.content.first() {
            Some(crate::mcp::protocol::ToolContent::Text { text }) => text,
            other => panic!("expected text result, got {other:?}"),
        };
        assert!(text.contains("must use millimetres"));
    }

    #[tokio::test]
    async fn jlcpcb_policy_is_rejected_when_the_export_cannot_apply_it() {
        let dir = tempfile::tempdir().unwrap();
        let board = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/specctra_two_resistors_locked.kicad_pcb");
        let output = dir.path().join("package");
        let policy = dir.path().join("policy.json");
        std::fs::write(&policy, "{}").unwrap();
        let ctx = ToolContext::new(
            crate::tools::ServerConfig {
                kicad_cli: String::new(),
                kicad_binary: String::new(),
                ipc_address: String::new(),
                project_dir: None,
                jlcpcb_db_path: None,
                auto_load_toolsets: false,
                eager_toolsets: false,
            },
            std::sync::Arc::new(crate::router::ToolRouter::new()),
        );

        let result = handle_export_manufacturing_package(
            &json!({
                "board": board,
                "output_dir": output,
                "fab_house": "generic",
                "include_assembly": true,
                "jlcpcb_cpl_corrections_path": policy
            }),
            &ctx,
        )
        .await
        .unwrap();

        assert!(result.is_error);
        assert!(!output.exists(), "invalid request must not create output");
        let text = match result.content.first() {
            Some(crate::mcp::protocol::ToolContent::Text { text }) => text,
            other => panic!("expected text result, got {other:?}"),
        };
        assert!(text.contains("requires fab_house='jlcpcb'"));
    }
}

// ─── Helpers ─────────────────────────────────────────────────────────────────

/// Distinct nets and routed items on the board, read from the parsed tree.
///
/// Both net shapes have to be handled. KiCad ≤ 9 declares each net once at top
/// level — `(net 1 "GND")` — and items refer to it by number, `(net 1)`. KiCad
/// 10 dropped the top-level table entirely and writes the name on every item,
/// `(net "GND")`. Counting names when any are present covers both without
/// double-counting a KiCad 9 net through its declaration *and* its references.
///
/// Tracks are always direct children of `(kicad_pcb …)`, so they are counted
/// there rather than by walking: `(arc …)` also appears inside `(pts …)` of a
/// zone outline, which is not routed copper.
fn count_nets_and_tracks(tree: &copperpour_sexp::SexpNode) -> (usize, usize) {
    use std::collections::HashSet;

    fn walk(
        node: &copperpour_sexp::SexpNode,
        names: &mut HashSet<String>,
        ids: &mut HashSet<String>,
    ) {
        let Some(children) = node.children() else {
            return;
        };
        if node.head() == Some("net") {
            match children.last() {
                // (net 1 "GND") and (net "GND") both end in the quoted name.
                Some(copperpour_sexp::SexpNode::Str(name)) if !name.is_empty() => {
                    names.insert(name.clone());
                }
                // (net 1) — a bare reference in a file whose table we have not
                // seen. Net 0 is the unconnected pseudo-net.
                Some(copperpour_sexp::SexpNode::Atom(id)) if id != "0" => {
                    ids.insert(id.clone());
                }
                _ => {}
            }
        }
        for child in children {
            walk(child, names, ids);
        }
    }

    let mut names = HashSet::new();
    let mut ids = HashSet::new();
    walk(tree, &mut names, &mut ids);
    let net_count = if names.is_empty() {
        ids.len()
    } else {
        names.len()
    };

    let track_count =
        tree.find_all("segment").len() + tree.find_all("via").len() + tree.find_all("arc").len();

    (net_count, track_count)
}

fn find_setup_value(content: &str, key: &str) -> Option<f64> {
    let pat = format!("({} ", key);
    let pos = content.find(&pat)?;
    let after = &content[pos + pat.len()..];
    let end = after.find(')')?;
    after[..end].trim().parse().ok()
}

fn estimate_board_dimensions(content: &str) -> (f64, f64) {
    let mut min_x = f64::MAX;
    let mut max_x = f64::MIN;
    let mut min_y = f64::MAX;
    let mut max_y = f64::MIN;
    let mut found = false;

    // Scan gr_line on Edge.Cuts for board outline coordinates
    let mut pos = 0;
    while let Some(line_pos) = content[pos..].find("(gr_line") {
        let abs = pos + line_pos;
        let block_end = content[abs..].find(")\n").unwrap_or(300) + abs;
        let block = &content[abs..block_end.min(content.len())];

        if block.contains("Edge.Cuts") {
            // Extract start and end coordinates
            if let (Some(sx), Some(sy)) = (
                extract_coord(block, "start", 0),
                extract_coord(block, "start", 1),
            ) {
                if sx < min_x {
                    min_x = sx;
                }
                if sx > max_x {
                    max_x = sx;
                }
                if sy < min_y {
                    min_y = sy;
                }
                if sy > max_y {
                    max_y = sy;
                }
                found = true;
            }
            if let (Some(ex), Some(ey)) = (
                extract_coord(block, "end", 0),
                extract_coord(block, "end", 1),
            ) {
                if ex < min_x {
                    min_x = ex;
                }
                if ex > max_x {
                    max_x = ex;
                }
                if ey < min_y {
                    min_y = ey;
                }
                if ey > max_y {
                    max_y = ey;
                }
            }
        }
        pos = abs + 1;
    }

    if found {
        ((max_x - min_x).abs(), (max_y - min_y).abs())
    } else {
        (0.0, 0.0) // Unknown
    }
}

fn extract_coord(block: &str, keyword: &str, index: usize) -> Option<f64> {
    let pat = format!("({} ", keyword);
    let pos = block.find(&pat)? + pat.len();
    let rest = &block[pos..];
    let parts: Vec<&str> = rest.split([' ', ')']).collect();
    parts.get(index)?.parse().ok()
}

#[cfg(test)]
mod net_track_count_tests {
    use super::*;
    use copperpour_sexp::parser::parse_sexp;

    /// KiCad 10 (file format 20260206) writes tab indentation, puts each
    /// `(segment …)` / `(via …)` on its own multi-line form, and has **no**
    /// top-level net table — every item names its net instead. The old
    /// substring probes (`"\n  (net "`, `"(segment "`, `"(via "`) match none of
    /// that, so a fully routed board reported net_count 0 / track_count 0 and
    /// still came back READY.
    const KICAD_10_BOARD: &str = "(kicad_pcb\n\
        \t(version 20260206)\n\
        \t(generator \"pcbnew\")\n\
        \t(segment\n\t\t(start 110 110)\n\t\t(end 120 110)\n\t\t(width 0.2)\n\t\t(layer \"F.Cu\")\n\t\t(net \"GND\")\n\t)\n\
        \t(segment\n\t\t(start 120 110)\n\t\t(end 130 120)\n\t\t(width 0.2)\n\t\t(layer \"F.Cu\")\n\t\t(net \"GND\")\n\t)\n\
        \t(via\n\t\t(at 130 120)\n\t\t(size 0.6)\n\t\t(drill 0.3)\n\t\t(net \"GND\")\n\t)\n\
        \t(segment\n\t\t(start 110 130)\n\t\t(end 120 130)\n\t\t(width 0.2)\n\t\t(layer \"B.Cu\")\n\t\t(net \"VCC\")\n\t)\n\
        )\n";

    /// KiCad ≤ 9: a top-level net table plus numeric references on the items.
    /// The same net must not be counted once for its declaration and again for
    /// every segment that mentions it.
    const KICAD_9_BOARD: &str = "(kicad_pcb\n\
        \t(version 20241229)\n\
        \t(net 0 \"\")\n\
        \t(net 1 \"GND\")\n\
        \t(net 2 \"VCC\")\n\
        \t(segment (start 110 110) (end 120 110) (width 0.2) (layer \"F.Cu\") (net 1))\n\
        \t(segment (start 120 110) (end 130 120) (width 0.2) (layer \"F.Cu\") (net 1))\n\
        \t(via (at 130 120) (size 0.6) (drill 0.3) (layers \"F.Cu\" \"B.Cu\") (net 1))\n\
        \t(segment (start 110 130) (end 120 130) (width 0.2) (layer \"B.Cu\") (net 2))\n\
        )\n";

    #[test]
    fn counts_a_kicad_10_board_with_no_top_level_net_table() {
        let tree = parse_sexp(KICAD_10_BOARD).unwrap();
        assert_eq!(count_nets_and_tracks(&tree), (2, 4));
    }

    #[test]
    fn counts_a_kicad_9_board_without_double_counting_declarations() {
        let tree = parse_sexp(KICAD_9_BOARD).unwrap();
        assert_eq!(count_nets_and_tracks(&tree), (2, 4));
    }

    #[test]
    fn the_unconnected_pseudo_net_does_not_count() {
        let tree = parse_sexp("(kicad_pcb\n\t(net 0 \"\")\n)\n").unwrap();
        assert_eq!(count_nets_and_tracks(&tree), (0, 0));
    }

    /// A zone outline may carry `(arc …)` inside its `(pts …)`; that is a
    /// polygon corner, not routed copper.
    #[test]
    fn zone_outline_arcs_are_not_routed_copper() {
        let board = "(kicad_pcb\n\
            \t(zone\n\t\t(net \"GND\")\n\t\t(polygon\n\t\t\t(pts\n\t\t\t\t(xy 0 0)\n\t\t\t\t(arc (start 1 0) (mid 2 1) (end 1 2))\n\t\t\t)\n\t\t)\n\t)\n\
            )\n";
        let tree = parse_sexp(board).unwrap();
        assert_eq!(count_nets_and_tracks(&tree), (1, 0));
    }

    // ─── End to end through the tool ─────────────────────────────────────────

    fn test_ctx() -> ToolContext {
        ToolContext::new(
            crate::tools::ServerConfig {
                kicad_cli: String::new(),
                kicad_binary: String::new(),
                ipc_address: String::new(),
                project_dir: None,
                jlcpcb_db_path: None,
                auto_load_toolsets: false,
                eager_toolsets: false,
            },
            std::sync::Arc::new(crate::router::ToolRouter::new()),
        )
    }

    async fn validate(board_text: &str) -> serde_json::Value {
        let dir = tempfile::tempdir().unwrap();
        let board = dir.path().join("board.kicad_pcb");
        std::fs::write(&board, board_text).unwrap();
        let result = handle_validate_for_manufacturing(
            &json!({ "board": board.to_str().unwrap() }),
            &test_ctx(),
        )
        .await
        .unwrap();
        match result.content.first() {
            Some(crate::mcp::protocol::ToolContent::Text { text }) => {
                serde_json::from_str(text).unwrap()
            }
            other => panic!("expected text content, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn a_routed_kicad_10_board_reports_its_nets_and_tracks() {
        let report = validate(KICAD_10_BOARD).await;
        assert_eq!(report["board_info"]["net_count"], json!(2));
        assert_eq!(report["board_info"]["track_count"], json!(4));
    }

    /// The symptom that surfaced this: an unrouted board came back READY on the
    /// routing check because both counts read zero, so the `net_count > 3 &&
    /// track_count == 0` guard could never fire.
    #[tokio::test]
    async fn an_unrouted_kicad_10_board_is_flagged_not_ready() {
        let board = "(kicad_pcb\n\
            \t(version 20260206)\n\
            \t(gr_line (start 0 0) (end 10 0) (layer \"Edge.Cuts\"))\n\
            \t(footprint \"R:R_0402\"\n\
            \t\t(pad \"1\" smd rect (at 0 0) (size 1 1) (net \"GND\"))\n\
            \t\t(pad \"2\" smd rect (at 1 0) (size 1 1) (net \"VCC\"))\n\
            \t)\n\
            \t(footprint \"R:R_0402\"\n\
            \t\t(pad \"1\" smd rect (at 5 0) (size 1 1) (net \"SDA\"))\n\
            \t\t(pad \"2\" smd rect (at 6 0) (size 1 1) (net \"SCL\"))\n\
            \t)\n\
            )\n";
        let report = validate(board).await;
        assert_eq!(report["board_info"]["net_count"], json!(4));
        assert_eq!(report["board_info"]["track_count"], json!(0));
        assert_eq!(report["verdict"], json!("NOT READY"));
        let issues = report["issues"].as_array().unwrap();
        assert!(
            issues.iter().any(|i| i["issue"]
                .as_str()
                .unwrap_or("")
                .contains("no traces routed")),
            "{issues:?}"
        );
    }
}

#[cfg(test)]
mod readiness_evidence_tests {
    use super::*;
    use serde_json::json;

    /// No kicad-cli, so DRC cannot run — the point of these tests.
    fn ctx_without_kicad_cli() -> ToolContext {
        ToolContext::new(
            crate::tools::ServerConfig {
                kicad_cli: String::new(),
                kicad_binary: String::new(),
                ipc_address: String::new(),
                project_dir: None,
                jlcpcb_db_path: None,
                auto_load_toolsets: false,
                eager_toolsets: false,
            },
            std::sync::Arc::new(crate::router::ToolRouter::new()),
        )
    }

    async fn validate(board_text: &str) -> serde_json::Value {
        let dir = tempfile::tempdir().unwrap();
        let board = dir.path().join("board.kicad_pcb");
        std::fs::write(&board, board_text).unwrap();
        let result = handle_validate_for_manufacturing(
            &json!({ "board": board.to_str().unwrap() }),
            &ctx_without_kicad_cli(),
        )
        .await
        .unwrap();
        match result.content.first() {
            Some(crate::mcp::protocol::ToolContent::Text { text }) => {
                serde_json::from_str(text).unwrap()
            }
            other => panic!("expected text content, got {other:?}"),
        }
    }

    /// A board that passes every check this tool performs itself.
    const CLEAN_LOOKING_BOARD: &str = "(kicad_pcb\n\
        \t(version 20260206)\n\
        \t(generator \"pcbnew\")\n\
        \t(layers\n\t\t(0 \"F.Cu\" signal)\n\t\t(31 \"B.Cu\" signal)\n\t)\n\
        \t(gr_line (start 0 0) (end 50 0) (layer \"Edge.Cuts\") (width 0.1))\n\
        \t(footprint \"R:R_0402\"\n\
        \t\t(pad \"1\" smd rect (at 5 0) (size 1 1) (net \"SDA\"))\n\
        \t)\n\
        \t(segment (start 0 0) (end 5 0) (width 0.25) (layer \"F.Cu\") (net \"SDA\"))\n\
        )\n";

    /// #247. This tool returned `READY` with zero issues on a board carrying
    /// 25 DRC errors and an unrouted item, because it never asked DRC —
    /// its only routing check fires when a board has *no* tracks at all.
    ///
    /// The test context has no kicad-cli, so DRC cannot run. Missing evidence
    /// must block the verdict: "I found nothing wrong" is not the same claim
    /// as "nothing is wrong", and only one of them justifies ordering boards.
    #[tokio::test]
    async fn readiness_needs_drc_evidence_not_just_an_absence_of_findings() {
        let report = validate(CLEAN_LOOKING_BOARD).await;

        assert_ne!(
            report["verdict"], "READY",
            "a board whose DRC was never run cannot be declared ready: {report}"
        );
        assert!(report["drc"].is_null(), "no DRC ran, so no DRC summary");
        let issues = report["issues"].as_array().unwrap();
        assert!(
            issues.iter().any(|i| i["issue"]
                .as_str()
                .unwrap_or("")
                .contains("DRC could not run")),
            "the missing evidence must be named: {issues:?}"
        );
    }
}

#[cfg(test)]
mod copper_layer_count_tests {
    //! Issue #461: `estimate_cost` and `validate_for_manufacturing` counted
    //! copper layers by finding the substring `signal)` in the file text, so a
    //! board whose inner layers are `power`/`mixed`/`jumper` planes was quoted
    //! as a two-layer board while `get_board_info` on the same file said six.
    //!
    //! The fixture is pcbnew's own serialization of a six-layer board with two
    //! `power` planes and one `mixed` layer — provenance in
    //! `tests/fixtures/six_layer_power_planes_kicad10.README.md`. The old
    //! substring count reads it as 3.

    use super::*;
    use serde_json::json;

    const SIX_LAYER: &str =
        include_str!("../../tests/fixtures/six_layer_power_planes_kicad10.kicad_pcb");

    /// Two copper layers, both `signal` — the control the old scan got right.
    const TWO_LAYER: &str = "(kicad_pcb\n\
        \t(version 20260206)\n\
        \t(generator \"pcbnew\")\n\
        \t(layers\n\t\t(0 \"F.Cu\" signal)\n\t\t(31 \"B.Cu\" signal)\n\t)\n\
        \t(gr_line (start 0 0) (end 50 0) (layer \"Edge.Cuts\") (width 0.1))\n\
        )\n";

    fn ctx() -> ToolContext {
        ToolContext::new(
            crate::tools::ServerConfig {
                kicad_cli: String::new(),
                kicad_binary: String::new(),
                ipc_address: String::new(),
                project_dir: None,
                jlcpcb_db_path: None,
                auto_load_toolsets: false,
                eager_toolsets: false,
            },
            std::sync::Arc::new(crate::router::ToolRouter::new()),
        )
    }

    fn text_of(result: CallToolResult) -> serde_json::Value {
        match result.content.first() {
            Some(crate::mcp::protocol::ToolContent::Text { text }) => {
                serde_json::from_str(text).unwrap()
            }
            other => panic!("expected text content, got {other:?}"),
        }
    }

    async fn estimate(board_text: &str, extra: serde_json::Value) -> serde_json::Value {
        let dir = tempfile::tempdir().unwrap();
        let board = dir.path().join("board.kicad_pcb");
        std::fs::write(&board, board_text).unwrap();
        let mut args =
            json!({ "board": board.to_str().unwrap(), "fab_house": "jlcpcb", "quantity": 5 });
        for (k, v) in extra.as_object().unwrap() {
            args[k] = v.clone();
        }
        text_of(handle_estimate_cost(&args, &ctx()).await.unwrap())
    }

    async fn validate(board_text: &str) -> serde_json::Value {
        let dir = tempfile::tempdir().unwrap();
        let board = dir.path().join("board.kicad_pcb");
        std::fs::write(&board, board_text).unwrap();
        text_of(
            handle_validate_for_manufacturing(&json!({ "board": board.to_str().unwrap() }), &ctx())
                .await
                .unwrap(),
        )
    }

    /// The fixture really does defeat the substring scan; if a future KiCad
    /// resave changed that, this test would be proving nothing.
    #[test]
    fn the_fixture_defeats_a_substring_count() {
        let substring =
            SIX_LAYER.matches("signal)").count() + SIX_LAYER.matches("signal \"").count();
        assert_eq!(substring, 3, "fixture must carry non-signal copper");
        let tree = copperpour_sexp::parser::parse_sexp(SIX_LAYER).unwrap();
        assert_eq!(copperpour_sexp::layers::copper_layer_count(&tree), 6);
    }

    /// The reported case: a six-layer board priced as a six-layer board, with
    /// the quote's count and the board's count agreeing and no warning.
    #[tokio::test]
    async fn estimate_cost_prices_the_boards_declared_copper_count() {
        let report = estimate(SIX_LAYER, json!({})).await;
        assert_eq!(report["board"]["copper_layers"], 6, "{report}");
        assert_eq!(report["board"]["board_copper_layers"], 6);
        assert_eq!(
            report["cost_estimate"]["pcb_fabrication"], "$15.00",
            "JLCPCB six-layer price at quantity 5, not the two-layer $2.00: {report}"
        );
        assert_eq!(report["warnings"], json!([]));

        let control = estimate(TWO_LAYER, json!({})).await;
        assert_eq!(control["board"]["copper_layers"], 2);
        assert_eq!(control["cost_estimate"]["pcb_fabrication"], "$2.00");
    }

    /// A caller may quote at a different count, but the response must say the
    /// board disagrees rather than present the request as the board's fact.
    #[tokio::test]
    async fn estimate_cost_reports_a_layer_override_against_the_board() {
        let report = estimate(SIX_LAYER, json!({ "layers": 2 })).await;
        assert_eq!(report["board"]["copper_layers"], 2, "priced as asked");
        assert_eq!(
            report["board"]["board_copper_layers"], 6,
            "but the board says six"
        );
        assert_eq!(report["cost_estimate"]["pcb_fabrication"], "$2.00");
        let warnings = report["warnings"].as_array().unwrap();
        assert_eq!(warnings.len(), 1, "{report}");
        let text = warnings[0].as_str().unwrap();
        assert!(
            text.contains("quoted at 2") && text.contains("declares 6"),
            "{text}"
        );

        // Asking for the count the board already has is not a discrepancy.
        let agree = estimate(SIX_LAYER, json!({ "layers": 6 })).await;
        assert_eq!(agree["warnings"], json!([]));
    }

    /// No `(layers …)` table: zero and a warning, never an invented two.
    #[tokio::test]
    async fn estimate_cost_does_not_invent_two_layers_for_a_board_without_a_table() {
        let report = estimate(
            "(kicad_pcb (version 20260206) (generator \"pcbnew\"))",
            json!({}),
        )
        .await;
        assert_eq!(report["board"]["copper_layers"], 0);
        assert_eq!(report["board"]["board_copper_layers"], 0);
        assert!(
            report["warnings"][0]
                .as_str()
                .unwrap()
                .contains("no copper layers"),
            "{report}"
        );
    }

    #[tokio::test]
    async fn validate_for_manufacturing_counts_copper_structurally() {
        let report = validate(SIX_LAYER).await;
        assert_eq!(report["board_info"]["copper_layers"], 6, "{report}");
        assert!(report["summary"]
            .as_str()
            .unwrap()
            .contains("6 copper layers"));

        let control = validate(TWO_LAYER).await;
        assert_eq!(control["board_info"]["copper_layers"], 2);
    }
}

#[cfg(test)]
mod estimate_cost_catalog_tests {
    //! Issue #92: `estimate_cost` priced every part at a flat average, added no
    //! fee for Extended parts, and pointed to a `generate_bom` that does not
    //! exist.

    use super::*;
    use serde_json::json;
    use std::path::Path;

    /// Three resistors on one Basic part (R3 saved the KiCad 7 way), an
    /// Extended MCU, a part the catalog lacks, an Extended part with no price,
    /// a part without an LCSC field, two footprints the BOM leaves out, and a
    /// resistor whose price the feed rounds to $0.000 (R4).
    const BOARD: &str = "(kicad_pcb\n\
        \t(version 20260206)\n\
        \t(generator \"pcbnew\")\n\
        \t(layers\n\t\t(0 \"F.Cu\" signal)\n\t\t(31 \"B.Cu\" signal)\n\t)\n\
        \t(footprint \"R_0402\" (layer \"F.Cu\") (attr smd) (property \"Reference\" \"R1\" (at 0 0 0)) (property \"LCSC\" \"C25744\" (at 0 0 0)))\n\
        \t(footprint \"R_0402\" (layer \"F.Cu\") (attr smd) (property \"Reference\" \"R2\" (at 0 0 0)) (property \"LCSC\" \"C25744\" (at 0 0 0)))\n\
        \t(footprint \"QFN\" (layer \"F.Cu\") (attr smd) (property \"Reference\" \"U1\" (at 0 0 0)) (property \"LCSC\" \"C8734\" (at 0 0 0)))\n\
        \t(footprint \"QFN\" (layer \"F.Cu\") (attr smd) (property \"Reference\" \"U2\" (at 0 0 0)) (property \"LCSC\" \"C99999999\" (at 0 0 0)))\n\
        \t(footprint \"Conn\" (layer \"F.Cu\") (attr smd) (property \"Reference\" \"J1\" (at 0 0 0)) (property \"LCSC\" \"C1111\" (at 0 0 0)))\n\
        \t(footprint \"C_0402\" (layer \"F.Cu\") (attr smd) (property \"Reference\" \"C1\" (at 0 0 0)) (property \"LCSC\" \"\" (at 0 0 0)))\n\
        \t(footprint \"MountingHole\" (layer \"F.Cu\") (attr exclude_from_pos_files exclude_from_bom) (property \"Reference\" \"H1\" (at 0 0 0)))\n\
        \t(footprint \"LED\" (layer \"F.Cu\") (attr smd dnp) (property \"Reference\" \"D1\" (at 0 0 0)) (property \"LCSC\" \"C25744\" (at 0 0 0)))\n\
        \t(footprint \"R_0402\" (layer \"F.Cu\") (attr smd) (fp_text reference \"R3\" (at 0 0)) (property \"LCSC\" \"C25744\"))\n\
        \t(footprint \"R_0402\" (layer \"F.Cu\") (attr smd) (property \"Reference\" \"R4\" (at 0 0 0)) (property \"LCSC\" \"C1119\" (at 0 0 0)))\n\
        )\n";

    /// A current catalog, with price breaks as the feed writes them: rounded
    /// to 3 decimals, breaks below $0.01 dropped, the last one open
    /// (kicad-jlcpcb-tools `common/translate.py`). C25744's order of 15 falls
    /// in its second break. C8734 has one break, from 10 up, as some parts
    /// have. C1119's price rounds to $0.000, as 671 parts' do in the
    /// 2026-11-04 feed. C1111 has no price: NULL here, and 0 in a catalog
    /// from before #63.
    fn seed_catalog(dir: &Path) -> PathBuf {
        seed(dir, true)
    }

    /// A catalog from before #63: no `Price_Breaks` column, user_version 1.
    fn seed_old_catalog(dir: &Path) -> PathBuf {
        seed(dir, false)
    }

    fn seed(dir: &Path, with_price_breaks: bool) -> PathBuf {
        let db_path = dir.join("jlcpcb.db");
        let conn = rusqlite::Connection::open(&db_path).unwrap();
        conn.execute_batch(
            "CREATE TABLE components (LCSC TEXT, MFR_Part TEXT, Package TEXT, Manufacturer TEXT, \
             Library_Type TEXT, Description TEXT, Datasheet TEXT, Price REAL, Stock INTEGER, Category TEXT);",
        )
        .unwrap();
        if with_price_breaks {
            conn.execute_batch(
                "ALTER TABLE components ADD COLUMN Price_Breaks TEXT; PRAGMA user_version = 2;",
            )
            .unwrap();
        }
        for (lcsc, library_type, price, breaks) in [
            (
                "C25744",
                "Basic",
                Some(0.014),
                Some("1-9:0.014,10-99:0.012,100-:0.010"),
            ),
            ("C8734", "Extended", Some(5.62), Some("10-:5.620")),
            ("C1119", "Basic", Some(0.0), Some("1-:0.000")),
            (
                "C1111",
                "Extended",
                (!with_price_breaks).then_some(0.0),
                None,
            ),
        ] {
            conn.execute(
                "INSERT INTO components (LCSC, MFR_Part, Package, Manufacturer, Library_Type, \
                 Description, Datasheet, Price, Stock, Category) \
                 VALUES (?1, 'MPN', 'PKG', 'MFR', ?2, 'part', '', ?3, 100, 'cat')",
                rusqlite::params![lcsc, library_type, price],
            )
            .unwrap();
            if with_price_breaks {
                conn.execute(
                    "UPDATE components SET Price_Breaks = ?2 WHERE LCSC = ?1",
                    rusqlite::params![lcsc, breaks],
                )
                .unwrap();
            }
        }
        db_path
    }

    /// The `priced_from_catalog` entry for `lcsc`.
    fn priced<'a>(report: &'a serde_json::Value, lcsc: &str) -> &'a serde_json::Value {
        report["components"]["priced_from_catalog"]
            .as_array()
            .unwrap()
            .iter()
            .find(|part| part["lcsc"] == lcsc)
            .unwrap_or_else(|| panic!("{lcsc} not priced: {report}"))
    }

    /// `estimate_cost` takes no `db_path` argument: a caller points it at a
    /// catalog through the `jlcpcb_db_path` setting, as these tests do.
    fn config(db_path: &Path) -> crate::tools::ServerConfig {
        crate::tools::ServerConfig {
            jlcpcb_db_path: Some(db_path.to_path_buf()),
            eager_toolsets: true,
            ..Default::default()
        }
    }

    fn write_board(dir: &Path) -> PathBuf {
        let board = dir.join("board.kicad_pcb");
        std::fs::write(&board, BOARD).unwrap();
        board
    }

    async fn estimate(fab_house: &str, db_path: &Path) -> serde_json::Value {
        let dir = tempfile::tempdir().unwrap();
        let args = json!({
            "board": write_board(dir.path()).to_str().unwrap(),
            "fab_house": fab_house,
            "quantity": 5
        });
        let ctx = ToolContext::new(
            config(db_path),
            std::sync::Arc::new(crate::router::ToolRouter::new()),
        );
        match handle_estimate_cost(&args, &ctx)
            .await
            .unwrap()
            .content
            .first()
        {
            Some(crate::mcp::protocol::ToolContent::Text { text }) => {
                serde_json::from_str(text).unwrap()
            }
            other => panic!("expected text content, got {other:?}"),
        }
    }

    /// Parts with catalog prices are priced from them, the rest at the
    /// average, and each unique Extended part adds its fee once.
    #[tokio::test]
    async fn jlcpcb_prices_parts_from_the_catalog_and_adds_extended_fees() {
        let dir = tempfile::tempdir().unwrap();
        let report = estimate("jlcpcb", &seed_catalog(dir.path())).await;
        let components = &report["components"];
        assert_eq!(components["catalog"]["read"], true, "{report}");
        assert_eq!(components["catalog"]["price_breaks"], true, "{report}");

        let priced: Vec<serde_json::Value> = components["priced_from_catalog"]
            .as_array()
            .unwrap()
            .iter()
            .map(|part| {
                json!([
                    part["lcsc"],
                    part["references"],
                    part["order_quantity"],
                    part["unit_price"],
                    part["price_break"]
                ])
            })
            .collect();
        // Three per board for 5 boards is 15, in C25744's 10-99 break. C1119
        // is priced at $0, and C8734's order of 5 is below its minimum of 10.
        assert_eq!(
            priced,
            [
                json!(["C1119", ["R4"], 5, 0.0, "1-:0.000"]),
                json!(["C25744", ["R1", "R2", "R3"], 15, 0.012, "10-99:0.012"]),
                json!(["C8734", ["U1"], 5, 5.62, "10-:5.620"])
            ]
        );
        let flags: Vec<serde_json::Value> = components["priced_from_catalog"]
            .as_array()
            .unwrap()
            .iter()
            .map(|part| {
                json!([
                    part["lcsc"],
                    part["below_minimum_order"],
                    part.get("note").is_some()
                ])
            })
            .collect();
        assert_eq!(
            flags,
            [
                json!(["C1119", null, true]),
                json!(["C25744", null, false]),
                json!(["C8734", 10, false])
            ]
        );

        let estimated: Vec<(serde_json::Value, &str)> = components["estimated"]
            .as_array()
            .unwrap()
            .iter()
            .map(|part| (part["references"].clone(), part["reason"].as_str().unwrap()))
            .collect();
        assert_eq!(
            estimated,
            [
                (json!(["C1"]), "no LCSC field"),
                (json!(["J1"]), "the catalog has no price"),
                (json!(["U2"]), "not in the catalog"),
            ]
        );
        assert_eq!(components["excluded_from_bom"], json!(["H1", "D1"]));
        // U1 and J1 are Extended; J1's unknown price does not waive its fee.
        assert_eq!(components["extended_unique_parts"], 2);

        // Per board: 3 x 0.012 + 5.62 + 3 x 0.05 = 5.806, for 5 boards.
        let cost = &report["cost_estimate"];
        assert_eq!(cost["components_estimate"], "$29.03", "{report}");
        assert_eq!(cost["extended_parts_fee"], "$6.14");
        // Setup plus the eight parts the BOM lists, not H1 or D1:
        // 8 + 8 x 0.003 x 5.
        assert_eq!(cost["smt_assembly"], "$8.12", "{report}");
        // No price model cites a source yet, so the estimate says so (#209).
        assert_eq!(cost["approximate"], true);
        assert!(
            report["notes"].to_string().contains(
                "JLCPCB's PCB and assembly prices here are rough figures with no cited source"
            ),
            "{report}"
        );
        let notes = report["notes"].to_string();
        assert!(notes.contains("search_jlcpcb_parts"), "{notes}");
        assert!(!notes.contains("generate_bom"), "{notes}");
    }

    /// Without a catalog every part is at the average, and the response says
    /// why, rather than presenting averages as prices.
    #[tokio::test]
    async fn jlcpcb_without_a_catalog_reports_every_part_as_estimated() {
        let dir = tempfile::tempdir().unwrap();
        let report = estimate("jlcpcb", &dir.path().join("missing.db")).await;
        let components = &report["components"];
        assert_eq!(components["catalog"]["read"], false, "{report}");
        assert!(components["catalog"]["reason"]
            .as_str()
            .unwrap()
            .contains("download_jlcpcb_database"));
        assert_eq!(components["priced_from_catalog"], json!([]));
        let estimated = components["estimated"].as_array().unwrap();
        assert!(estimated[1..]
            .iter()
            .all(|part| part["reason"] == "catalog not read"));
        // Eight parts the BOM lists, at 0.05, for 5 boards; no Extended fee.
        assert_eq!(report["cost_estimate"]["components_estimate"], "$2.00");
        assert_eq!(report["cost_estimate"]["extended_parts_fee"], "$0.00");
    }

    /// A catalog from before #63 has no price breaks: parts are at their
    /// first break's price, and the response says so.
    #[tokio::test]
    async fn an_old_catalog_prices_at_the_first_break_and_says_so() {
        let dir = tempfile::tempdir().unwrap();
        let report = estimate("jlcpcb", &seed_old_catalog(dir.path())).await;
        let catalog = &report["components"]["catalog"];
        assert_eq!(catalog["read"], true, "{report}");
        assert_eq!(catalog["price_breaks"], false);
        assert!(catalog["reason"]
            .as_str()
            .unwrap()
            .contains("download_jlcpcb_database"));
        let resistor = priced(&report, "C25744");
        assert_eq!(resistor["unit_price"], 0.014, "{report}");
        assert_eq!(resistor["price_break"], json!(null));
        // Before #63 a 0 meant unknown, so C1119 and C1111 are at the average.
        let estimated: Vec<&serde_json::Value> = report["components"]["estimated"]
            .as_array()
            .unwrap()
            .iter()
            .map(|part| &part["lcsc"])
            .collect();
        assert_eq!(
            estimated,
            [
                &json!(null),
                &json!("C1111"),
                &json!("C1119"),
                &json!("C99999999")
            ]
        );
        // Per board: 3 x 0.014 + 5.62 + 4 x 0.05 = 5.862, for 5 boards.
        assert_eq!(report["cost_estimate"]["components_estimate"], "$29.31");
    }

    /// Through served dispatch, with the catalog from the setting: the
    /// schema accepts the call, and a part is priced at its quantity break.
    #[tokio::test]
    async fn served_estimate_prices_at_the_quantity_break() {
        let dir = tempfile::tempdir().unwrap();
        let handler = crate::mcp::handler::McpHandler::new(config(&seed_catalog(dir.path())))
            .await
            .unwrap();
        let response = handler
            .handle_message(json!({
                "jsonrpc": "2.0",
                "id": 1,
                "method": "tools/call",
                "params": {
                    "name": "estimate_cost",
                    "arguments": {
                        "board": write_board(dir.path()).to_str().unwrap(),
                        "quantity": 40
                    }
                }
            }))
            .await
            .expect("tools/call receives a response");
        let result: CallToolResult =
            serde_json::from_value(response.result.expect("a result")).unwrap();
        assert!(!result.is_error, "{result:?}");
        let body: serde_json::Value = match result.content.first() {
            Some(crate::mcp::protocol::ToolContent::Text { text }) => {
                serde_json::from_str(text).unwrap()
            }
            other => panic!("expected text content, got {other:?}"),
        };
        // Three per board for 40 boards is 120, in the open 100- break.
        let resistor = priced(&body, "C25744");
        assert_eq!(resistor["order_quantity"], 120, "{body}");
        assert_eq!(resistor["unit_price"], 0.01);
        assert_eq!(resistor["price_break"], "100-:0.010");
    }

    /// A quantity so large that count x boards overflows saturates instead
    /// of panicking or wrapping to a small order.
    #[tokio::test]
    async fn a_huge_quantity_saturates_the_order_quantity() {
        let dir = tempfile::tempdir().unwrap();
        let args = json!({
            "board": write_board(dir.path()).to_str().unwrap(),
            "quantity": u64::MAX
        });
        let ctx = ToolContext::new(
            config(&seed_catalog(dir.path())),
            std::sync::Arc::new(crate::router::ToolRouter::new()),
        );
        let result = handle_estimate_cost(&args, &ctx).await.unwrap();
        let body: serde_json::Value = match result.content.first() {
            Some(crate::mcp::protocol::ToolContent::Text { text }) => {
                serde_json::from_str(text).unwrap()
            }
            other => panic!("expected text content, got {other:?}"),
        };
        let resistor = priced(&body, "C25744");
        assert_eq!(resistor["order_quantity"], u64::MAX, "{body}");
        assert_eq!(resistor["price_break"], "100-:0.010");
    }

    /// PCBWay keeps its per-part average and does not read the catalog.
    #[tokio::test]
    async fn pcbway_keeps_its_average() {
        let dir = tempfile::tempdir().unwrap();
        let report = estimate("pcbway", &seed_catalog(dir.path())).await;
        // The eight parts the BOM lists at 0.08, for 5 boards. H1 and D1 are
        // neither bought nor placed.
        assert_eq!(report["cost_estimate"]["components_estimate"], "$3.20");
        // 8 x 0.005 x 5, not ten footprints' $0.25.
        assert_eq!(report["cost_estimate"]["smt_assembly"], "$0.20");
        assert_eq!(report["cost_estimate"]["extended_parts_fee"], "$0.00");
        assert_eq!(report["components"]["priced_from_catalog"], json!([]));
        assert!(report["components"].get("catalog").is_none());
    }
}
