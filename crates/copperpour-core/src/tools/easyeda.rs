//! EasyEDA / JLCPCB symbol and footprint importer.
//!
//! Downloads symbols and footprints from the public EasyEDA component API by LCSC part
//! number, converts them to KiCad 10 format (`.kicad_sym` and `.kicad_mod`), registers
//! both in the project's library tables (`sym-lib-table` and `fp-lib-table`), and optionally
//! places the component directly into a schematic.

use std::collections::BTreeMap;
use std::path::PathBuf;

use crate::mcp::error::ToolErrorKind;
use serde_json::json;
use sha2::{Digest, Sha256};

use crate::mcp::protocol::{CallToolResult, ToolContent};
use crate::tools::{opt_bool_flag, opt_f64, opt_str, require_str, ToolContext};

// The EasyEDA download moved to `sourcing::easyeda` (#209).
use crate::sourcing::easyeda::{fetch_easyeda_3d_assets, fetch_easyeda_bundle};

pub(crate) const EASYEDA_ATTRIBUTION: &str =
    "JLCEDA/EasyEDA Official Library (https://easyeda.com / https://lceda.cn/)";

/// Sanitize component or footprint name for use in KiCad identifiers and filenames.
pub(crate) fn sanitize_name(name: &str) -> String {
    let sanitized: String = name
        .chars()
        .map(|c| match c {
            ' ' | ':' | '/' | '\\' | '"' | '\'' | '<' | '>' | '|' | '?' | '*' => '_',
            _ => c,
        })
        .collect();
    let trimmed = sanitized.trim_matches('_');
    if trimmed.is_empty() {
        "Part".to_string()
    } else {
        trimmed.to_string()
    }
}

/// Convert an EasyEDA electrical type code to a KiCad electrical type string.
fn easyeda_pin_type(code: &str) -> &'static str {
    match code {
        "1" => "input",
        "2" => "output",
        "3" => "bidirectional",
        "4" => "power_in",
        // 0 and unspecified map to passive to prevent noisy ERC warnings in KiCad
        _ => "passive",
    }
}

/// Convert an EasyEDA pin rotation angle to a KiCad pin angle in degrees.
/// EasyEDA pin rotation convention is offset by 180° from KiCad's pin orientation convention:
/// - 180° (pin on left edge entering body going East) -> KiCad 0° (angle 0, pointing right towards body)
/// - 0° (pin on right edge entering body going West) -> KiCad 180° (angle 180, pointing left towards body)
/// - 270° (pin on bottom edge entering body going North) -> KiCad 90° (angle 90, pointing up towards body)
/// - 90° (pin on top edge entering body going South) -> KiCad 270° (angle 270, pointing down towards body)
pub(crate) fn easyeda_pin_angle(rot: &str) -> f64 {
    let rot_deg: f64 = rot.parse().unwrap_or(0.0);
    (180.0 + rot_deg).rem_euclid(360.0)
}

/// Convert an EasyEDA pin rotation angle to a KiCad pin orientation name.
fn easyeda_pin_orientation(rot: &str) -> &'static str {
    match rot {
        "0" => "left",
        "90" => "down",
        "180" => "right",
        "270" => "up",
        _ => "right",
    }
}

/// Parse symbol pins from EasyEDA shape array.
pub(crate) fn parse_easyeda_symbol_pins(
    shapes: &[serde_json::Value],
    head_x: f64,
    head_y: f64,
) -> Vec<serde_json::Value> {
    let mut pins = Vec::new();

    for shape_val in shapes {
        let Some(s) = shape_val.as_str() else {
            continue;
        };
        if !s.starts_with("P~") {
            continue;
        }

        let parts: Vec<&str> = s.split("^^").collect();
        if parts.is_empty() {
            continue;
        }
        let p0: Vec<&str> = parts[0].split('~').collect();
        if p0.len() < 7 {
            continue;
        }

        let elec_type = easyeda_pin_type(p0[2]);
        let fallback_num = p0[3];
        let raw_x: f64 = p0[4].parse().unwrap_or(0.0);
        let raw_y: f64 = p0[5].parse().unwrap_or(0.0);
        let rot = p0[6];

        // EasyEDA coordinates: 10 mil units relative to head canvas origin.
        // KiCad symbol coordinates: mm, with Y inverted (growing upwards).
        let kicad_x = (raw_x - head_x) * 0.254;
        let kicad_y = (head_y - raw_y) * 0.254;

        // Snap to KiCad's standard 1.27 mm symbol grid.
        let snap_x = (kicad_x / 1.27).round() * 1.27;
        let snap_y = (kicad_y / 1.27).round() * 1.27;

        // Pin name from part3
        let pin_name = if parts.len() > 3 {
            let p3: Vec<&str> = parts[3].split('~').collect();
            if p3.len() > 4 && !p3[4].is_empty() {
                p3[4]
            } else {
                "~"
            }
        } else {
            "~"
        };

        // Pin number from part4, falling back to part0
        let pin_num = if parts.len() > 4 {
            let p4: Vec<&str> = parts[4].split('~').collect();
            if p4.len() > 4 && !p4[4].is_empty() {
                p4[4]
            } else {
                fallback_num
            }
        } else {
            fallback_num
        };

        let angle = easyeda_pin_angle(rot);

        pins.push(json!({
            "number": pin_num,
            "name": pin_name,
            "type": elec_type,
            "electrical_type": elec_type,
            "x": snap_x,
            "y": snap_y,
            "angle": angle,
            "orientation": easyeda_pin_orientation(rot),
            "length": 2.54
        }));
    }

    pins
}

/// Parse symbol body graphics (rectangles, circles/ellipses) from EasyEDA shape array.
pub(crate) fn parse_easyeda_symbol_graphics(
    shapes: &[serde_json::Value],
    head_x: f64,
    head_y: f64,
) -> Vec<serde_json::Value> {
    let mut graphics = Vec::new();
    let snap = |val: f64| (val / 1.27).round() * 1.27;

    for shape_val in shapes {
        let Some(s) = shape_val.as_str() else {
            continue;
        };

        if s.starts_with("R~") {
            let parts: Vec<&str> = s.split('~').collect();
            if parts.len() < 7 {
                continue;
            }
            let raw_x: f64 = parts[1].parse().unwrap_or(0.0);
            let raw_y: f64 = parts[2].parse().unwrap_or(0.0);
            let raw_w: f64 = parts[5].parse().unwrap_or(0.0);
            let raw_h: f64 = parts[6].parse().unwrap_or(0.0);

            let sx = snap((raw_x - head_x) * 0.254);
            let sy = snap((head_y - raw_y) * 0.254);
            let ex = snap((raw_x + raw_w - head_x) * 0.254);
            let ey = snap((head_y - (raw_y + raw_h)) * 0.254);

            graphics.push(json!({
                "type": "rect",
                "start": { "x": sx, "y": sy },
                "end": { "x": ex, "y": ey },
                "stroke_width_mm": 0.254,
                "fill": "background"
            }));
        } else if s.starts_with("E~") || s.starts_with("C~") {
            let parts: Vec<&str> = s.split('~').collect();
            if parts.len() < 4 {
                continue;
            }
            let raw_x: f64 = parts[1].parse().unwrap_or(0.0);
            let raw_y: f64 = parts[2].parse().unwrap_or(0.0);
            let raw_r: f64 = parts[3].parse().unwrap_or(1.0);

            let cx = (raw_x - head_x) * 0.254;
            let cy = (head_y - raw_y) * 0.254;
            let r = (raw_r * 0.254).max(0.1);

            let fill = if parts.len() > 8 && parts[8] != "none" && !parts[8].is_empty() {
                "outline"
            } else {
                "none"
            };

            // Deduplicate if identical circle already added
            let already_exists = graphics.iter().any(|g| {
                g["type"] == "circle"
                    && (g["center"]["x"].as_f64().unwrap_or(0.0) - cx).abs() < 1e-4
                    && (g["center"]["y"].as_f64().unwrap_or(0.0) - cy).abs() < 1e-4
            });

            if !already_exists {
                graphics.push(json!({
                    "type": "circle",
                    "center": { "x": cx, "y": cy },
                    "radius_mm": r,
                    "stroke_width_mm": 0.254,
                    "fill": fill
                }));
            }
        }
    }

    graphics
}

/// Parse footprint pads from EasyEDA shape array.
pub(crate) fn parse_easyeda_footprint_pads(
    shapes: &[serde_json::Value],
    pkg_head_x: f64,
    pkg_head_y: f64,
) -> Vec<serde_json::Value> {
    let mut pads = Vec::new();

    for shape_val in shapes {
        let Some(s) = shape_val.as_str() else {
            continue;
        };
        if !s.starts_with("PAD~") {
            continue;
        }

        let parts: Vec<&str> = s.split('~').collect();
        if parts.len() < 9 {
            continue;
        }

        let shape_str = parts[1];
        let raw_x: f64 = parts[2].parse().unwrap_or(0.0);
        let raw_y: f64 = parts[3].parse().unwrap_or(0.0);
        let raw_w: f64 = parts[4].parse().unwrap_or(0.0);
        let raw_h: f64 = parts[5].parse().unwrap_or(0.0);
        let layer_id = parts[6];
        let pad_num = parts[8];
        let drill_rad: f64 = parts.get(9).and_then(|v| v.parse().ok()).unwrap_or(0.0);
        let rot: f64 = parts.get(11).and_then(|v| v.parse().ok()).unwrap_or(0.0);

        // Footprint coordinates: mm, centered relative to pkg canvas origin.
        // Both KiCad footprint and EasyEDA footprint have Y positive downwards.
        let x = (raw_x - pkg_head_x) * 0.254;
        let y = (raw_y - pkg_head_y) * 0.254;
        let w = raw_w * 0.254;
        let h = raw_h * 0.254;

        let is_tht = layer_id == "11" || layer_id == "MULTIPLAYER" || drill_rad > 0.0;
        let shape = match shape_str {
            "RECT" => "rect",
            "OVAL" => "oval",
            "ELLIPSE" => "circle",
            _ => "rect",
        };

        let (pad_type, layers) = if is_tht {
            ("thru_hole", vec!["*.Cu", "*.Mask"])
        } else if layer_id == "2" {
            ("smd", vec!["B.Cu", "B.Paste", "B.Mask"])
        } else {
            ("smd", vec!["F.Cu", "F.Paste", "F.Mask"])
        };

        let mut pad = json!({
            "number": pad_num,
            "type": pad_type,
            "shape": shape,
            "x": (x * 1000.0).round() / 1000.0,
            "y": (y * 1000.0).round() / 1000.0,
            "width": (w * 1000.0).round() / 1000.0,
            "height": (h * 1000.0).round() / 1000.0,
            "layers": layers,
            "rotation": rot
        });

        if is_tht && drill_rad > 0.0 {
            let drill_dia = (drill_rad * 2.0 * 0.254 * 1000.0).round() / 1000.0;
            pad["drill"] = json!(drill_dia);
        }

        pads.push(pad);
    }

    pads
}

/// Parse silkscreen and fab graphic lines from EasyEDA tracks and circles.
pub(crate) fn parse_easyeda_graphics(
    shapes: &[serde_json::Value],
    pkg_head_x: f64,
    pkg_head_y: f64,
) -> (Vec<serde_json::Value>, Vec<serde_json::Value>) {
    let mut silk_graphics = Vec::new();
    let mut fab_graphics = Vec::new();

    for shape_val in shapes {
        let Some(s) = shape_val.as_str() else {
            continue;
        };
        let parts: Vec<&str> = s.split('~').collect();
        if parts.is_empty() {
            continue;
        }

        match parts[0] {
            "TRACK" if parts.len() >= 5 => {
                let layer_id = parts[2];
                let layer_name = match layer_id {
                    "3" => "F.SilkS",
                    "4" => "B.SilkS",
                    "12" | "50" | "99" => "F.Fab",
                    _ => continue,
                };

                let coords: Vec<f64> = parts[4]
                    .split_whitespace()
                    .filter_map(|w| w.parse().ok())
                    .collect();
                if coords.len() >= 4 {
                    for chunk in coords.chunks_exact(2).collect::<Vec<_>>().windows(2) {
                        let p1_x = ((chunk[0][0] - pkg_head_x) * 0.254 * 1000.0).round() / 1000.0;
                        let p1_y = ((chunk[0][1] - pkg_head_y) * 0.254 * 1000.0).round() / 1000.0;
                        let p2_x = ((chunk[1][0] - pkg_head_x) * 0.254 * 1000.0).round() / 1000.0;
                        let p2_y = ((chunk[1][1] - pkg_head_y) * 0.254 * 1000.0).round() / 1000.0;

                        let prim = json!({
                            "type": "line",
                            "layer": layer_name,
                            "width": 0.12,
                            "start": { "x": p1_x, "y": p1_y },
                            "end": { "x": p2_x, "y": p2_y }
                        });
                        if layer_name == "F.SilkS" || layer_name == "B.SilkS" {
                            silk_graphics.push(prim);
                        } else {
                            fab_graphics.push(prim);
                        }
                    }
                }
            }
            "CIRCLE" if parts.len() >= 6 => {
                let raw_cx: f64 = parts[1].parse().unwrap_or(0.0);
                let raw_cy: f64 = parts[2].parse().unwrap_or(0.0);
                let raw_r: f64 = parts[3].parse().unwrap_or(0.0);
                let layer_id = parts[5];
                let layer_name = match layer_id {
                    "3" => "F.SilkS",
                    "4" => "B.SilkS",
                    "12" | "50" | "99" => "F.Fab",
                    _ => continue,
                };

                let cx = ((raw_cx - pkg_head_x) * 0.254 * 1000.0).round() / 1000.0;
                let cy = ((raw_cy - pkg_head_y) * 0.254 * 1000.0).round() / 1000.0;
                let r = (raw_r * 0.254 * 1000.0).round() / 1000.0;

                let prim = json!({
                    "type": "circle",
                    "layer": layer_name,
                    "width": 0.12,
                    "center": { "x": cx, "y": cy },
                    "radius": r
                });
                if layer_name == "F.SilkS" || layer_name == "B.SilkS" {
                    silk_graphics.push(prim);
                } else {
                    fab_graphics.push(prim);
                }
            }
            _ => {}
        }
    }

    (silk_graphics, fab_graphics)
}

/// Handler for the `import_jlcpcb_part` tool.
pub(crate) async fn handle_import_jlcpcb_part(
    args: &serde_json::Value,
    ctx: &ToolContext,
) -> anyhow::Result<CallToolResult> {
    handle_import_jlcpcb_part_with_fetch(args, ctx, |uuid| async move {
        fetch_easyeda_3d_assets(&uuid).await
    })
    .await
}

async fn handle_import_jlcpcb_part_with_fetch<F, Fut>(
    args: &serde_json::Value,
    ctx: &ToolContext,
    fetch_model: F,
) -> anyhow::Result<CallToolResult>
where
    F: FnOnce(String) -> Fut + Send,
    Fut: std::future::Future<Output = anyhow::Result<crate::sourcing::easyeda::Easyeda3dAssets>>
        + Send,
{
    let project_dir = match args["project_dir"].as_str() {
        Some(d) => PathBuf::from(d),
        None => match &ctx.config.project_dir {
            Some(d) => d.clone(),
            None => return Ok(CallToolResult::error("Missing 'project_dir' parameter")),
        },
    };

    if !project_dir.is_dir() {
        return Ok(CallToolResult::error(format!(
            "Project directory not found: {}",
            project_dir.display()
        )));
    }

    let lcsc_id = match require_str(args, "lcsc_id") {
        Ok(v) => v.trim(),
        Err(e) => return Ok(e),
    };

    let include_3d = match opt_bool_flag(args, "include_3d") {
        Ok(v) => v.unwrap_or(false),
        Err(e) => return Ok(e),
    };

    let library_name = opt_str(args, "library_name").unwrap_or("jlcpcb");
    let bundle = match fetch_easyeda_bundle(lcsc_id).await {
        Ok(b) => b,
        Err(e) => {
            return Ok(CallToolResult::error(format!(
                "Failed to fetch part '{lcsc_id}': {e}"
            )))
        }
    };

    // Extract symbol metadata
    let sym_head = &bundle["dataStr"]["head"];
    let sym_c_para = &sym_head["c_para"];

    let mpn = sym_c_para["Manufacturer Part"]
        .as_str()
        .or_else(|| sym_c_para["name"].as_str())
        .or_else(|| bundle["title"].as_str())
        .unwrap_or(lcsc_id);

    let manufacturer = sym_c_para["Manufacturer"].as_str().unwrap_or("");
    let jlc_class = sym_c_para["JLCPCB Part Class"].as_str().unwrap_or("");
    let raw_pre = sym_c_para["pre"].as_str().unwrap_or("U?");
    let ref_prefix = raw_pre.trim_end_matches('?');
    let ref_prefix = if ref_prefix.is_empty() {
        "U"
    } else {
        ref_prefix
    };

    let description = bundle["description"]
        .as_str()
        .unwrap_or_else(|| sym_c_para["name"].as_str().unwrap_or(mpn));

    let pkg_head = &bundle["packageDetail"]["dataStr"]["head"];
    let pkg_c_para = &pkg_head["c_para"];

    // Preferred verified PDF URL from EasyEDA packageDetail
    let mut datasheet_url = pkg_c_para["link"].as_str().unwrap_or("").to_string();
    // Without one from EasyEDA, the local catalog's; the response says what
    // the catalog gave.
    let mut lcsc_catalog = None;
    if datasheet_url.is_empty() {
        let catalog = crate::sourcing::LcscCatalog::resolve(args, ctx);
        let lookup = catalog.part_info(lcsc_id);
        lcsc_catalog = Some(lookup.report(&catalog));
        if let crate::sourcing::Lookup::Read(Some(info)) = lookup {
            datasheet_url = info.datasheet;
        }
    }

    let raw_pkg_name = pkg_c_para["package"]
        .as_str()
        .or_else(|| bundle["packageDetail"]["title"].as_str())
        .unwrap_or("Footprint");

    let symbol_name = sanitize_name(mpn);
    let footprint_name = sanitize_name(raw_pkg_name);

    let sym_head_x = sym_head["x"].as_f64().unwrap_or(0.0);
    let sym_head_y = sym_head["y"].as_f64().unwrap_or(0.0);

    let sym_shapes = bundle["dataStr"]["shape"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    let pins = parse_easyeda_symbol_pins(&sym_shapes, sym_head_x, sym_head_y);

    if pins.is_empty() {
        return Ok(CallToolResult::error(format!(
            "No symbol pins could be parsed for part '{lcsc_id}'"
        )));
    }

    let pkg_head_x = pkg_head["x"].as_f64().unwrap_or(0.0);
    let pkg_head_y = pkg_head["y"].as_f64().unwrap_or(0.0);

    let pkg_shapes = bundle["packageDetail"]["dataStr"]["shape"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    let pads = parse_easyeda_footprint_pads(&pkg_shapes, pkg_head_x, pkg_head_y);

    if pads.is_empty() {
        return Ok(CallToolResult::error(format!(
            "No footprint pads could be parsed for part '{lcsc_id}'"
        )));
    }

    let (silk_graphics, fab_graphics) = parse_easyeda_graphics(&pkg_shapes, pkg_head_x, pkg_head_y);

    // 1. Create Symbol Library and Symbol
    let sym_lib_path = project_dir.join(format!("{library_name}.kicad_sym"));
    let symbol_desc = format!("{} [Source: {}]", description, EASYEDA_ATTRIBUTION);
    let footprint_id = format!("{library_name}:{footprint_name}");

    // If symbol already exists in library, delete it cleanly first
    if sym_lib_path.is_file() {
        let _ = crate::tools::library::handle_delete_symbol(
            &json!({
                "library_path": sym_lib_path.display().to_string(),
                "symbol_name": symbol_name
            }),
            ctx,
        )
        .await;
    }

    let sym_graphics = parse_easyeda_symbol_graphics(&sym_shapes, sym_head_x, sym_head_y);

    let mut sym_args = json!({
        "library_path": sym_lib_path.display().to_string(),
        "name": symbol_name,
        "reference_prefix": ref_prefix,
        "value": mpn,
        "footprint": footprint_id,
        "datasheet": datasheet_url,
        "description": symbol_desc,
        "pins": pins,
        "fields": {
            "LCSC": lcsc_id,
            "MPN": mpn,
            "Manufacturer": manufacturer,
            "JLCPCB_Class": jlc_class
        }
    });

    if !sym_graphics.is_empty() {
        sym_args["graphics"] = json!(sym_graphics);
    }

    let sym_res = crate::tools::library::handle_create_symbol(&sym_args, ctx).await?;
    if sym_res.is_error {
        return Ok(sym_res);
    }

    // 2. Create Footprint Library and Footprint
    let fp_lib_dir = project_dir.join(format!("{library_name}.pretty"));
    let _ = tokio::fs::create_dir_all(&fp_lib_dir).await;
    let fp_path = fp_lib_dir.join(format!("{footprint_name}.kicad_mod"));

    let fp_desc = format!("{} [Source: {}]", raw_pkg_name, EASYEDA_ATTRIBUTION);

    let fp_args = json!({
        "output": fp_path.display().to_string(),
        "name": footprint_name,
        "description": fp_desc,
        "pads": pads,
        "replace_existing": true
    });

    let fp_res = crate::tools::library::handle_create_footprint(&fp_args, ctx).await?;
    if fp_res.is_error {
        return Ok(fp_res);
    }

    // Append silkscreen graphics if any were converted
    if !silk_graphics.is_empty() {
        let _ = crate::tools::footprint_graphics::handle_set_footprint_graphics(
            &json!({
                "footprint_path": fp_path.display().to_string(),
                "selector": { "layer": "F.SilkS" },
                "mode": "append",
                "graphics": silk_graphics
            }),
            ctx,
        )
        .await;
    }

    // Append fab graphics if any were converted
    if !fab_graphics.is_empty() {
        let _ = crate::tools::footprint_graphics::handle_set_footprint_graphics(
            &json!({
                "footprint_path": fp_path.display().to_string(),
                "selector": { "layer": "F.Fab" },
                "mode": "append",
                "graphics": fab_graphics
            }),
            ctx,
        )
        .await;
    }

    // 3. Register symbol and footprint libraries in project tables
    let _ = crate::tools::library::handle_register_symbol_library(
        &json!({
            "library_path": format!("${{KIPRJMOD}}/{library_name}.kicad_sym"),
            "nickname": library_name,
            "scope": "project",
            "project": project_dir.display().to_string(),
            "replace_existing": true
        }),
        ctx,
    )
    .await;

    let _ = crate::tools::library::handle_register_footprint_library(
        &json!({
            "library_path": format!("${{KIPRJMOD}}/{library_name}.pretty"),
            "nickname": library_name,
            "scope": "project",
            "project": project_dir.display().to_string(),
            "replace_existing": true
        }),
        ctx,
    )
    .await;

    // A failed requested model import is partial: library files already exist,
    // but schematic placement has not started. Preserve the original error kind.
    let model_3d_report = if include_3d {
        match attach_easyeda_3d_model_with(
            &pkg_shapes,
            [pkg_head_x, pkg_head_y],
            &project_dir,
            &footprint_name,
            &fp_path,
            fetch_model,
            |args| async move {
                crate::tools::footprint_models::handle_set_footprint_models(&args, ctx).await
            },
        )
        .await
        {
            Ok(report) => Some(report),
            Err(result) => return Ok(import_3d_failure(result, &sym_lib_path, &fp_path)),
        }
    } else {
        None
    };

    let lib_id = format!("{library_name}:{symbol_name}");

    // 4. Optional immediate placement onto schematic
    let mut placement_readback = None;
    if let (Some(sch_path), Some(x), Some(y)) = (
        args["schematic"].as_str(),
        opt_f64(args, "x"),
        opt_f64(args, "y"),
    ) {
        let mut place_args = json!({
            "schematic": sch_path,
            "lib_id": lib_id,
            "x": x,
            "y": y,
            "footprint": footprint_id,
            "lcsc_id": lcsc_id
        });
        if let Some(reference) = opt_str(args, "reference") {
            place_args["reference"] = json!(reference);
        }
        if let Some(val) = opt_str(args, "value") {
            place_args["value"] = json!(val);
        }
        if let Ok(placed) =
            crate::tools::sch_components::handle_add_schematic_component(&place_args, ctx).await
        {
            if !placed.is_error {
                if let Some(ToolContent::Text { text }) = placed.content.first() {
                    if let Ok(parsed) = serde_json::from_str::<serde_json::Value>(text) {
                        placement_readback = Some(parsed);
                    }
                }
            }
        }
    }

    let mut body = json!({
        "success": true,
        "lcsc_id": lcsc_id,
        "mpn": mpn,
        "manufacturer": manufacturer,
        "lib_id": lib_id,
        "footprint_id": footprint_id,
        "symbol_path": sym_lib_path.display().to_string(),
        "footprint_path": fp_path.display().to_string(),
        "datasheet_url": datasheet_url,
        "source_attribution": EASYEDA_ATTRIBUTION
    });

    if let Some(placement) = placement_readback {
        body["placement"] = placement;
    }
    if let Some(report) = lcsc_catalog {
        body["lcsc_catalog"] = report;
    }
    if let Some(model_3d) = model_3d_report {
        body["model_3d"] = model_3d;
    }

    Ok(CallToolResult::text(serde_json::to_string(&body).unwrap()))
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Easyeda3dSpec {
    pub(crate) uuid: String,
    /// Target center on the PCB in KiCad 3D coordinates (mm, X right, Y up).
    pub(crate) target_center_mm: [f64; 2],
    /// Vertical elevation in mm from `SVGNODE.attrs.z` (EasyEDA 10-mil units * 0.254).
    pub(crate) z_mm: f64,
    /// Raw EasyEDA `c_rotation` angles (CCW degrees around X, Y, Z).
    pub(crate) c_rotation_deg: [f64; 3],
}

fn round4(v: f64) -> f64 {
    let r = (v * 10_000.0).round() / 10_000.0;
    if r == 0.0 {
        0.0
    } else {
        r
    }
}

fn parse_triple(s: Option<&str>) -> [f64; 3] {
    let Some(s) = s else {
        return [0.0; 3];
    };
    let mut vals = [0.0; 3];
    for (slot, part) in vals.iter_mut().zip(s.split(',')) {
        if let Ok(v) = part.trim().parse::<f64>() {
            if v.is_finite() {
                *slot = v;
            }
        }
    }
    vals
}

/// Extract the 3D model UUID and placement metadata from EasyEDA `SVGNODE` shapes.
///
/// EasyEDA stores the actual 3D model UUID and transform inside an `SVGNODE`
/// with `attrs.c_etype == "outline3D"`. (`pkg_head["uuid_3d"]` is often absent
/// or points to an internal document UUID that returns 404 from `modules.easyeda.com`.)
pub(crate) fn parse_easyeda_3d_model(
    shapes: &[serde_json::Value],
    pkg_head_x: f64,
    pkg_head_y: f64,
) -> Option<Easyeda3dSpec> {
    for shape_val in shapes {
        let Some(s) = shape_val.as_str() else {
            continue;
        };
        let Some(json_str) = s.strip_prefix("SVGNODE~") else {
            continue;
        };
        let Ok(node) = serde_json::from_str::<serde_json::Value>(json_str) else {
            continue;
        };
        let attrs = &node["attrs"];
        if attrs["c_etype"].as_str() != Some("outline3D") {
            continue;
        }
        let Some(uuid) = attrs["uuid"]
            .as_str()
            .map(str::trim)
            .filter(|u| !u.is_empty())
        else {
            continue;
        };

        let c_rotation_deg = parse_triple(attrs["c_rotation"].as_str());
        let z_raw: f64 = attrs["z"]
            .as_str()
            .and_then(|v| v.trim().parse().ok())
            .or_else(|| attrs["z"].as_f64())
            .filter(|v| v.is_finite())
            .unwrap_or(0.0);
        let z_mm = z_raw * 0.254;

        // Prefer the 2D outline polygon center from childNodes: some EasyEDA
        // parts carry stale canvas coordinates in `c_origin` while their
        // `childNodes` outline sits directly over the footprint pads.
        let mut min_x = f64::INFINITY;
        let mut max_x = f64::NEG_INFINITY;
        let mut min_y = f64::INFINITY;
        let mut max_y = f64::NEG_INFINITY;
        if let Some(children) = node["childNodes"].as_array() {
            for child in children {
                let Some(pts) = child["attrs"]["points"].as_str() else {
                    continue;
                };
                let nums: Vec<f64> = pts
                    .split_whitespace()
                    .filter_map(|t| t.parse::<f64>().ok())
                    .filter(|v| v.is_finite())
                    .collect();
                for pair in nums.chunks_exact(2) {
                    min_x = min_x.min(pair[0]);
                    max_x = max_x.max(pair[0]);
                    min_y = min_y.min(pair[1]);
                    max_y = max_y.max(pair[1]);
                }
            }
        }

        let target_center_mm =
            if min_x.is_finite() && max_x.is_finite() && min_y.is_finite() && max_y.is_finite() {
                let cx = ((min_x + max_x) / 2.0 - pkg_head_x) * 0.254;
                let cy = -((min_y + max_y) / 2.0 - pkg_head_y) * 0.254;
                [cx, cy]
            } else if let Some(origin) = attrs["c_origin"].as_str() {
                let mut parts = origin
                    .split(',')
                    .filter_map(|p| p.trim().parse::<f64>().ok());
                match (parts.next(), parts.next()) {
                    (Some(ox), Some(oy)) if ox.is_finite() && oy.is_finite() => {
                        [(ox - pkg_head_x) * 0.254, -(oy - pkg_head_y) * 0.254]
                    }
                    _ => [0.0, 0.0],
                }
            } else {
                [0.0, 0.0]
            };

        return Some(Easyeda3dSpec {
            uuid: uuid.to_string(),
            target_center_mm,
            z_mm,
            c_rotation_deg,
        });
    }
    None
}

fn rotate_point_xyz(mut p: [f64; 3], rot_deg: [f64; 3]) -> [f64; 3] {
    let (rx, ry, rz) = (
        rot_deg[0].to_radians(),
        rot_deg[1].to_radians(),
        rot_deg[2].to_radians(),
    );
    if rx != 0.0 {
        let (s, c) = rx.sin_cos();
        let y = p[1] * c - p[2] * s;
        let z = p[1] * s + p[2] * c;
        p[1] = y;
        p[2] = z;
    }
    if ry != 0.0 {
        let (s, c) = ry.sin_cos();
        let x = p[0] * c + p[2] * s;
        let z = -p[0] * s + p[2] * c;
        p[0] = x;
        p[2] = z;
    }
    if rz != 0.0 {
        let (s, c) = rz.sin_cos();
        let x = p[0] * c - p[1] * s;
        let y = p[0] * s + p[1] * c;
        p[0] = x;
        p[1] = y;
    }
    p
}

pub(crate) fn obj_bounds_rotated(
    obj: &str,
    c_rotation_deg: [f64; 3],
) -> Option<([f64; 3], [f64; 3])> {
    let mut min = [f64::INFINITY; 3];
    let mut max = [f64::NEG_INFINITY; 3];
    let mut count = 0usize;
    for line in obj.lines() {
        let trimmed = line.trim_start();
        let Some(rest) = trimmed.strip_prefix("v ") else {
            continue;
        };
        let mut coords = rest.split_whitespace();
        let x = coords.next()?.parse::<f64>().ok()?;
        let y = coords.next()?.parse::<f64>().ok()?;
        let z = coords.next()?.parse::<f64>().ok()?;
        if ![x, y, z].iter().all(|v| v.is_finite()) {
            return None;
        }
        let r = rotate_point_xyz([x, y, z], c_rotation_deg);
        for i in 0..3 {
            min[i] = min[i].min(r[i]);
            max[i] = max[i].max(r[i]);
        }
        count += 1;
    }
    (count > 0 && min.iter().chain(max.iter()).all(|v| v.is_finite())).then_some((min, max))
}

/// Compute KiCad `(offset (xyz ...))` in mm and `(rotate (xyz ...))` in degrees.
///
/// KiCad's footprint `(rotate (xyz ...))` negates right-hand axis angles relative
/// to EasyEDA's CCW `c_rotation`, and applies `(rotate ...)` before `(offset ...)`.
pub(crate) fn compute_kicad_3d_transform(
    spec: &Easyeda3dSpec,
    obj_text: Option<&str>,
) -> Option<([f64; 3], [f64; 3])> {
    let kicad_rot = [
        round4((-spec.c_rotation_deg[0]).rem_euclid(360.0)),
        round4((-spec.c_rotation_deg[1]).rem_euclid(360.0)),
        round4((-spec.c_rotation_deg[2]).rem_euclid(360.0)),
    ];
    let (min, max) = obj_text.and_then(|txt| obj_bounds_rotated(txt, spec.c_rotation_deg))?;
    let offset = {
        let model_cx = (min[0] + max[0]) / 2.0;
        let model_cy = (min[1] + max[1]) / 2.0;
        [
            round4(spec.target_center_mm[0] - model_cx),
            round4(spec.target_center_mm[1] - model_cy),
            round4(-min[2] + spec.z_mm),
        ]
    };
    Some((offset, kicad_rot))
}

pub(crate) fn is_step_bytes(bytes: &[u8]) -> bool {
    let start = bytes
        .iter()
        .position(|b| !b.is_ascii_whitespace())
        .unwrap_or(bytes.len());
    let end = bytes
        .iter()
        .rposition(|b| !b.is_ascii_whitespace())
        .map_or(start, |i| i + 1);
    let bytes = &bytes[start..end];
    bytes.starts_with(b"ISO-10303-21;")
        && bytes.ends_with(b"END-ISO-10303-21;")
        && bytes.windows(7).any(|w| w == b"HEADER;")
        && bytes.windows(5).any(|w| w == b"DATA;")
        && bytes.windows(7).filter(|w| *w == b"ENDSEC;").count() >= 2
}

#[derive(Default, Clone)]
struct ObjMaterial {
    diffuse: [f64; 3],
    specular: [f64; 3],
    transparency: f64,
}

/// Convert an EasyEDA Wavefront OBJ mesh into KiCad-compatible VRML 2.0 (`.wrl`).
///
/// KiCad's VRML unit is 0.1 inch (2.54 mm), so the root `Transform` applies
/// `scale 0.3937007874` (1 / 2.54) to keep vertex coordinates in millimetres.
pub(crate) fn obj_to_vrml(obj: &str) -> Option<String> {
    let mut vertices: Vec<[f64; 3]> = Vec::new();
    let mut materials: BTreeMap<String, ObjMaterial> = BTreeMap::new();
    let mut current_def_mat: Option<String> = None;
    let mut current_use_mat = String::from("default");
    let mut faces_by_mat: BTreeMap<String, Vec<Vec<usize>>> = BTreeMap::new();

    for raw_line in obj.lines() {
        let line = raw_line.split('#').next().unwrap_or("").trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let mut parts = line.split_whitespace();
        let Some(head) = parts.next() else {
            continue;
        };
        match head {
            "v" => {
                let (Some(x), Some(y), Some(z)) = (
                    parts.next().and_then(|s| s.parse::<f64>().ok()),
                    parts.next().and_then(|s| s.parse::<f64>().ok()),
                    parts.next().and_then(|s| s.parse::<f64>().ok()),
                ) else {
                    return None;
                };
                if ![x, y, z].iter().all(|v| v.is_finite()) {
                    return None;
                }
                vertices.push([x, y, z]);
            }
            "newmtl" => {
                if let Some(name) = parts.next() {
                    current_def_mat = Some(name.to_string());
                    materials
                        .entry(name.to_string())
                        .or_insert_with(|| ObjMaterial {
                            diffuse: [0.7, 0.7, 0.7],
                            specular: [0.2, 0.2, 0.2],
                            transparency: 0.0,
                        });
                }
            }
            "Kd" => {
                if let Some(ref name) = current_def_mat {
                    if let (Some(r), Some(g), Some(b)) = (
                        parts.next().and_then(|s| s.parse::<f64>().ok()),
                        parts.next().and_then(|s| s.parse::<f64>().ok()),
                        parts.next().and_then(|s| s.parse::<f64>().ok()),
                    ) {
                        if ![r, g, b].iter().all(|v| v.is_finite()) {
                            return None;
                        }
                        if let Some(mat) = materials.get_mut(name) {
                            mat.diffuse = [r.clamp(0.0, 1.0), g.clamp(0.0, 1.0), b.clamp(0.0, 1.0)];
                        }
                    }
                }
            }
            "Ks" => {
                if let Some(ref name) = current_def_mat {
                    if let (Some(r), Some(g), Some(b)) = (
                        parts.next().and_then(|s| s.parse::<f64>().ok()),
                        parts.next().and_then(|s| s.parse::<f64>().ok()),
                        parts.next().and_then(|s| s.parse::<f64>().ok()),
                    ) {
                        if ![r, g, b].iter().all(|v| v.is_finite()) {
                            return None;
                        }
                        if let Some(mat) = materials.get_mut(name) {
                            mat.specular =
                                [r.clamp(0.0, 1.0), g.clamp(0.0, 1.0), b.clamp(0.0, 1.0)];
                        }
                    }
                }
            }
            "d" => {
                if let Some(ref name) = current_def_mat {
                    if let Some(alpha) = parts.next().and_then(|s| s.parse::<f64>().ok()) {
                        if !alpha.is_finite() {
                            return None;
                        }
                        if let Some(mat) = materials.get_mut(name) {
                            mat.transparency = (1.0 - alpha).clamp(0.0, 1.0);
                        }
                    }
                }
            }
            "usemtl" => {
                if let Some(name) = parts.next() {
                    current_use_mat = name.to_string();
                }
            }
            "f" => {
                let mut face = Vec::new();
                for tok in parts {
                    let v_str = tok.split('/').next().unwrap_or("");
                    let idx = v_str.parse::<isize>().ok()?;
                    let resolved = if idx > 0 {
                        (idx - 1) as usize
                    } else if idx < 0 {
                        let abs = idx.unsigned_abs();
                        vertices.len().checked_sub(abs)?
                    } else {
                        return None;
                    };
                    if resolved >= vertices.len() {
                        return None;
                    }
                    face.push(resolved);
                }
                if face.len() < 3 {
                    return None;
                }
                if face.len() >= 3 {
                    faces_by_mat
                        .entry(current_use_mat.clone())
                        .or_default()
                        .push(face);
                }
            }
            _ => {}
        }
    }

    if vertices.is_empty() || faces_by_mat.is_empty() {
        return None;
    }

    let mut out = String::from("#VRML V2.0 utf8\nTransform {\n  scale 0.3937007874 0.3937007874 0.3937007874\n  children [\n");
    for (mat_name, faces) in faces_by_mat {
        let mat = materials.get(&mat_name).cloned().unwrap_or(ObjMaterial {
            diffuse: [0.7, 0.7, 0.7],
            specular: [0.2, 0.2, 0.2],
            transparency: 0.0,
        });
        out.push_str(&format!(
            "    Shape {{\n      appearance Appearance {{\n        material Material {{\n          diffuseColor {:.4} {:.4} {:.4}\n          specularColor {:.4} {:.4} {:.4}\n          transparency {:.4}\n        }}\n      }}\n      geometry IndexedFaceSet {{\n        solid FALSE\n        coord Coordinate {{\n          point [\n",
            mat.diffuse[0],
            mat.diffuse[1],
            mat.diffuse[2],
            mat.specular[0],
            mat.specular[1],
            mat.specular[2],
            mat.transparency
        ));
        let mut local_map: BTreeMap<usize, usize> = BTreeMap::new();
        let mut local_verts: Vec<[f64; 3]> = Vec::new();
        let mut coord_indices: Vec<String> = Vec::with_capacity(faces.len());
        for face in faces {
            let mut face_str = String::new();
            for global_idx in face {
                let local_idx = *local_map.entry(global_idx).or_insert_with(|| {
                    let next = local_verts.len();
                    local_verts.push(vertices[global_idx]);
                    next
                });
                face_str.push_str(&format!("{local_idx},"));
            }
            face_str.push_str("-1");
            coord_indices.push(face_str);
        }
        for v in local_verts {
            out.push_str(&format!("            {} {} {},\n", v[0], v[1], v[2]));
        }
        out.push_str("          ]\n        }\n        coordIndex [\n");
        for idx_line in coord_indices {
            out.push_str(&format!("          {idx_line},\n"));
        }
        out.push_str("        ]\n      }\n    }\n");
    }
    out.push_str("  ]\n}\n");
    Some(out)
}

fn body_of(result: &CallToolResult) -> serde_json::Value {
    result
        .content
        .iter()
        .find_map(|content| match content {
            ToolContent::Text { text } => serde_json::from_str(text).ok(),
            _ => None,
        })
        .unwrap_or_else(|| json!({"message": "3D model import failed"}))
}

fn with_model_failure(
    result: CallToolResult,
    uuid: &str,
    path: Option<&std::path::Path>,
    stage: &str,
) -> CallToolResult {
    let mut body = body_of(&result);
    let uncertain = body["error"]["kind"] == "mutation_outcome_uncertain";
    body["model_3d"] = json!({
        "status": if uncertain { "uncertain" } else { "error" },
        "uuid": uuid,
        "path": path.map(|p| p.display().to_string()),
        "stage": stage,
        "asset_state": match stage { "attach" => "published", "download" | "prepare" => "not_written", _ => "unknown" },
        "retry_safe": false,
        "recovery": "Inspect the reported model and footprint before retrying. Imported library files are not rolled back; do not repeat the whole import blindly."
    });
    let mut result = CallToolResult::json(&body);
    result.is_error = true;
    result
}

fn model_error(
    message: impl Into<String>,
    uuid: &str,
    path: Option<&std::path::Path>,
    stage: &str,
) -> CallToolResult {
    let message = message.into();
    with_model_failure(
        CallToolResult::error_kind(
            ToolErrorKind::HandlerError {
                reason: message.clone(),
            },
            message,
        ),
        uuid,
        path,
        stage,
    )
}

fn import_3d_failure(
    result: CallToolResult,
    symbol_path: &std::path::Path,
    footprint_path: &std::path::Path,
) -> CallToolResult {
    let mut body = body_of(&result);
    body["success"] = json!(false);
    body["imported"] = json!({
        "symbol_path": symbol_path.display().to_string(),
        "footprint_path": footprint_path.display().to_string(),
        "rollback_attempted": false,
        "placement": "not_attempted"
    });
    let mut result = CallToolResult::json(&body);
    result.is_error = true;
    result
}

// Both production and tests run this orchestration. Only the external download
// and footprint-writer boundaries are injectable; selection/publication are real.
async fn attach_easyeda_3d_model_with<F, FFut, A, AFut>(
    pkg_shapes: &[serde_json::Value],
    pkg_head: [f64; 2],
    project_dir: &std::path::Path,
    footprint_name: &str,
    fp_path: &std::path::Path,
    fetch: F,
    attach: A,
) -> Result<serde_json::Value, CallToolResult>
where
    F: FnOnce(String) -> FFut,
    FFut: std::future::Future<Output = anyhow::Result<crate::sourcing::easyeda::Easyeda3dAssets>>,
    A: FnOnce(serde_json::Value) -> AFut,
    AFut: std::future::Future<Output = anyhow::Result<CallToolResult>>,
{
    let Some(spec) = parse_easyeda_3d_model(pkg_shapes, pkg_head[0], pkg_head[1]) else {
        return Ok(
            json!({"status": "not_available", "reason": "no EasyEDA outline3D SVGNODE with a 3D model UUID was found for this footprint"}),
        );
    };
    let assets = fetch(spec.uuid.clone()).await.map_err(|e| {
        model_error(
            format!("Failed to download EasyEDA 3D model: {e}"),
            &spec.uuid,
            None,
            "download",
        )
    })?;
    let obj_text = assets.obj.as_deref().ok();
    // The OBJ bounds establish the EasyEDA normalization for both formats.
    // A STEP download alone does not prove its native origin or placement.
    let (offset_mm, rotation_deg) =
        compute_kicad_3d_transform(&spec, obj_text).ok_or_else(|| {
            model_error(
                format!(
                    "Cannot establish model placement without valid OBJ bounds: {}",
                    assets
                        .obj
                        .as_ref()
                        .err()
                        .map(String::as_str)
                        .unwrap_or("invalid or empty vertices")
                ),
                &spec.uuid,
                None,
                "prepare",
            )
        })?;
    let (ext, bytes) = match &assets.step {
        Ok(bytes) if is_step_bytes(bytes) => ("step", bytes.clone()),
        _ => (
            "wrl",
            obj_text
                .and_then(obj_to_vrml)
                .ok_or_else(|| {
                    model_error(
                        "No complete STEP or convertible OBJ mesh was returned",
                        &spec.uuid,
                        None,
                        "prepare",
                    )
                })?
                .into_bytes(),
        ),
    };
    let digest = crate::hex::encode_hex(&Sha256::digest(&bytes));
    let filename = format!("{footprint_name}-{}.{ext}", &digest[..16]);
    let models_dir = project_dir.join("3dmodels");
    std::fs::create_dir_all(&models_dir).map_err(|e| {
        model_error(
            format!("Failed to create 3dmodels directory: {e}"),
            &spec.uuid,
            None,
            "publish",
        )
    })?;
    let model_path = models_dir.join(&filename);
    publish_model(&model_path, &bytes)
        .map_err(|e| with_model_failure(e, &spec.uuid, Some(&model_path), "publish"))?;
    let kicad_model_path = format!("${{KIPRJMOD}}/3dmodels/{filename}");
    let args = json!({
        "footprint_path": fp_path.display().to_string(), "mode": "replace",
        "models": [{"path": kicad_model_path,
            "offset": {"x": offset_mm[0], "y": offset_mm[1], "z": offset_mm[2]},
            "rotate": {"x": rotation_deg[0], "y": rotation_deg[1], "z": rotation_deg[2]}}]
    });
    let attached = match attach(args).await {
        Ok(result) => result,
        Err(error) => {
            let result = if let Some(path) = crate::tools::readback_mismatch_path(&error) {
                crate::tools::readback_mismatch_result(path, "Attach imported 3D model")
            } else if let Some(path) = crate::tools::revision_conflict_path(&error) {
                crate::tools::revision_conflict_result(path, "Attach imported 3D model")
            } else {
                // An unclassified error from a writer does not prove no write.
                crate::tools::file_outcome_uncertain(
                    fp_path,
                    "Attach imported 3D model",
                    error.to_string(),
                )
            };
            return Err(with_model_failure(
                result,
                &spec.uuid,
                Some(&model_path),
                "attach",
            ));
        }
    };
    if attached.is_error {
        return Err(with_model_failure(
            attached,
            &spec.uuid,
            Some(&model_path),
            "attach",
        ));
    }
    Ok(
        json!({"status": "attached", "uuid": spec.uuid, "format": ext,
        "kicad_path": kicad_model_path, "path": model_path.display().to_string(),
        "offset_mm": {"x": offset_mm[0], "y": offset_mm[1], "z": offset_mm[2]},
        "rotation_degrees": {"x": rotation_deg[0], "y": rotation_deg[1], "z": rotation_deg[2]}}),
    )
}

// STEP Part-21 payloads may contain non-UTF-8 strings. Keep their original
// bytes; the S-expression writer's text-only create helper cannot publish them.
fn publish_model(path: &std::path::Path, bytes: &[u8]) -> Result<(), CallToolResult> {
    use std::io::Write;
    let conflict = || crate::tools::revision_conflict_result(path, "Publish imported 3D model");
    let io_error = |e: std::io::Error| {
        CallToolResult::error_kind(
            ToolErrorKind::HandlerError {
                reason: e.to_string(),
            },
            e.to_string(),
        )
    };
    match std::fs::symlink_metadata(path) {
        Ok(metadata) => {
            if metadata.file_type().is_file() && std::fs::read(path).ok().as_deref() == Some(bytes)
            {
                return Ok(());
            }
            return Err(conflict());
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => return Err(io_error(e)),
    }
    let parent = path.parent().unwrap_or_else(|| std::path::Path::new("."));
    let mut builder = tempfile::Builder::new();
    builder.prefix(".copperpour-model-");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        builder.permissions(std::fs::Permissions::from_mode(0o666));
    }
    let mut staged = builder.tempfile_in(parent).map_err(io_error)?;
    staged.write_all(bytes).map_err(io_error)?;
    staged.as_file().sync_all().map_err(io_error)?;
    staged.persist_noclobber(path).map_err(|e| {
        if e.error.kind() == std::io::ErrorKind::AlreadyExists {
            conflict()
        } else {
            io_error(e.error)
        }
    })?;
    // The file has been published: every failed or mismatching readback is
    // uncertain, never a pre-write refusal. Do not delete it to retry.
    if std::fs::read(path).ok().as_deref() != Some(bytes) {
        return Err(crate::tools::readback_mismatch_result(
            path,
            "Publish imported 3D model",
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::router::ToolRouter;
    use crate::sourcing::easyeda::easyeda_cache_dir;
    use crate::tools::ServerConfig;
    use std::sync::Arc;

    pub(super) fn test_ctx() -> ToolContext {
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
    fn sanitize_name_removes_invalid_characters() {
        assert_eq!(sanitize_name("FPC/SMD:40P"), "FPC_SMD_40P");
        assert_eq!(sanitize_name("Part \"Special\"*1"), "Part__Special__1");
        assert_eq!(sanitize_name(""), "Part");
    }

    #[test]
    fn parse_easyeda_symbol_pins_converts_coordinates_and_snaps_to_grid() {
        let shapes = vec![
            json!("P~show~0~1~380~95~180~gge41~0^^380~95^^M380,95h10~#880000^^1~393.7~99~0~PIN1~start^^1~389.5~94~0~1~end"),
            json!("P~show~1~2~380~105~0~gge42~0^^380~105^^M380,105h10~#880000^^1~393.7~109~0~IN~start^^1~389.5~104~0~2~end"),
        ];

        let head_x = 400.0;
        let head_y = 290.0;
        let pins = parse_easyeda_symbol_pins(&shapes, head_x, head_y);

        assert_eq!(pins.len(), 2);
        // Pin 1: type 0 -> passive, rot 180 -> KiCad angle 0.0 (points East into body), name PIN1, num 1
        assert_eq!(pins[0]["number"], "1");
        assert_eq!(pins[0]["name"], "PIN1");
        assert_eq!(pins[0]["electrical_type"], "passive");
        assert_eq!(pins[0]["angle"], 0.0);
        // Pin 2: type 1 -> input, rot 0 -> KiCad angle 180.0 (points West into body), name IN, num 2
        assert_eq!(pins[1]["number"], "2");
        assert_eq!(pins[1]["name"], "IN");
        assert_eq!(pins[1]["electrical_type"], "input");
        assert_eq!(pins[1]["angle"], 180.0);

        // Coordinates must be multiples of 1.27 mm
        let x1 = pins[0]["x"].as_f64().unwrap();
        let y1 = pins[0]["y"].as_f64().unwrap();
        assert!(((x1 / 1.27).round() * 1.27 - x1).abs() < 1e-6);
        assert!(((y1 / 1.27).round() * 1.27 - y1).abs() < 1e-6);
    }

    #[test]
    fn parse_easyeda_symbol_graphics_converts_rect_and_circle() {
        let shapes = vec![
            json!("R~360~278~2~2~80~44~#880000~1~0~none~gge1~0~"),
            json!("E~365~283~1.5~1.5~#880000~1~0~#880000~gge2~0"),
            json!("E~365~283~1.5~1.5~#880000~1~0~#880000~gge3~0"), // duplicate circle
        ];
        let graphics = parse_easyeda_symbol_graphics(&shapes, 400.0, 300.0);
        assert_eq!(graphics.len(), 2);
        assert_eq!(graphics[0]["type"], "rect");
        assert_eq!(graphics[0]["start"]["x"], -10.16);
        assert_eq!(graphics[0]["start"]["y"], 5.08);
        assert_eq!(graphics[0]["end"]["x"], 10.16);
        assert_eq!(graphics[0]["end"]["y"], -5.08);
        assert_eq!(graphics[0]["fill"], "background");

        assert_eq!(graphics[1]["type"], "circle");
        assert_eq!(graphics[1]["fill"], "outline");
    }

    #[test]
    fn parse_easyeda_footprint_pads_converts_smd_and_tht() {
        let shapes = vec![
            // SMD pad
            json!("PAD~RECT~3926.228~2997.5~1.1811~4.7244~1~~1~0~3925.6377 2995.1378 3926.8189 2995.1378 3926.8189 2999.8622 3925.6377 2999.8622~0"),
            // THT pad
            json!("PAD~OVAL~3900.0~2900.0~5.0~5.0~11~~2~1.5~~0"),
        ];

        let pkg_head_x = 3926.228;
        let pkg_head_y = 2997.5;
        let pads = parse_easyeda_footprint_pads(&shapes, pkg_head_x, pkg_head_y);

        assert_eq!(pads.len(), 2);

        // SMD Pad at (0.0, 0.0)
        assert_eq!(pads[0]["number"], "1");
        assert_eq!(pads[0]["type"], "smd");
        assert_eq!(pads[0]["shape"], "rect");
        assert_eq!(pads[0]["x"], 0.0);
        assert_eq!(pads[0]["y"], 0.0);
        assert_eq!(pads[0]["layers"], json!(["F.Cu", "F.Paste", "F.Mask"]));

        // THT Pad
        assert_eq!(pads[1]["number"], "2");
        assert_eq!(pads[1]["type"], "thru_hole");
        assert_eq!(pads[1]["shape"], "oval");
        assert_eq!(pads[1]["layers"], json!(["*.Cu", "*.Mask"]));
        assert!(pads[1]["drill"].as_f64().unwrap() > 0.0);
    }

    #[test]
    fn parse_easyeda_graphics_converts_silk_and_fab() {
        let shapes = vec![
            // Silkscreen track
            json!("TRACK~1~3~~100 100 200 100~gge1~0"),
            // Fab track
            json!("TRACK~1~12~~100 100 100 200~gge2~0"),
            // Silkscreen circle
            json!("CIRCLE~150~150~20~1~3~gge3~0~~"),
        ];

        let (silk, fab) = parse_easyeda_graphics(&shapes, 100.0, 100.0);
        assert_eq!(silk.len(), 2); // 1 track + 1 circle
        assert_eq!(fab.len(), 1); // 1 track
        assert_eq!(silk[0]["layer"], "F.SilkS");
        assert_eq!(silk[1]["layer"], "F.SilkS");
        assert_eq!(fab[0]["layer"], "F.Fab");
    }

    /// An EasyEDA bundle for a two-pin mock part, with `link` as its
    /// datasheet.
    fn mock_bundle(link: &str) -> serde_json::Value {
        json!({
            "title": "MOCK-PART-40P",
            "description": "Mock 40-Pin Connector",
            "dataStr": {
                "head": {
                    "x": 400.0,
                    "y": 290.0,
                    "c_para": {
                        "pre": "J?",
                        "name": "MOCK-PART-40P",
                        "package": "MOCK-PKG-40P",
                        "Manufacturer": "MockMfr",
                        "Manufacturer Part": "MOCK-PART-40P",
                        "JLCPCB Part Class": "Basic Part"
                    }
                },
                "shape": [
                    "P~show~0~1~380~95~180~gge1~0^^380~95^^M380,95h10~#880000^^1~393.7~99~0~P1~start^^1~389.5~94~0~1~end",
                    "P~show~0~2~380~105~180~gge2~0^^380~105^^M380,105h10~#880000^^1~393.7~109~0~P2~start^^1~389.5~104~0~2~end"
                ]
            },
            "packageDetail": {
                "title": "MOCK-PKG-40P",
                "dataStr": {
                    "head": {
                        "x": 100.0,
                        "y": 100.0,
                        "c_para": {
                            "package": "MOCK-PKG-40P",
                            "link": link
                        }
                    },
                    "shape": [
                        "PAD~RECT~100.0~100.0~1.0~2.0~1~~1~0~100 100~0",
                        "PAD~RECT~105.0~100.0~1.0~2.0~1~~2~0~105 100~0",
                        "TRACK~1~3~~95 95 110 95~gge1~0"
                    ]
                }
            }
        })
    }

    /// Without a datasheet from EasyEDA, the import takes the catalog's, and
    /// says what the catalog gave, as every catalog tool reports it (#209).
    #[tokio::test]
    async fn import_falls_back_to_the_catalogs_datasheet_and_reports_it() {
        let tmp = tempfile::tempdir().unwrap();
        let cache_dir = easyeda_cache_dir().unwrap();
        std::fs::create_dir_all(&cache_dir).unwrap();
        let mock_lcsc_id = "C9999998";
        std::fs::write(
            cache_dir.join(format!("{mock_lcsc_id}.json")),
            serde_json::to_string(&mock_bundle("")).unwrap(),
        )
        .unwrap();
        let db_path = tmp.path().join("jlcpcb.db");
        let conn = rusqlite::Connection::open(&db_path).unwrap();
        conn.execute_batch(&format!(
            "CREATE TABLE components (LCSC TEXT, MFR_Part TEXT, Manufacturer TEXT, Datasheet TEXT, Description TEXT);
             INSERT INTO components VALUES ('{mock_lcsc_id}', 'MOCK-PART-40P', 'MockMfr', 'https://example.com/catalog.pdf', 'd');"
        ))
        .unwrap();
        drop(conn);

        let args = json!({
            "project_dir": tmp.path().join("project").display().to_string(),
            "lcsc_id": mock_lcsc_id,
            "library_name": "test_jlc",
            "db_path": db_path.display().to_string()
        });
        std::fs::create_dir_all(tmp.path().join("project")).unwrap();
        let result = handle_import_jlcpcb_part(&args, &test_ctx()).await.unwrap();
        assert!(!result.is_error, "{:?}", result.content);
        let ToolContent::Text { text } = &result.content[0] else {
            panic!("expected text response");
        };
        let body: serde_json::Value = serde_json::from_str(text).unwrap();
        assert_eq!(body["datasheet_url"], "https://example.com/catalog.pdf");
        assert_eq!(body["lcsc_catalog"]["read"], true, "{body}");
        assert_eq!(body["lcsc_catalog"]["path"], json!(db_path), "{body}");
    }

    #[tokio::test]
    async fn handle_import_jlcpcb_part_with_mock_cache() {
        let tmp = tempfile::tempdir().unwrap();
        let project_dir = tmp.path();

        // Seed mock EasyEDA response in cache
        let cache_dir = easyeda_cache_dir().unwrap();
        std::fs::create_dir_all(&cache_dir).unwrap();
        let mock_lcsc_id = "C9999999";
        let mock_bundle = mock_bundle("https://example.com/mock.pdf");
        std::fs::write(
            cache_dir.join(format!("{mock_lcsc_id}.json")),
            serde_json::to_string(&mock_bundle).unwrap(),
        )
        .unwrap();

        let ctx = test_ctx();
        let args = json!({
            "project_dir": project_dir.display().to_string(),
            "lcsc_id": mock_lcsc_id,
            "library_name": "test_jlc"
        });

        let result = handle_import_jlcpcb_part(&args, &ctx).await.unwrap();
        assert!(!result.is_error, "{:?}", result.content);

        let ToolContent::Text { text } = &result.content[0] else {
            panic!("expected text response");
        };
        let body: serde_json::Value = serde_json::from_str(text).unwrap();
        assert_eq!(body["success"], true);
        assert_eq!(body["lcsc_id"], mock_lcsc_id);
        assert_eq!(body["mpn"], "MOCK-PART-40P");
        assert_eq!(body["manufacturer"], "MockMfr");
        assert_eq!(body["lib_id"], "test_jlc:MOCK-PART-40P");
        assert_eq!(body["footprint_id"], "test_jlc:MOCK-PKG-40P");
        assert_eq!(body["datasheet_url"], "https://example.com/mock.pdf");
        assert_eq!(body["source_attribution"], EASYEDA_ATTRIBUTION);
        // EasyEDA had a datasheet, so the catalog wasn't asked.
        assert!(body.get("lcsc_catalog").is_none(), "{body}");

        // Verify .kicad_sym was created and contains symbol with custom fields
        let sym_file = project_dir.join("test_jlc.kicad_sym");
        assert!(sym_file.is_file());
        let sym_content = std::fs::read_to_string(&sym_file).unwrap();
        assert!(sym_content.contains("MOCK-PART-40P"));
        assert!(sym_content.contains("C9999999"));
        assert!(sym_content.contains("MockMfr"));
        assert!(sym_content.contains(EASYEDA_ATTRIBUTION));

        // Verify .kicad_mod was created
        let fp_file = project_dir
            .join("test_jlc.pretty")
            .join("MOCK-PKG-40P.kicad_mod");
        assert!(fp_file.is_file());
        let fp_content = std::fs::read_to_string(&fp_file).unwrap();
        assert!(fp_content.contains(EASYEDA_ATTRIBUTION));
        assert!(fp_content.contains("(pad \"1\" smd rect"));

        // Verify library tables exist
        assert!(project_dir.join("sym-lib-table").is_file());
        assert!(project_dir.join("fp-lib-table").is_file());
    }

    #[tokio::test]
    async fn handle_import_jlcpcb_part_reimport_replaces_existing_footprint() {
        let tmp = tempfile::tempdir().unwrap();
        let project_dir = tmp.path();

        let cache_dir = easyeda_cache_dir().unwrap();
        std::fs::create_dir_all(&cache_dir).unwrap();
        let mock_lcsc_id = "C9999998";
        let mock_bundle = mock_bundle("https://example.com/mock.pdf");
        std::fs::write(
            cache_dir.join(format!("{mock_lcsc_id}.json")),
            serde_json::to_string(&mock_bundle).unwrap(),
        )
        .unwrap();

        let ctx = test_ctx();
        let args = json!({
            "project_dir": project_dir.display().to_string(),
            "lcsc_id": mock_lcsc_id,
            "library_name": "test_jlc"
        });

        let first = handle_import_jlcpcb_part(&args, &ctx).await.unwrap();
        assert!(!first.is_error, "{:?}", first.content);

        let second = handle_import_jlcpcb_part(&args, &ctx).await.unwrap();
        assert!(
            !second.is_error,
            "re-importing an existing JLCPCB part must replace its footprint: {:?}",
            second.content
        );
    }

    #[tokio::test]
    async fn handle_import_jlcpcb_part_with_placement() {
        let tmp = tempfile::tempdir().unwrap();
        let project_dir = tmp.path();

        let sch_path = project_dir.join("test_sheet.kicad_sch");
        std::fs::write(
            &sch_path,
            "(kicad_sch\n  (version 20250610)\n  (generator \"konnect\")\n  (uuid \"aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee\")\n  (paper \"A4\")\n  (lib_symbols\n  )\n)\n",
        )
        .unwrap();

        let cache_dir = easyeda_cache_dir().unwrap();
        std::fs::create_dir_all(&cache_dir).unwrap();
        let mock_lcsc_id = "C8888888";
        let mock_bundle = json!({
            "title": "PLACED-PART",
            "description": "Mock Part for Placement",
            "dataStr": {
                "head": {
                    "x": 400.0,
                    "y": 290.0,
                    "c_para": {
                        "pre": "U?",
                        "name": "PLACED-PART",
                        "package": "PLACED-PKG",
                        "Manufacturer": "TestMfr",
                        "Manufacturer Part": "PLACED-PART",
                        "JLCPCB Part Class": "Basic Part"
                    }
                },
                "shape": [
                    "P~show~0~1~380~95~180~gge1~0^^380~95^^M380,95h10~#880000^^1~393.7~99~0~P1~start^^1~389.5~94~0~1~end"
                ]
            },
            "packageDetail": {
                "title": "PLACED-PKG",
                "dataStr": {
                    "head": {
                        "x": 100.0,
                        "y": 100.0,
                        "c_para": {
                            "package": "PLACED-PKG"
                        }
                    },
                    "shape": [
                        "PAD~RECT~100.0~100.0~1.0~2.0~1~~1~0~100 100~0"
                    ]
                }
            }
        });
        std::fs::write(
            cache_dir.join(format!("{mock_lcsc_id}.json")),
            serde_json::to_string(&mock_bundle).unwrap(),
        )
        .unwrap();

        let ctx = test_ctx();
        let args = json!({
            "project_dir": project_dir.display().to_string(),
            "lcsc_id": mock_lcsc_id,
            "library_name": "place_lib",
            "schematic": sch_path.display().to_string(),
            "x": 120.0,
            "y": 80.0,
            "reference": "U10"
        });

        let result = handle_import_jlcpcb_part(&args, &ctx).await.unwrap();
        assert!(!result.is_error);

        let ToolContent::Text { text } = &result.content[0] else {
            panic!("expected text response");
        };
        let body: serde_json::Value = serde_json::from_str(text).unwrap();
        assert_eq!(body["success"], true);
        assert!(body.get("placement").is_some());

        // Verify the schematic contains the placed symbol with custom fields
        let sch = copperpour_schematic_editor::Schematic::load(&sch_path).unwrap();
        let u10 = sch.symbols.by_reference("U10").expect("U10 placed");
        assert_eq!(u10.property("LCSC"), Some("C8888888"));
        assert_eq!(u10.property("MPN"), Some("PLACED-PART"));
        assert_eq!(u10.property("Manufacturer"), Some("TestMfr"));
    }

    #[test]
    fn parse_easyeda_3d_model_prefers_child_nodes_over_stale_c_origin() {
        // Real EasyEDA footprints like C20526 can have stale c_origin coordinates far off-pad
        // while childNodes outline sits centered on the footprint pads.
        let shapes = vec![
            json!("PAD~RECT~100.0~100.0~1.0~2.0~1~~1~0~100 100~0"),
            json!(concat!(
                "SVGNODE~{\"attrs\":{",
                "\"c_etype\":\"outline3D\",",
                "\"uuid\":\"abc123def456\",",
                "\"c_origin\":\"-3292,3420\",",
                "\"z\":\"-3.5433\",",
                "\"c_rotation\":\"0,0,180\"",
                "},\"childNodes\":[{\"attrs\":{",
                "\"points\":\"95 90 105 90 105 110 95 110\"",
                "}}]}"
            )),
        ];

        let spec = parse_easyeda_3d_model(&shapes, 100.0, 100.0).expect("outline3D parsed");
        assert_eq!(spec.uuid, "abc123def456");
        assert!((spec.target_center_mm[0] - 0.0).abs() < 1e-6);
        assert!((spec.target_center_mm[1] - 0.0).abs() < 1e-6);
        assert!((spec.z_mm - (-3.5433 * 0.254)).abs() < 1e-6);
        assert_eq!(spec.c_rotation_deg, [0.0, 0.0, 180.0]);
    }

    #[test]
    fn parse_easyeda_3d_model_falls_back_to_c_origin_and_handles_missing() {
        let shapes_fallback = vec![json!(concat!(
            "SVGNODE~{\"attrs\":{",
            "\"c_etype\":\"outline3D\",",
            "\"uuid\":\"u123\",",
            "\"c_origin\":\"110,90\",",
            "\"z\":0,",
            "\"c_rotation\":\"90,0,270\"",
            "}}"
        ))];
        let spec = parse_easyeda_3d_model(&shapes_fallback, 100.0, 100.0).expect("fallback origin");
        assert!((spec.target_center_mm[0] - 2.54).abs() < 1e-6);
        assert!((spec.target_center_mm[1] - 2.54).abs() < 1e-6);
        assert_eq!(spec.c_rotation_deg, [90.0, 0.0, 270.0]);

        let shapes_missing = vec![json!(
            "SVGNODE~{\"attrs\":{\"c_etype\":\"outline3D\",\"uuid\":\"   \"}}"
        )];
        assert!(parse_easyeda_3d_model(&shapes_missing, 100.0, 100.0).is_none());
        assert!(parse_easyeda_3d_model(&[], 100.0, 100.0).is_none());
    }

    #[test]
    fn compute_kicad_3d_transform_applies_rotation_centering_and_z() {
        // Off-center box spanning X:[0, 2], Y:[-3, -1], Z:[-0.4, 0.6] rotated 180 around Z
        // -> rotated spans X:[-2, 0], Y:[1, 3], Z:[-0.4, 0.6] (center (-1, 2), min_z -0.4)
        // With target_center (0, 0) and z_mm = 0 -> offset = (1.0, -2.0, 0.4), rot = (0, 0, 180)
        let obj = "\
v 0 -3 -0.4
v 2 -1 0.6
f 1 2 1
";
        let spec = Easyeda3dSpec {
            uuid: "u".to_string(),
            target_center_mm: [0.0, 0.0],
            z_mm: 0.0,
            c_rotation_deg: [0.0, 0.0, 180.0],
        };
        let (offset, rot) = compute_kicad_3d_transform(&spec, Some(obj)).unwrap();
        assert_eq!(offset, [1.0, -2.0, 0.4]);
        assert_eq!(rot, [0.0, 0.0, 180.0]);

        // Positive CCW 90 -> KiCad CW 270; negative z_mm lifts pins to PCB surface
        let spec_rot90 = Easyeda3dSpec {
            uuid: "u".to_string(),
            target_center_mm: [0.5, -0.5],
            z_mm: -0.9,
            c_rotation_deg: [0.0, 0.0, 90.0],
        };
        assert!(compute_kicad_3d_transform(&spec_rot90, None).is_none());
    }

    #[test]
    fn is_step_bytes_checks_iso_10303_header() {
        assert!(is_step_bytes(
            b"ISO-10303-21;\nHEADER;ENDSEC;\nDATA;ENDSEC;\nEND-ISO-10303-21;"
        ));
        assert!(!is_step_bytes(b"  \r\n\tISO-10303-21;"));
        assert!(!is_step_bytes(b"v 0 0 0\nf 1 2 3\n"));
        assert!(!is_step_bytes(b"<html>404 Not Found</html>"));
    }

    #[test]
    fn obj_to_vrml_converts_mesh_materials_and_scale() {
        let obj = "\
newmtl red_body
Kd 0.8 0.1 0.1
Ks 0.2 0.2 0.2
d 0.75
usemtl red_body
v 0 0 0
v 2 0 0
v 2 1 0
v 0 1 0
f 1/1/1 2/1/1 3/1/1 4/1/1
";
        let wrl = obj_to_vrml(obj).expect("valid VRML output");
        assert!(wrl.starts_with("#VRML V2.0 utf8"));
        assert!(wrl.contains("scale 0.3937007874 0.3937007874 0.3937007874"));
        assert!(wrl.contains("diffuseColor 0.8000 0.1000 0.1000"));
        assert!(wrl.contains("transparency 0.2500"));
        assert!(wrl.contains("0,1,2,3,-1"));

        assert!(obj_to_vrml("v 0 0 0\n").is_none());
        assert!(obj_to_vrml("").is_none());
    }

    #[tokio::test]
    async fn handle_import_jlcpcb_part_refuses_non_boolean_include_3d() {
        let tmp = tempfile::tempdir().unwrap();
        let ctx = test_ctx();
        let args = json!({
            "project_dir": tmp.path().display().to_string(),
            "lcsc_id": "C8888888",
            "include_3d": "yes"
        });
        let res = handle_import_jlcpcb_part(&args, &ctx).await.unwrap();
        assert!(res.is_error, "non-boolean include_3d must be refused");
        let ToolContent::Text { text } = &res.content[0] else {
            panic!("expected text error response");
        };
        let body: serde_json::Value = serde_json::from_str(text).unwrap();
        assert_eq!(body["error"]["kind"], "invalid_argument");
    }

    #[tokio::test]
    async fn handle_import_jlcpcb_part_include_3d_reports_not_available_when_missing_outline3d() {
        let tmp = tempfile::tempdir().unwrap();
        let project_dir = tmp.path();

        let cache_dir = easyeda_cache_dir().unwrap();
        std::fs::create_dir_all(&cache_dir).unwrap();
        let mock_lcsc_id = "C7777777";
        let cache_file = cache_dir.join(format!("{mock_lcsc_id}.json"));
        let mock_bundle = json!({
            "title": "NO3D-PART",
            "description": "Mock Part Without 3D",
            "dataStr": {
                "head": {
                    "x": 400.0,
                    "y": 290.0,
                    "c_para": {
                        "pre": "R?",
                        "name": "NO3D-PART",
                        "package": "NO3D-PKG"
                    }
                },
                "shape": [
                    "P~show~0~1~380~95~180~gge1~0^^380~95^^M380,95h10~#880000^^1~393.7~99~0~P1~start^^1~389.5~94~0~1~end"
                ]
            },
            "packageDetail": {
                "title": "NO3D-PKG",
                "dataStr": {
                    "head": {
                        "x": 100.0,
                        "y": 100.0,
                        "c_para": { "package": "NO3D-PKG" }
                    },
                    "shape": [
                        "PAD~RECT~100.0~100.0~1.0~2.0~1~~1~0~100 100~0"
                    ]
                }
            }
        });
        std::fs::write(&cache_file, serde_json::to_string(&mock_bundle).unwrap()).unwrap();

        let ctx = test_ctx();
        let args = json!({
            "project_dir": project_dir.display().to_string(),
            "lcsc_id": mock_lcsc_id,
            "library_name": "no3d_lib",
            "include_3d": true
        });
        let result = handle_import_jlcpcb_part(&args, &ctx).await;
        let _ = std::fs::remove_file(&cache_file);
        let result = result.unwrap();
        assert!(!result.is_error);

        let ToolContent::Text { text } = &result.content[0] else {
            panic!("expected text response");
        };
        let body: serde_json::Value = serde_json::from_str(text).unwrap();
        assert_eq!(body["success"], true);
        assert_eq!(body["model_3d"]["status"], "not_available");
    }

    #[tokio::test]
    async fn wrl_conversion_and_footprint_attachment_roundtrip() {
        let tmp = tempfile::tempdir().unwrap();
        let fp_path = tmp.path().join("TEST_PKG.kicad_mod");
        std::fs::write(
            &fp_path,
            "(footprint \"TEST_PKG\"\n  (layer \"F.Cu\")\n  (pad \"1\" smd rect (at 0 0) (size 1 1) (layers \"F.Cu\"))\n)\n",
        )
        .unwrap();

        let obj = "newmtl body\nKd 0.2 0.2 0.2\nusemtl body\nv -1 -2 0\nv 1 2 1\nf 1 2 1\n";
        let wrl = obj_to_vrml(obj).expect("wrl generated");
        let models_dir = tmp.path().join("3dmodels");
        std::fs::create_dir_all(&models_dir).unwrap();
        std::fs::write(models_dir.join("TEST_PKG.wrl"), wrl).unwrap();

        let spec = Easyeda3dSpec {
            uuid: "u".to_string(),
            target_center_mm: [0.5, -0.5],
            z_mm: 0.0,
            c_rotation_deg: [0.0, 0.0, 180.0],
        };
        let (offset, rot) = compute_kicad_3d_transform(&spec, Some(obj)).unwrap();

        let ctx = test_ctx();
        let res = crate::tools::footprint_models::handle_set_footprint_models(
            &json!({
                "footprint_path": fp_path.display().to_string(),
                "mode": "replace",
                "models": [{
                    "path": "${KIPRJMOD}/3dmodels/TEST_PKG.wrl",
                    "offset": { "x": offset[0], "y": offset[1], "z": offset[2] },
                    "rotate": { "x": rot[0], "y": rot[1], "z": rot[2] }
                }]
            }),
            &ctx,
        )
        .await
        .unwrap();
        assert!(!res.is_error);

        let updated = std::fs::read_to_string(&fp_path).unwrap();
        assert!(updated.contains("(model \"${KIPRJMOD}/3dmodels/TEST_PKG.wrl\""));
        assert!(updated.contains("(offset (xyz 0.5 -0.5 0))"));
        assert!(updated.contains("(rotate (xyz 0 0 180))"));
    }
}

#[cfg(test)]
#[path = "easyeda_3d_tests.rs"]
mod model_tests;
