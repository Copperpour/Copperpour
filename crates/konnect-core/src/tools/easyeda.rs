//! EasyEDA / JLCPCB symbol and footprint importer.
//!
//! Downloads symbols and footprints from the public EasyEDA component API by LCSC part
//! number, converts them to KiCad 10 format (`.kicad_sym` and `.kicad_mod`), registers
//! both in the project's library tables (`sym-lib-table` and `fp-lib-table`), and optionally
//! places the component directly into a schematic.

use std::path::PathBuf;

use serde_json::json;

use crate::mcp::protocol::{CallToolResult, ToolContent};
use crate::tools::{opt_f64, opt_str, require_str, ToolContext};

pub(crate) const EASYEDA_ATTRIBUTION: &str =
    "JLCEDA/EasyEDA Official Library (https://easyeda.com / https://lceda.cn/)";

/// Resolve the local disk cache directory for EasyEDA API responses.
pub(crate) fn easyeda_cache_dir() -> PathBuf {
    #[cfg(target_os = "windows")]
    {
        let appdata = std::env::var("APPDATA").unwrap_or_default();
        PathBuf::from(appdata)
            .join("konnect")
            .join("cache")
            .join("easyeda")
    }
    #[cfg(not(target_os = "windows"))]
    {
        let home = std::env::var("HOME").unwrap_or_default();
        PathBuf::from(home)
            .join(".konnect")
            .join("cache")
            .join("easyeda")
    }
}

/// Fetch an EasyEDA component bundle by LCSC ID, using local disk cache first
/// and falling back across easyeda.com and lceda.cn with standard browser headers.
pub(crate) async fn fetch_easyeda_bundle(lcsc_id: &str) -> anyhow::Result<serde_json::Value> {
    let clean_id = lcsc_id.trim();
    if clean_id.is_empty() {
        anyhow::bail!("LCSC part number cannot be empty");
    }

    let cache_dir = easyeda_cache_dir();
    let cache_file = cache_dir.join(format!("{clean_id}.json"));
    if cache_file.is_file() {
        if let Ok(content) = std::fs::read_to_string(&cache_file) {
            if let Ok(json) = serde_json::from_str::<serde_json::Value>(&content) {
                if json.get("dataStr").is_some() {
                    return Ok(json);
                }
            }
        }
    }

    let client = reqwest::Client::builder()
        .user_agent("Mozilla/5.0 (X11; Linux x86_64; rv:130.0) Gecko/20100101 Firefox/130.0")
        .timeout(std::time::Duration::from_secs(20))
        .build()?;

    let primary_url =
        format!("https://easyeda.com/api/products/{clean_id}/components?version=6.4.19.5");
    let fallback_url =
        format!("https://lceda.cn/api/products/{clean_id}/components?version=6.4.19.5");

    let mut body = None;
    for url in [&primary_url, &fallback_url] {
        if let Ok(resp) = client
            .get(url)
            .header("Accept", "application/json")
            .send()
            .await
        {
            if resp.status().is_success() {
                if let Ok(json) = resp.json::<serde_json::Value>().await {
                    if json["success"].as_bool() == Some(true) && json.get("result").is_some() {
                        body = Some(json["result"].clone());
                        break;
                    }
                }
            }
        }
    }

    let result = body.ok_or_else(|| {
        anyhow::anyhow!("Part '{clean_id}' not found in EasyEDA or could not be downloaded")
    })?;

    if let Err(e) = std::fs::create_dir_all(&cache_dir) {
        tracing::warn!("Failed to create EasyEDA cache dir: {e}");
    } else if let Ok(s) = serde_json::to_string(&result) {
        let _ = std::fs::write(&cache_file, s);
    }

    Ok(result)
}

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
    if datasheet_url.is_empty() {
        let db_path = crate::tools::integration::resolve_db_path(args, ctx);
        if let Some(info) = crate::tools::integration::query_jlcpcb_db_part(&db_path, lcsc_id) {
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
        "pads": pads
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

    // Optional 3D Model fetch
    if args["include_3d"].as_bool().unwrap_or(false) {
        if let Some(uuid_3d) = pkg_head["uuid_3d"].as_str() {
            let model_url =
                format!("https://modules.easyeda.com/qAxj6KHrDKw4blvCG8QJPs7Y/{uuid_3d}");
            let client = reqwest::Client::new();
            if let Ok(resp) = client.get(&model_url).send().await {
                if resp.status().is_success() {
                    let models_dir = project_dir.join("3dmodels");
                    let _ = tokio::fs::create_dir_all(&models_dir).await;
                    let model_path = models_dir.join(format!("{footprint_name}.step"));
                    if let Ok(bytes) = resp.bytes().await {
                        let _ = tokio::fs::write(&model_path, bytes).await;
                    }
                }
            }
        }
    }

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

    Ok(CallToolResult::text(serde_json::to_string(&body).unwrap()))
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

    #[tokio::test]
    async fn handle_import_jlcpcb_part_with_mock_cache() {
        let tmp = tempfile::tempdir().unwrap();
        let project_dir = tmp.path();

        // Seed mock EasyEDA response in cache
        let cache_dir = easyeda_cache_dir();
        std::fs::create_dir_all(&cache_dir).unwrap();
        let mock_lcsc_id = "C9999999";
        let mock_bundle = json!({
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
                            "link": "https://example.com/mock.pdf"
                        }
                    },
                    "shape": [
                        "PAD~RECT~100.0~100.0~1.0~2.0~1~~1~0~100 100~0",
                        "PAD~RECT~105.0~100.0~1.0~2.0~1~~2~0~105 100~0",
                        "TRACK~1~3~~95 95 110 95~gge1~0"
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
    async fn handle_import_jlcpcb_part_with_placement() {
        let tmp = tempfile::tempdir().unwrap();
        let project_dir = tmp.path();

        let sch_path = project_dir.join("test_sheet.kicad_sch");
        std::fs::write(
            &sch_path,
            "(kicad_sch\n  (version 20250610)\n  (generator \"konnect\")\n  (uuid \"aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee\")\n  (paper \"A4\")\n  (lib_symbols\n  )\n)\n",
        )
        .unwrap();

        let cache_dir = easyeda_cache_dir();
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
        let sch = konnect_schematic_editor::Schematic::load(&sch_path).unwrap();
        let u10 = sch.symbols.by_reference("U10").expect("U10 placed");
        assert_eq!(u10.property("LCSC"), Some("C8888888"));
        assert_eq!(u10.property("MPN"), Some("PLACED-PART"));
        assert_eq!(u10.property("Manufacturer"), Some("TestMfr"));
    }
}
