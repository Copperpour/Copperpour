//! Generate deliberately approximate package solids; attachment remains an
//! explicit call to the existing library/live-footprint model tools.
use crate::mcp::{error::ToolErrorKind, protocol::CallToolResult};
use crate::model_3d::{self, BoxSolid};
use crate::tool;
use crate::tools::{get_path, invalid_arg, require_f64, require_str, ToolContext, ToolDef};
use copperpour_sexp::{parse_sexp, writer::read_consistent, SexpNode};
use serde_json::{json, Value};
use std::path::Path;

pub fn tools() -> Vec<ToolDef> {
    let dimension = json!({"type":"number","minimum":0.001,"maximum":10000});
    let coordinate = json!({"type":"number","minimum":-10000,"maximum":10000});
    let color = json!({"type":"array","minItems":3,"maxItems":3,"items":{"type":"number","minimum":0,"maximum":1},"description":"RGB components from 0 to 1"});
    vec![tool!("create_3d_model",
        "Create a new AP214 STEP approximation from axis-aligned body and pin-row boxes, in millimetres with X right, Y up, Z above the PCB surface. No CAD kernel, network or KiCad runtime is required. Body dimensions are explicit or read from an unambiguous axis-aligned rectangular F.Fab outline in a saved .kicad_mod (F.CrtYd only if F.Fab has no outline). Does not modify or attach to the footprint. Refuses existing output paths; parent directory must exist. Separate solids are not fused and are not a manufacturing-accurate model.",
        json!({"type":"object","properties":{
            "output":{"type":"string","description":"New .step or .stp file; existing paths are refused"},
            "name":{"type":"string","description":"1..128 printable ASCII characters, excluding backslash; default package"},
            "body":{"type":"object","properties":{
                "width_mm":dimension,"length_mm":dimension,"height_mm":dimension,
                "footprint_path":{"type":"string","description":"Saved library .kicad_mod; mutually exclusive with explicit width_mm/length_mm"},
                "standoff_mm":{"type":"number","minimum":0,"maximum":10000,"default":0},
                "color":color
            },"required":["height_mm"],"oneOf":[{"required":["width_mm","length_mm"],"not":{"required":["footprint_path"]}},{"required":["footprint_path"],"not":{"anyOf":[{"required":["width_mm"]},{"required":["length_mm"]}]}}]},
            "pin_rows":{"type":"array","maxItems":16,"description":"Optional rows centred at the supplied XY coordinates; at most 256 pins in total","items":{"type":"object","properties":{
                "axis":{"type":"string","enum":["x","y"]},
                "count":{"type":"integer","minimum":1,"maximum":128},
                "pitch_mm":dimension,"width_mm":dimension,"length_mm":dimension,"height_mm":dimension,
                "center_x_mm":coordinate,"center_y_mm":coordinate,"bottom_z_mm":coordinate,
                "color":color
            },"required":["axis","count","pitch_mm","width_mm","length_mm","height_mm","center_x_mm","center_y_mm"]}}
        },"required":["output","body"]}),
        |args,ctx| async move { handle_create_3d_model(args,ctx).await })]
}

fn number(obj: &Value, key: &str, default: Option<f64>) -> Result<f64, CallToolResult> {
    if obj.get(key).is_none() {
        if let Some(default) = default {
            return Ok(default);
        }
    }
    let value = require_f64(obj, key)?;
    if !value.is_finite() {
        return Err(invalid_arg(key, "must be finite"));
    }
    Ok(value)
}
fn dimension(obj: &Value, key: &str) -> Result<f64, CallToolResult> {
    let value = number(obj, key, None)?;
    if !(0.001..=10_000.).contains(&value) {
        return Err(invalid_arg(key, "must be between 0.001 and 10000 mm"));
    }
    Ok(value)
}
fn color(obj: &Value, default: [f64; 3]) -> Result<[f64; 3], CallToolResult> {
    let Some(value) = obj.get("color") else {
        return Ok(default);
    };
    let Some(array) = value.as_array().filter(|a| a.len() == 3) else {
        return Err(invalid_arg(
            "color",
            "must be an RGB array of three numbers from 0 to 1",
        ));
    };
    let mut rgb = [0.; 3];
    for (i, v) in array.iter().enumerate() {
        rgb[i] = v
            .as_f64()
            .filter(|v| v.is_finite() && (0.0..=1.).contains(v))
            .ok_or_else(|| invalid_arg("color", "components must be finite numbers from 0 to 1"))?;
    }
    Ok(rgb)
}

struct Plan {
    name: String,
    solids: Vec<BoxSolid>,
    source: Value,
}
fn plan(args: &Value) -> Result<Plan, CallToolResult> {
    let name = if args.get("name").is_some() {
        require_str(args, "name")?
    } else {
        "package"
    };
    model_3d::step_string(name).map_err(|e| invalid_arg("name", &e))?;
    let body = &args["body"];
    if !body.is_object() {
        return Err(invalid_arg("body", "must be an object"));
    }
    let height = dimension(body, "height_mm")?;
    let standoff = number(body, "standoff_mm", Some(0.))?;
    if !(0.0..=10_000.).contains(&standoff) {
        return Err(invalid_arg("standoff_mm", "must be between 0 and 10000 mm"));
    }
    let (min, max, source) = if body.get("footprint_path").is_some() {
        if body.get("width_mm").is_some() || body.get("length_mm").is_some() {
            return Err(invalid_arg(
                "body",
                "footprint_path and explicit dimensions are mutually exclusive",
            ));
        }
        let path = Path::new(require_str(body, "footprint_path")?);
        footprint_rectangle(path)?
    } else {
        let width = dimension(body, "width_mm")?;
        let length = dimension(body, "length_mm")?;
        (
            [-width / 2., -length / 2.],
            [width / 2., length / 2.],
            json!({"mode":"explicit"}),
        )
    };
    let mut solids = vec![BoxSolid {
        name: "body".into(),
        min: [min[0], min[1], standoff],
        max: [max[0], max[1], standoff + height],
        color: color(body, [0.15; 3])?,
    }];
    if let Some(rows) = args.get("pin_rows") {
        let rows = rows
            .as_array()
            .filter(|r| r.len() <= 16)
            .ok_or_else(|| invalid_arg("pin_rows", "must be an array of at most 16 rows"))?;
        for (row_idx, row) in rows.iter().enumerate() {
            if !row.is_object() {
                return Err(invalid_arg("pin_rows", "each row must be an object"));
            }
            let axis = match require_str(row, "axis")? {
                "x" => 0,
                "y" => 1,
                _ => return Err(invalid_arg("axis", "must be x or y")),
            };
            let count = match crate::tools::require_u64(row, "count") {
                Ok(count) => count,
                Err(error) => {
                    // JSON Schema accepts integral floating-point numbers as
                    // integers too. Bound before narrowing; never truncate.
                    let value = require_f64(row, "count")?;
                    if !value.is_finite() || value.fract() != 0.0 || !(1.0..=128.0).contains(&value)
                    {
                        return Err(error);
                    }
                    value as u64
                }
            };
            if !(1..=128).contains(&count) || solids.len() + count as usize > 257 {
                return Err(invalid_arg(
                    "count",
                    "each row requires 1..128 pins and the model at most 256 pins",
                ));
            }
            let pitch = dimension(row, "pitch_mm")?;
            let size = [
                dimension(row, "width_mm")?,
                dimension(row, "length_mm")?,
                dimension(row, "height_mm")?,
            ];
            let center = [
                number(row, "center_x_mm", None)?,
                number(row, "center_y_mm", None)?,
            ];
            let z = number(row, "bottom_z_mm", Some(0.))?;
            let rgb = color(row, [0.75; 3])?;
            for pin in 0..count {
                let mut p = center;
                p[axis] += (pin as f64 - (count - 1) as f64 / 2.) * pitch;
                solids.push(BoxSolid {
                    name: format!("pin_{}_{}", row_idx + 1, pin + 1),
                    min: [p[0] - size[0] / 2., p[1] - size[1] / 2., z],
                    max: [p[0] + size[0] / 2., p[1] + size[1] / 2., z + size[2]],
                    color: rgb,
                });
            }
        }
    }
    for (index, solid) in solids.iter().enumerate() {
        solid
            .validate()
            .map_err(|e| invalid_arg(if index == 0 { "body" } else { "pin_rows" }, &e))?;
    }
    Ok(Plan {
        name: name.into(),
        solids,
        source,
    })
}

fn point(node: &SexpNode, tag: &str) -> Result<[f64; 2], String> {
    let p = node
        .find(tag)
        .ok_or_else(|| format!("outline is missing {tag}"))?;
    let x = p
        .get_f64(1)
        .filter(|v| v.is_finite())
        .ok_or("invalid outline X coordinate")?;
    let y = p
        .get_f64(2)
        .filter(|v| v.is_finite())
        .ok_or("invalid outline Y coordinate")?;
    Ok([x, y])
}
type Rectangle = ([f64; 2], [f64; 2]);

fn rectangle_on_layer(root: &SexpNode, layer: &str) -> Result<Option<Rectangle>, String> {
    let mut edges = Vec::new();
    for node in root.children().unwrap_or(&[]) {
        if node.find_str("layer") != Some(layer) {
            continue;
        }
        match node.head() {
            Some("fp_line") => edges.push((point(node, "start")?, point(node, "end")?)),
            Some("fp_rect") => {
                let a = point(node, "start")?;
                let b = point(node, "end")?;
                edges.extend([
                    (a, [b[0], a[1]]),
                    ([b[0], a[1]], b),
                    (b, [a[0], b[1]]),
                    ([a[0], b[1]], a),
                ]);
            }
            Some("fp_arc" | "fp_circle" | "fp_poly" | "fp_curve") => {
                return Err(format!(
                    "{layer} has unsupported non-rectangular outline graphics"
                ))
            }
            _ => {}
        }
    }
    if edges.is_empty() {
        return Ok(None);
    }
    if edges.len() != 4 {
        return Err(format!(
            "{layer} must contain exactly one rectangle or four closed rectangle edges"
        ));
    }
    let mut min = [f64::INFINITY; 2];
    let mut max = [f64::NEG_INFINITY; 2];
    for (a, b) in &edges {
        for p in [a, b] {
            for i in 0..2 {
                min[i] = min[i].min(p[i]);
                max[i] = max[i].max(p[i]);
            }
        }
    }
    let corners = [min, [max[0], min[1]], max, [min[0], max[1]]];
    for i in 0..4 {
        let a = corners[i];
        let b = corners[(i + 1) % 4];
        if a == b
            || edges
                .iter()
                .filter(|(u, v)| (*u == a && *v == b) || (*u == b && *v == a))
                .count()
                != 1
        {
            return Err(format!(
                "{layer} is not a single closed axis-aligned rectangle"
            ));
        }
    }
    // Footprint XY is Y-down; model XY is Y-up, matching KiCad 3D model offsets.
    Ok(Some(([min[0], -max[1]], [max[0], -min[1]])))
}
fn footprint_rectangle(path: &Path) -> Result<([f64; 2], [f64; 2], Value), CallToolResult> {
    if path.extension().and_then(|e| e.to_str()) != Some("kicad_mod") {
        return Err(invalid_arg(
            "footprint_path",
            "must name a saved .kicad_mod library footprint",
        ));
    }
    let text = read_consistent(path).map_err(|e| {
        if matches!(&e, copperpour_sexp::SexpError::Io(io) if io.kind() == std::io::ErrorKind::NotFound) {
            return CallToolResult::error_kind(ToolErrorKind::file_not_found(path),format!("Footprint not found: {}",path.display()));
        }
        CallToolResult::error_kind(
            ToolErrorKind::HandlerError {
                reason: e.to_string(),
            },
            e.to_string(),
        )
    })?;
    let root = parse_sexp(&text).map_err(|e| invalid_arg("footprint_path", &e.to_string()))?;
    if root.head() != Some("footprint") {
        return Err(invalid_arg(
            "footprint_path",
            &crate::tools::footprint_root_reason(root.head()),
        ));
    }
    for layer in ["F.Fab", "F.CrtYd"] {
        if let Some((min, max)) =
            rectangle_on_layer(&root, layer).map_err(|e| invalid_arg("footprint_path", &e))?
        {
            return Ok((
                min,
                max,
                json!({"mode":"footprint_outline","footprint_path":path,"layer":layer,"state":"saved"}),
            ));
        }
    }
    Err(invalid_arg(
        "footprint_path",
        "no rectangular F.Fab or F.CrtYd outline was found",
    ))
}

async fn handle_create_3d_model(
    args: &Value,
    _ctx: &ToolContext,
) -> anyhow::Result<CallToolResult> {
    create_with_writer(args, |path, text| {
        crate::tools::write_if_unchanged(
            path,
            "STEP model",
            "Create 3D model",
            None,
            text.to_owned(),
        )
    })
}

fn create_with_writer<F>(args: &Value, write: F) -> anyhow::Result<CallToolResult>
where
    F: FnOnce(&Path, &str) -> anyhow::Result<Option<CallToolResult>>,
{
    let path = get_path(args, "output")?;
    if !path
        .extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| matches!(e.to_ascii_lowercase().as_str(), "step" | "stp"))
    {
        return Ok(invalid_arg("output", "must end in .step or .stp"));
    }
    let plan = match plan(args) {
        Ok(p) => p,
        Err(e) => return Ok(e),
    };
    let text = match model_3d::write_step(&plan.name, &plan.solids) {
        Ok(t) => t,
        Err(e) => return Ok(invalid_arg("body", &e)),
    };
    match write(&path, &text) {
        Ok(Some(result)) => return Ok(result),
        Ok(None) => {}
        Err(error) if path.exists() => {
            return Ok(crate::tools::file_outcome_uncertain(
                &path,
                "Create 3D model",
                error.to_string(),
            ))
        }
        Err(error) => return Err(error),
    }
    let boxes: Vec<_> = plan
        .solids
        .iter()
        .map(|s| json!({"name":s.name,"min_mm":s.min,"max_mm":s.max,"color":s.color}))
        .collect();
    Ok(CallToolResult::json(
        &json!({"success":true,"output":path,"format":"step_ap214","units":"mm","approximation":"independent_axis_aligned_boxes","solid_count":boxes.len(),"solids":boxes,"body_source":plan.source,"attached":false,"verification":{"file_readback":"matches_generated_step","cad_kernel":"not_run"}}),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    fn args(path: &Path) -> Value {
        json!({"output":path,"body":{"width_mm":4.,"length_mm":6.,"height_mm":2.,"standoff_mm":0.3},
        "pin_rows":[{"axis":"x","count":3,"pitch_mm":1.5,"width_mm":0.4,"length_mm":0.8,"height_mm":0.3,"center_x_mm":0.,"center_y_mm":-3.4}]})
    }
    #[test]
    fn step_model_pin_rows_are_centred_and_bounds_are_checked_before_write() {
        let a = args(Path::new("unused.step"));
        let p = plan(&a).unwrap();
        assert_eq!(p.solids.len(), 4);
        assert_eq!(p.solids[0].min, [-2., -3., 0.3]);
        assert_eq!(p.solids[1].min, [-1.7, -3.8, 0.]);
        assert_eq!(p.solids[3].max, [1.7, -3., 0.3]);
        let mut a = a.clone();
        a["pin_rows"][0]["axis"] = json!("y");
        let p = plan(&a).unwrap();
        assert_eq!(p.solids[1].min[0], -0.2);
        assert!((p.solids[1].min[1] + 5.3).abs() < 1e-12);
        assert_eq!(p.solids[1].min[2], 0.);
        for (field, value) in [
            ("count", json!(1.5)),
            ("count", json!(129)),
            ("count", json!(0)),
            ("pitch_mm", json!(0)),
            ("center_x_mm", json!(10001)),
            ("color", json!([1, 2, 3])),
        ] {
            let mut a = args(Path::new("unused.step"));
            a["pin_rows"][0][field] = value;
            assert!(
                create_with_writer(&a, |_, _| panic!("invalid geometry reached writer"))
                    .unwrap()
                    .is_error,
                "{field}"
            );
        }
    }
    #[test]
    fn step_model_total_pin_limit_names_count_before_publication() {
        let mut a = args(Path::new("unused.step"));
        let mut row = a["pin_rows"][0].clone();
        row["count"] = json!(128);
        a["pin_rows"] = json!([row.clone(), row.clone()]);
        assert_eq!(
            plan(&a)
                .unwrap_or_else(|_| panic!("256 pins must be accepted"))
                .solids
                .len(),
            257
        );
        a["pin_rows"].as_array_mut().unwrap().push(row);
        let result = create_with_writer(&a, |_, _| panic!("excess pins reached writer")).unwrap();
        assert!(result.is_error);
        let crate::mcp::protocol::ToolContent::Text { text } = &result.content[0] else {
            panic!("expected structured error")
        };
        let body: Value = serde_json::from_str(text).unwrap();
        assert_eq!(body["error"]["kind"], "invalid_argument");
        assert_eq!(body["error"]["field"], "count");
    }

    #[test]
    fn step_model_rectangles_require_a_closed_unambiguous_outline() {
        let root = parse_sexp("(footprint X (fp_rect (start 1 2)(end 5 8)(layer F.Fab)))").unwrap();
        assert_eq!(
            rectangle_on_layer(&root, "F.Fab").unwrap(),
            Some(([1., -8.], [5., -2.]))
        );
        for graphics in [
            "(fp_line(start 0 0)(end 1 1)(layer F.Fab))",
            "(fp_rect(start 1 2)(end 5 8)(layer F.Fab))(fp_line(start 1 3)(end 2 2)(layer F.Fab))",
            "(fp_rect(start 0 0)(end 1 1)(layer F.Fab))(fp_rect(start 2 2)(end 3 3)(layer F.Fab))",
            "(fp_circle(center 0 0)(end 1 0)(layer F.Fab))",
            "(fp_rect(start nan 0)(end 1 1)(layer F.Fab))",
            "(fp_rect(start 0 0)(end 1 1)(layer F.Fab))(fp_arc(layer F.Fab))",
        ] {
            let root = parse_sexp(&format!("(footprint X {graphics})")).unwrap();
            assert!(rectangle_on_layer(&root, "F.Fab").is_err());
        }
        let root=parse_sexp("(footprint X (fp_line(start 1 2)(end 5 2)(layer F.Fab))(fp_line(start 5 8)(end 1 8)(layer F.Fab))(fp_line(start 1 8)(end 1 2)(layer F.Fab))(fp_line(start 5 2)(end 5 8)(layer F.Fab)))").unwrap();
        assert_eq!(
            rectangle_on_layer(&root, "F.Fab").unwrap(),
            Some(([1., -8.], [5., -2.]))
        );
    }
    #[test]
    fn step_model_post_write_readback_failure_is_uncertain_and_preserved() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("model.step");
        let result = create_with_writer(&args(&path), |p, text| {
            copperpour_sexp::writer::write_new_atomic_verified(p, text)?;
            crate::tools::replace_outcome(
                p,
                "STEP model",
                "Create 3D model",
                Err(copperpour_sexp::SexpError::ReadbackMismatch { path: p.into() }),
            )
        })
        .unwrap();
        assert!(result.is_error);
        assert!(path.is_file());
        let crate::mcp::protocol::ToolContent::Text { text } = &result.content[0] else {
            panic!()
        };
        assert_eq!(
            serde_json::from_str::<Value>(text).unwrap()["error"]["kind"],
            "mutation_outcome_uncertain"
        );
    }
    #[test]
    fn step_model_writer_error_after_publication_is_uncertain_and_preserved() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("model.step");
        let mut expected = String::new();
        let result = create_with_writer(&args(&path), |p, text| {
            expected = text.to_owned();
            copperpour_sexp::writer::write_new_atomic_verified(p, text)?;
            anyhow::bail!("injected error after publication")
        })
        .unwrap();
        assert!(result.is_error);
        assert_eq!(std::fs::read_to_string(&path).unwrap(), expected);
        let crate::mcp::protocol::ToolContent::Text { text } = &result.content[0] else {
            panic!("expected structured error")
        };
        let body: Value = serde_json::from_str(text).unwrap();
        assert_eq!(body["error"]["kind"], "mutation_outcome_uncertain");
    }
}
