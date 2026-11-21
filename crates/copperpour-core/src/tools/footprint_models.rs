use crate::mcp::error::ToolErrorKind;
use crate::mcp::protocol::CallToolResult;
use crate::tool;
use crate::tools::{ToolContext, ToolDef};
use copperpour_schematic_editor::types::fmt_f64;
use copperpour_sexp::writer::{
    apply_edits, find_balanced_block, find_block_with_leading_whitespace, find_direct_child_blocks,
    read_consistent, write_atomic_if_unchanged, SexpEdit,
};
use serde::Deserialize;
use serde_json::json;
use std::path::Path;

fn xyz_schema(default: [f64; 3]) -> serde_json::Value {
    json!({
        "type": "object",
        "properties": {
            "x": { "type": "number", "default": default[0] },
            "y": { "type": "number", "default": default[1] },
            "z": { "type": "number", "default": default[2] }
        },
        "required": ["x", "y", "z"],
        "additionalProperties": false
    })
}

pub(super) fn tool() -> ToolDef {
    tool!(
        "set_footprint_models",
        "Atomically append, replace, or delete top-level 3D model blocks in an existing \
         .kicad_mod footprint while preserving every non-model child. Each model takes \
         visible and opacity, written as KiCad writes them; replace keeps a replaced model's \
         visibility and opacity when the new model has its path and omits them, and warns \
         when a hidden or transparent model is dropped.",
        json!({
            "type": "object",
            "properties": {
                "footprint_path": {
                    "type": "string",
                    "description": "Path to an existing .kicad_mod footprint file"
                },
                "mode": {
                    "type": "string",
                    "enum": ["append", "replace", "delete"]
                },
                "models": {
                    "type": "array",
                    "items": {
                        "type": "object",
                        "properties": {
                            "path": { "type": "string" },
                            "offset": xyz_schema([0.0, 0.0, 0.0]),
                            "scale": xyz_schema([1.0, 1.0, 1.0]),
                            "rotate": xyz_schema([0.0, 0.0, 0.0]),
                            "visible": {
                                "type": "boolean",
                                "description": "Show the model in the 3D viewer (default true). In replace mode, omitted means: keep the visibility of the replaced model with the same path"
                            },
                            "opacity": {
                                "type": "number",
                                "minimum": 0.0,
                                "maximum": 1.0,
                                "description": "Model opacity from 0 to 1 (default 1). In replace mode, omitted means: keep the opacity of the replaced model with the same path"
                            }
                        },
                        "required": ["path"],
                        "additionalProperties": false
                    }
                }
            },
            "required": ["footprint_path", "mode"],
            "additionalProperties": false
        }),
        |args, ctx| async move { handle_set_footprint_models(args, ctx).await }
    )
}

#[derive(Debug, Clone, Copy, Deserialize, PartialEq)]
#[serde(rename_all = "lowercase")]
enum ModelMode {
    Append,
    Replace,
    Delete,
}

#[derive(Debug, Clone, Copy, Deserialize, PartialEq)]
struct Xyz {
    x: f64,
    y: f64,
    z: f64,
}

impl Xyz {
    const ZERO: Self = Self {
        x: 0.0,
        y: 0.0,
        z: 0.0,
    };
    const ONE: Self = Self {
        x: 1.0,
        y: 1.0,
        z: 1.0,
    };

    fn is_finite(self) -> bool {
        self.x.is_finite() && self.y.is_finite() && self.z.is_finite()
    }
}

fn default_zero() -> Xyz {
    Xyz::ZERO
}

fn default_one() -> Xyz {
    Xyz::ONE
}

#[derive(Debug, Clone, Deserialize, PartialEq)]
struct FootprintModel {
    path: String,
    #[serde(default = "default_zero")]
    offset: Xyz,
    #[serde(default = "default_one")]
    scale: Xyz,
    #[serde(default = "default_zero")]
    rotate: Xyz,
    /// Shown in the 3D viewer; `None` keeps a replaced model's, else shown.
    #[serde(default)]
    visible: Option<bool>,
    /// From 0 to 1; `None` keeps a replaced model's, else 1.
    #[serde(default)]
    opacity: Option<f64>,
}

/// A `(model …)` block as KiCad reads it (`PCB_IO_KICAD_SEXPR_PARSER::parse3DModel`,
/// `pcbnew/pcb_io/kicad_sexpr/pcb_io_kicad_sexpr_parser.cpp`, 10.0.8): a bare
/// `hide` or `(hide yes)` hides it, a legacy `(at (xyz …))` is an offset in
/// inches, and what is absent takes KiCad's default.
#[derive(Debug, Clone, PartialEq)]
pub(super) struct WrittenModel {
    pub path: String,
    pub offset: [f64; 3],
    pub scale: [f64; 3],
    pub rotate: [f64; 3],
    pub visible: bool,
    pub opacity: f64,
}

impl WrittenModel {
    pub(super) fn read(model: &copperpour_sexp::SexpNode) -> Option<Self> {
        let path = model.get(1)?.as_str()?.to_string();
        let mut written = WrittenModel {
            path,
            offset: [0.0; 3],
            scale: [1.0; 3],
            rotate: [0.0; 3],
            visible: true,
            opacity: 1.0,
        };
        let xyz = |node: &copperpour_sexp::SexpNode| -> Option<[f64; 3]> {
            let xyz = node.find("xyz")?;
            Some([xyz.get_f64(1)?, xyz.get_f64(2)?, xyz.get_f64(3)?])
        };
        for child in model.children().unwrap_or(&[]).iter().skip(2) {
            match (child.head(), child.as_str()) {
                (_, Some("hide")) => written.visible = false,
                // `parseMaybeAbsentBool( true )`: `(hide)` and `(hide yes)` hide it.
                (Some("hide"), _) => {
                    written.visible = child.get(1).and_then(|v| v.as_str()) == Some("no");
                }
                (Some("opacity"), _) => written.opacity = child.get_f64(1).unwrap_or(1.0),
                (Some("offset"), _) => written.offset = xyz(child).unwrap_or(written.offset),
                (Some("at"), _) => {
                    if let Some(inches) = xyz(child) {
                        written.offset = inches.map(|value| value * 25.4);
                    }
                }
                (Some("scale"), _) => written.scale = xyz(child).unwrap_or(written.scale),
                (Some("rotate"), _) => written.rotate = xyz(child).unwrap_or(written.rotate),
                _ => {}
            }
        }
        Some(written)
    }

    pub(super) fn to_json(&self) -> serde_json::Value {
        let xyz = |v: [f64; 3]| json!({ "x": v[0], "y": v[1], "z": v[2] });
        json!({
            "path": self.path,
            "offset": xyz(self.offset),
            "scale": xyz(self.scale),
            "rotate": xyz(self.rotate),
            "visible": self.visible,
            "opacity": self.opacity
        })
    }
}

/// Every `(model …)` of a footprint, in file order.
pub(super) fn written_models(footprint: &copperpour_sexp::SexpNode) -> Vec<WrittenModel> {
    footprint
        .children()
        .unwrap_or(&[])
        .iter()
        .filter(|child| child.head() == Some("model"))
        .filter_map(WrittenModel::read)
        .collect()
}

#[derive(Debug, Clone, PartialEq)]
struct ModelRequest {
    mode: ModelMode,
    models: Vec<FootprintModel>,
}

#[derive(Debug, Clone, PartialEq)]
struct ModelError {
    field: String,
    reason: String,
}

#[derive(Debug, Clone, PartialEq)]
struct PreparedMutation {
    replacement: String,
    matched_count: usize,
    added_count: usize,
    model_count: usize,
    warnings: Vec<String>,
}

pub(crate) async fn handle_set_footprint_models(
    args: &serde_json::Value,
    _ctx: &ToolContext,
) -> anyhow::Result<CallToolResult> {
    let Some(path) = args["footprint_path"]
        .as_str()
        .map(std::path::PathBuf::from)
    else {
        return Ok(invalid_result("footprint_path", "missing or not a string"));
    };
    if path.extension().and_then(|extension| extension.to_str()) != Some("kicad_mod") {
        return Ok(invalid_result("footprint_path", "must end in .kicad_mod"));
    }
    let metadata = match std::fs::metadata(&path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(CallToolResult::error_kind(
                ToolErrorKind::file_not_found(&path),
                format!("Footprint file not found: {}", path.display()),
            ));
        }
        Err(error) => return Err(error.into()),
    };
    if !metadata.is_file() {
        return Ok(invalid_result("footprint_path", "must name a regular file"));
    }

    let request = match parse_request(args) {
        Ok(request) => request,
        Err(error) => return Ok(invalid_result(&error.field, error.reason)),
    };
    let source = match read_consistent(&path) {
        Ok(source) => source,
        Err(copperpour_sexp::SexpError::Io(error))
            if error.kind() == std::io::ErrorKind::NotFound =>
        {
            return Ok(CallToolResult::error_kind(
                ToolErrorKind::file_not_found(&path),
                format!("Footprint file not found: {}", path.display()),
            ));
        }
        Err(error) => return Err(error.into()),
    };
    let prepared = match prepare_mutation(&source, &request) {
        Ok(prepared) => prepared,
        Err(error) => return Ok(invalid_result(&error.field, error.reason)),
    };
    if let Err(error) = persist_replacement(&path, &source, &prepared.replacement) {
        return match error {
            copperpour_sexp::SexpError::Conflict { .. } => Ok(conflict_result(&path)),
            other => Err(other.into()),
        };
    }

    Ok(CallToolResult::json(&json!({
        "success": true,
        "footprint_path": path.display().to_string(),
        "mode": args["mode"],
        "matched_count": prepared.matched_count,
        "added_count": prepared.added_count,
        "model_count": prepared.model_count,
        "warnings": prepared.warnings
    })))
}

fn parse_request(args: &serde_json::Value) -> Result<ModelRequest, ModelError> {
    let mode: ModelMode = serde_json::from_value(args["mode"].clone())
        .map_err(|error| invalid("mode", error.to_string()))?;
    let models = match args.get("models") {
        None if matches!(mode, ModelMode::Delete) => Vec::new(),
        None => {
            return Err(invalid(
                "models",
                "is required for append and replace modes",
            ))
        }
        Some(value) => serde_json::from_value::<Vec<FootprintModel>>(value.clone())
            .map_err(|error| invalid("models", error.to_string()))?,
    };
    match mode {
        ModelMode::Append | ModelMode::Replace if models.is_empty() => {
            return Err(invalid(
                "models",
                "must contain at least one model in append and replace modes",
            ))
        }
        ModelMode::Delete if !models.is_empty() => {
            return Err(invalid("models", "must be omitted or empty in delete mode"))
        }
        _ => {}
    }
    validate_models(&models)?;
    Ok(ModelRequest { mode, models })
}

fn validate_models(models: &[FootprintModel]) -> Result<(), ModelError> {
    for (index, model) in models.iter().enumerate() {
        if model.path.trim().is_empty() {
            return Err(invalid(
                "models",
                format!("models[{index}].path must not be empty"),
            ));
        }
        if model.path.contains('\0') {
            return Err(invalid(
                "models",
                format!("models[{index}].path contains a NUL character"),
            ));
        }
        if !model.offset.is_finite() || !model.scale.is_finite() || !model.rotate.is_finite() {
            return Err(invalid(
                "models",
                format!("models[{index}] transforms must contain only finite numbers"),
            ));
        }
        if let Some(opacity) = model.opacity {
            if !opacity.is_finite() || !(0.0..=1.0).contains(&opacity) {
                return Err(invalid(
                    "models",
                    format!("models[{index}].opacity must be a number from 0 to 1"),
                ));
            }
        }
    }
    Ok(())
}

fn prepare_mutation(source: &str, request: &ModelRequest) -> Result<PreparedMutation, ModelError> {
    validate_models(&request.models)?;
    let root = copperpour_sexp::parse_sexp(source)
        .map_err(|error| invalid("footprint_path", format!("invalid S-expression: {error}")))?;
    if root.head() != Some("footprint") {
        return Err(invalid(
            "footprint_path",
            crate::tools::footprint_root_reason(root.head()),
        ));
    }

    let mut selected = Vec::new();
    let mut existing = Vec::new();
    let mut child_indent = None;
    for (start, end) in find_direct_child_blocks(source, "footprint") {
        let node = copperpour_sexp::parse_sexp(&source[start..end])
            .map_err(|_| invalid("footprint_path", "contains an invalid top-level item"))?;
        if child_indent.is_none() {
            child_indent = indent_before(source, start);
        }
        if node.head() == Some("model") {
            selected.push((start, end));
            existing.extend(WrittenModel::read(&node));
        }
    }

    let indent = selected
        .first()
        .and_then(|(start, _)| indent_before(source, *start))
        .or(child_indent)
        .unwrap_or_else(|| "  ".to_string());
    let (models, warnings) = resolve_models(request, &existing);
    let serialized = serialize_models(&models, &indent);
    let matched_count = selected.len();
    let added_count = request.models.len();
    let model_count = match request.mode {
        ModelMode::Append => matched_count + added_count,
        ModelMode::Replace => added_count,
        ModelMode::Delete => 0,
    };
    let mut edits = Vec::new();

    match request.mode {
        ModelMode::Replace => {
            if let Some((first, rest)) = selected.split_first() {
                edits.push(SexpEdit::replace(first.0, first.1, serialized));
                for (start, _) in rest {
                    let range = find_block_with_leading_whitespace(source, *start)
                        .ok_or_else(|| invalid("footprint_path", "contains an unbalanced model"))?;
                    edits.push(SexpEdit::delete(range.0, range.1));
                }
            } else {
                insert_models_at_root(source, &serialized, &indent, &mut edits)?;
            }
        }
        ModelMode::Append => {
            if let Some((_, end)) = selected.last() {
                edits.push(SexpEdit::insert(*end, format!("\n{indent}{serialized}")));
            } else {
                insert_models_at_root(source, &serialized, &indent, &mut edits)?;
            }
        }
        ModelMode::Delete => {
            for (start, _) in &selected {
                let range = find_block_with_leading_whitespace(source, *start)
                    .ok_or_else(|| invalid("footprint_path", "contains an unbalanced model"))?;
                edits.push(SexpEdit::delete(range.0, range.1));
            }
        }
    }

    let replacement = apply_edits(source.to_string(), edits);
    let parsed = copperpour_sexp::parse_sexp(&replacement)
        .map_err(|error| invalid("models", format!("replacement does not parse: {error}")))?;
    if parsed.head() != Some("footprint") {
        return Err(invalid("models", "replacement changed the footprint root"));
    }

    Ok(PreparedMutation {
        replacement,
        matched_count,
        added_count: if matches!(request.mode, ModelMode::Delete) {
            0
        } else {
            added_count
        },
        model_count,
        warnings,
    })
}

fn insert_models_at_root(
    source: &str,
    serialized: &str,
    indent: &str,
    edits: &mut Vec<SexpEdit>,
) -> Result<(), ModelError> {
    let root_end = find_balanced_block(source, 0)
        .map(|range| range.1 - 1)
        .ok_or_else(|| invalid("footprint_path", "unbalanced footprint root"))?;
    let insertion = if source[..root_end].ends_with('\n') {
        format!("{indent}{serialized}\n")
    } else {
        format!("\n{indent}{serialized}\n")
    };
    edits.push(SexpEdit::insert(root_end, insertion));
    Ok(())
}

/// The models to write, each with its visibility and opacity settled (#715):
/// as given; in replace mode, when omitted, those of the replaced model with
/// the same path (each replaced model lends them once); else shown and opaque.
/// Warns for a replaced model that was hidden or transparent and lent nothing.
fn resolve_models(
    request: &ModelRequest,
    existing: &[WrittenModel],
) -> (Vec<FootprintModel>, Vec<String>) {
    let mut lent = vec![false; existing.len()];
    let mut models = Vec::with_capacity(request.models.len());
    for model in &request.models {
        let mut model = model.clone();
        let previous = (request.mode == ModelMode::Replace)
            .then(|| {
                existing
                    .iter()
                    .enumerate()
                    .find(|(index, old)| !lent[*index] && old.path == model.path)
            })
            .flatten();
        if let Some((index, old)) = previous {
            lent[index] = true;
            model.visible = model.visible.or(Some(old.visible));
            model.opacity = model.opacity.or(Some(old.opacity));
        }
        model.visible = model.visible.or(Some(true));
        model.opacity = model.opacity.or(Some(1.0));
        models.push(model);
    }
    let mut warnings = Vec::new();
    if request.mode == ModelMode::Replace {
        for (old, lent) in existing.iter().zip(lent) {
            if lent || (old.visible && old.opacity == 1.0) {
                continue;
            }
            let mut was = Vec::new();
            if !old.visible {
                was.push("hidden".to_string());
            }
            if old.opacity != 1.0 {
                was.push(format!("at opacity {}", fmt_f64(old.opacity)));
            }
            warnings.push(format!(
                "replaced model '{}' was {}; no new model has its path, so that is not kept",
                old.path,
                was.join(" and ")
            ));
        }
    }
    (models, warnings)
}

fn serialize_models(models: &[FootprintModel], indent: &str) -> String {
    let unit = if indent.contains('\t') { "\t" } else { "  " };
    models
        .iter()
        .map(|model| serialize_model(model, indent, unit))
        .collect::<Vec<_>>()
        .join(&format!("\n{indent}"))
}

/// A `(model …)` block as KiCad's footprint writer lays it out
/// (`PCB_IO_KICAD_SEXPR::format( const FOOTPRINT* )`, then `Prettify`, 10.0.8):
/// `(hide yes)` when hidden and `(opacity …)` with four decimals when not 1,
/// then each transform with its `(xyz …)` on its own line, one indent unit
/// deeper per level from the file's own `indent`.
fn serialize_model(model: &FootprintModel, indent: &str, unit: &str) -> String {
    let inner = format!("{indent}{unit}");
    let mut text = format!("(model {}", quote_string(&model.path));
    if model.visible == Some(false) {
        text.push_str(&format!("\n{inner}(hide yes)"));
    }
    if let Some(opacity) = model.opacity.filter(|opacity| *opacity != 1.0) {
        text.push_str(&format!("\n{inner}(opacity {opacity:.4})"));
    }
    for (name, xyz) in [
        ("offset", model.offset),
        ("scale", model.scale),
        ("rotate", model.rotate),
    ] {
        text.push_str(&format!(
            "\n{inner}({name}\n{inner}{unit}(xyz {} {} {})\n{inner})",
            fmt_f64(xyz.x),
            fmt_f64(xyz.y),
            fmt_f64(xyz.z)
        ));
    }
    text.push_str(&format!("\n{indent})"));
    text
}

fn indent_before(source: &str, offset: usize) -> Option<String> {
    let line_start = source[..offset]
        .rfind('\n')
        .map(|index| index + 1)
        .unwrap_or(0);
    let indent = &source[line_start..offset];
    indent
        .chars()
        .all(|character| matches!(character, ' ' | '\t'))
        .then(|| indent.to_string())
}

fn quote_string(value: &str) -> String {
    let mut escaped = String::with_capacity(value.len() + 2);
    escaped.push('"');
    for character in value.chars() {
        match character {
            '\\' => escaped.push_str("\\\\"),
            '"' => escaped.push_str("\\\""),
            '\n' => escaped.push_str("\\n"),
            '\r' => escaped.push_str("\\r"),
            '\t' => escaped.push_str("\\t"),
            other => escaped.push(other),
        }
    }
    escaped.push('"');
    escaped
}

fn invalid(field: &str, reason: impl Into<String>) -> ModelError {
    ModelError {
        field: field.to_string(),
        reason: reason.into(),
    }
}

fn invalid_result(field: &str, reason: impl Into<String>) -> CallToolResult {
    let reason = reason.into();
    CallToolResult::error_kind(
        ToolErrorKind::InvalidArgument {
            field: field.to_string(),
            reason: reason.clone(),
        },
        format!("Argument '{field}' is invalid: {reason}"),
    )
}

fn conflict_result(path: &Path) -> CallToolResult {
    CallToolResult::error_kind(
        ToolErrorKind::Conflict {
            paths: vec![path.display().to_string()],
        },
        "Footprint models were not changed because the file changed after it was read",
    )
}

fn persist_replacement(
    path: &Path,
    expected: &str,
    replacement: &str,
) -> Result<(), copperpour_sexp::SexpError> {
    write_atomic_if_unchanged(path, expected, replacement)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::router::ToolRouter;
    use crate::tools::{ServerConfig, ToolContext};
    use serde_json::json;
    use std::sync::Arc;

    const ONE_MODEL_FOOTPRINT: &str = r#"(footprint "Socket"
  (version 20240108)
  (generator "pcbnew")
  (layer "F.Cu")
  (descr "preserve me")
  (fp_line (start 0 0) (end 1 1) (stroke (width 0.1) (type default)) (layer "F.SilkS"))
  (pad "1" thru_hole circle (at 0 0) (size 2 2) (drill 1) (layers "*.Cu" "*.Mask"))
  (model "../models/old.step"
    (offset (xyz 1 2 3))
    (scale (xyz 1 1 1))
    (rotate (xyz 0 0 90)))
)
"#;

    /// Regression for #304: a pre-6.0 `(module ...)` root was refused with the
    /// generic message, which reads like a dead end when `kicad-cli fp upgrade`
    /// migrates the file in one command. The refusal stays; the message names
    /// the way out.
    #[test]
    fn module_root_refusal_names_the_migration() {
        let legacy = "(module TRIM_PART (layer F.Cu) (tedit 62174BF4)\n  (pad 1 smd rect (at 0 0) (size 1 1) (layers F.Cu)))\n";
        let request = ModelRequest {
            mode: ModelMode::Delete,
            models: Vec::new(),
        };
        let err = prepare_mutation(legacy, &request).unwrap_err();
        let msg = format!("{err:?}");
        assert!(
            msg.contains("pre-6.0"),
            "must name the legacy format: {msg}"
        );
        assert!(msg.contains("fp upgrade"), "must name the migration: {msg}");
        // Anything else that is not a footprint keeps the generic message.
        let err = prepare_mutation("(kicad_pcb)", &request).unwrap_err();
        let msg = format!("{err:?}");
        assert!(msg.contains("must be a footprint"), "{msg}");
        assert!(!msg.contains("fp upgrade"), "{msg}");
    }

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

    fn result_text(result: &crate::mcp::protocol::CallToolResult) -> String {
        match result.content.first() {
            Some(crate::mcp::protocol::ToolContent::Text { text }) => text.clone(),
            other => panic!("expected text content, got {other:?}"),
        }
    }

    fn model(path: &str) -> serde_json::Value {
        json!({"path": path})
    }

    #[test]
    fn schema_exposes_modes_and_xyz_model_fields() {
        let model_tool = tool();
        assert_eq!(model_tool.name, "set_footprint_models");
        assert_eq!(
            model_tool.input_schema["required"],
            json!(["footprint_path", "mode"])
        );
        assert_eq!(
            model_tool.input_schema["properties"]["mode"]["enum"],
            json!(["append", "replace", "delete"])
        );
        let item = &model_tool.input_schema["properties"]["models"]["items"];
        assert_eq!(item["required"], json!(["path"]));
        for transform in ["offset", "scale", "rotate"] {
            assert_eq!(
                item["properties"][transform]["required"],
                json!(["x", "y", "z"])
            );
        }
    }

    #[test]
    fn append_adds_a_defaulted_model_after_existing_models() {
        let request = parse_request(&json!({
            "mode": "append",
            "models": [model("../models/new.step")]
        }))
        .unwrap();

        let prepared = prepare_mutation(ONE_MODEL_FOOTPRINT, &request).unwrap();

        assert_eq!(prepared.matched_count, 1);
        assert_eq!(prepared.added_count, 1);
        assert_eq!(prepared.model_count, 2);
        assert!(
            prepared.replacement.find("../models/old.step").unwrap()
                < prepared.replacement.find("../models/new.step").unwrap()
        );
        assert!(
            prepared.replacement.contains(
                "  (model \"../models/new.step\"\n    (offset\n      (xyz 0 0 0)\n    )\n    \
                 (scale\n      (xyz 1 1 1)\n    )\n    (rotate\n      (xyz 0 0 0)\n    )\n  )"
            ),
            "KiCad's nested layout, in the file's two-space indent:\n{}",
            prepared.replacement
        );
        copperpour_sexp::parse_sexp(&prepared.replacement).unwrap();
    }

    #[test]
    fn replace_one_model_with_two_explicit_models() {
        let request = parse_request(&json!({
            "mode": "replace",
            "models": [
                {
                    "path": "../models/a.step",
                    "offset": {"x": 5.025, "y": -7.925, "z": -1.838},
                    "scale": {"x": -1, "y": 1, "z": 1},
                    "rotate": {"x": -90, "y": 0, "z": 0}
                },
                {
                    "path": "../models/b.step",
                    "rotate": {"x": 90, "y": 0, "z": 0}
                }
            ]
        }))
        .unwrap();

        let prepared = prepare_mutation(ONE_MODEL_FOOTPRINT, &request).unwrap();

        assert_eq!(prepared.matched_count, 1);
        assert_eq!(prepared.added_count, 2);
        assert_eq!(prepared.model_count, 2);
        assert!(!prepared.replacement.contains("../models/old.step"));
        assert!(prepared
            .replacement
            .contains("(offset\n      (xyz 5.025 -7.925 -1.838)\n    )"));
        assert!(prepared
            .replacement
            .contains("(scale\n      (xyz -1 1 1)\n    )"));
        assert!(prepared
            .replacement
            .contains("(rotate\n      (xyz -90 0 0)\n    )"));
        assert!(prepared
            .replacement
            .contains("(rotate\n      (xyz 90 0 0)\n    )"));
        assert!(prepared.replacement.contains("(descr \"preserve me\")"));
        assert!(prepared
            .replacement
            .contains("(fp_line (start 0 0) (end 1 1)"));
        assert!(prepared.replacement.contains("(pad \"1\" thru_hole circle"));
    }

    #[test]
    fn old_inline_header_accepts_inserting_the_first_model() {
        let source = r#"(footprint "Socket" (version 20221018) (generator pcbnew)
  (layer "F.Cu")
  (descr "old description")
  (pad "1" thru_hole circle (at 0 0) (size 2 2) (drill 1) (layers "*.Cu" "*.Mask"))
)
"#;
        let request = parse_request(&json!({
            "mode": "replace",
            "models": [model("../models/new.step")]
        }))
        .unwrap();

        let prepared = prepare_mutation(source, &request).unwrap();

        assert_eq!(prepared.replacement.matches("(footprint ").count(), 1);
        assert!(prepared.replacement.contains("../models/new.step"));
        copperpour_sexp::parse_sexp(&prepared.replacement).unwrap();
    }

    #[test]
    fn delete_removes_all_models_and_preserves_non_model_children() {
        let source = ONE_MODEL_FOOTPRINT.replace(
            "\n)",
            "\n  (model \"../models/second.step\"\n    (offset (xyz 0 0 0))\n    (scale (xyz 1 1 1))\n    (rotate (xyz 0 0 0)))\n)\n",
        );
        let request = parse_request(&json!({"mode": "delete", "models": []})).unwrap();

        let prepared = prepare_mutation(&source, &request).unwrap();

        assert_eq!(prepared.matched_count, 2);
        assert_eq!(prepared.added_count, 0);
        assert_eq!(prepared.model_count, 0);
        assert!(!prepared.replacement.contains("(model "));
        assert!(prepared.replacement.contains("(descr \"preserve me\")"));
        assert!(prepared.replacement.contains("(pad \"1\" thru_hole circle"));
        copperpour_sexp::parse_sexp(&prepared.replacement).unwrap();
    }

    #[test]
    fn rejects_invalid_mode_lists_empty_paths_and_non_finite_transforms() {
        for value in [
            json!({"mode": "append", "models": []}),
            json!({"mode": "replace"}),
            json!({"mode": "delete", "models": [model("x.step")]}),
            json!({"mode": "append", "models": [model("")]}),
        ] {
            assert!(parse_request(&value).is_err(), "{value}");
        }

        let mut request = parse_request(&json!({
            "mode": "append",
            "models": [model("x.step")]
        }))
        .unwrap();
        request.models[0].offset.x = f64::INFINITY;
        assert_eq!(
            prepare_mutation(ONE_MODEL_FOOTPRINT, &request)
                .unwrap_err()
                .field,
            "models"
        );
    }

    #[tokio::test]
    async fn handler_replaces_models_in_one_atomic_operation() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("Socket.kicad_mod");
        std::fs::write(&path, ONE_MODEL_FOOTPRINT).unwrap();

        let result = handle_set_footprint_models(
            &json!({
                "footprint_path": path,
                "mode": "replace",
                "models": [model("../models/new.step")]
            }),
            &test_ctx(),
        )
        .await
        .unwrap();

        assert!(!result.is_error, "{:?}", result.content);
        let body: serde_json::Value = serde_json::from_str(&result_text(&result)).unwrap();
        assert_eq!(body["matched_count"], 1);
        assert_eq!(body["added_count"], 1);
        assert_eq!(body["model_count"], 1);
        let updated = std::fs::read_to_string(path).unwrap();
        assert!(!updated.contains("../models/old.step"));
        assert!(updated.contains("../models/new.step"));
    }

    #[test]
    fn stale_source_is_rejected_without_overwriting_the_newer_file() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("Socket.kicad_mod");
        std::fs::write(&path, ONE_MODEL_FOOTPRINT).unwrap();
        let request = parse_request(&json!({
            "mode": "replace",
            "models": [model("../models/new.step")]
        }))
        .unwrap();
        let prepared = prepare_mutation(ONE_MODEL_FOOTPRINT, &request).unwrap();

        let newer = ONE_MODEL_FOOTPRINT.replace("(layer \"F.Cu\")", "(layer \"B.Cu\")");
        std::fs::write(&path, &newer).unwrap();

        let error = persist_replacement(&path, ONE_MODEL_FOOTPRINT, &prepared.replacement)
            .expect_err("a stale expected source must conflict");
        assert!(matches!(error, copperpour_sexp::SexpError::Conflict { .. }));
        assert_eq!(std::fs::read_to_string(path).unwrap(), newer);
    }

    // KiCad 10.0.8 wrote each from the stock Winbond USON-8, one change to its
    // models through FP_3DMODEL; provenance in
    // tests/fixtures/footprint_models_kicad10.README.md.
    const SOURCE: &str =
        include_str!("../../tests/fixtures/footprint_models_source_kicad10.kicad_mod");
    const HIDDEN: &str =
        include_str!("../../tests/fixtures/footprint_models_hidden_kicad10.kicad_mod");
    const TWO_MODELS: &str =
        include_str!("../../tests/fixtures/footprint_models_two_models_kicad10.kicad_mod");
    const STOCK_MODEL: &str =
        "${KICAD10_3DMODEL_DIR}/Package_SON.3dshapes/Winbond_USON-8-1EP_3x2mm_P0.5mm_EP0.2x1.6mm.step";

    async fn set_models(source: &str, arguments: serde_json::Value) -> (serde_json::Value, String) {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("Winbond.kicad_mod");
        std::fs::write(&path, source).unwrap();
        let mut arguments = arguments;
        arguments["footprint_path"] = json!(path);
        let result = handle_set_footprint_models(&arguments, &test_ctx())
            .await
            .unwrap();
        let body: serde_json::Value = serde_json::from_str(&result_text(&result)).unwrap();
        (body, std::fs::read_to_string(&path).unwrap())
    }

    fn masked(text: &str) -> String {
        crate::tools::solder_margins::without_uuids(text)
    }

    /// Each result equals the file KiCad wrote for the same change, byte for
    /// byte with UUIDs masked, so `kicad-cli fp upgrade --force` keeps it.
    #[tokio::test]
    async fn models_are_written_as_kicad_writes_them() {
        let cases = [
            (
                "hidden and transparent",
                SOURCE,
                json!({ "mode": "replace", "models": [
                    { "path": STOCK_MODEL, "visible": false, "opacity": 0.5 }] }),
                HIDDEN,
            ),
            (
                "kept when omitted",
                HIDDEN,
                json!({ "mode": "replace", "models": [{ "path": STOCK_MODEL }] }),
                HIDDEN,
            ),
            (
                "shown again",
                HIDDEN,
                json!({ "mode": "replace", "models": [
                    { "path": STOCK_MODEL, "visible": true, "opacity": 1 }] }),
                SOURCE,
            ),
            (
                "appended",
                SOURCE,
                json!({ "mode": "append", "models": [{
                    "path": "${KIPRJMOD}/models/extra.step",
                    "offset": { "x": 0.1, "y": -0.2, "z": 0 },
                    "rotate": { "x": 0, "y": 0, "z": 90 } }] }),
                TWO_MODELS,
            ),
        ];
        for (case, source, arguments, kicad) in cases {
            let (body, written) = set_models(source, arguments).await;
            assert_eq!(body["success"], true, "{case}: {body}");
            assert_eq!(masked(&written), masked(kicad), "{case}");
            assert_eq!(body["warnings"], json!([]), "{case}");
        }
    }

    /// A replaced hidden or transparent model whose path no new model has is
    /// not silently dropped: the answer says what was lost.
    #[tokio::test]
    async fn dropping_a_hidden_model_is_warned() {
        let (body, written) = set_models(
            HIDDEN,
            json!({ "mode": "replace", "models": [{ "path": "${KIPRJMOD}/other.step" }] }),
        )
        .await;
        assert_eq!(
            body["warnings"],
            json!([format!(
                "replaced model '{STOCK_MODEL}' was hidden and at opacity 0.5; no new model \
                 has its path, so that is not kept"
            )])
        );
        let model = &written[written.find("(model").unwrap()..];
        assert!(
            !model.contains("(hide yes)") && !model.contains("(opacity"),
            "{model}"
        );
        let (body, _) = set_models(
            SOURCE,
            json!({ "mode": "replace", "models": [{ "path": "${KIPRJMOD}/other.step" }] }),
        )
        .await;
        assert_eq!(
            body["warnings"],
            json!([]),
            "a shown, opaque model warns nothing"
        );
    }

    /// A replaced model lends its visibility and opacity once: of two new
    /// models with its path that set neither, the first takes them and the
    /// second is written shown and opaque (#829's review).
    #[tokio::test]
    async fn a_replaced_model_lends_its_values_once() {
        let (body, written) = set_models(
            HIDDEN,
            json!({ "mode": "replace", "models": [
                { "path": STOCK_MODEL }, { "path": STOCK_MODEL }] }),
        )
        .await;
        assert_eq!(body["success"], true, "{body}");
        assert_eq!(body["warnings"], json!([]), "{body}");
        let models: Vec<&str> = written.split("(model ").skip(1).collect();
        assert_eq!(models.len(), 2, "{written}");
        assert!(
            models[0].contains("(hide yes)") && models[0].contains("(opacity 0.5000)"),
            "first: {}",
            models[0]
        );
        assert!(
            !models[1].contains("(hide yes)") && !models[1].contains("(opacity"),
            "second: {}",
            models[1]
        );
    }

    #[test]
    fn visible_and_opacity_are_checked() {
        for model in [
            json!({ "path": "x.step", "opacity": 1.5 }),
            json!({ "path": "x.step", "opacity": -0.1 }),
            json!({ "path": "x.step", "visible": "no" }),
            json!({ "path": "x.step", "opacity": "half" }),
        ] {
            let error = parse_request(&json!({ "mode": "append", "models": [model] })).unwrap_err();
            assert_eq!(error.field, "models", "{model}");
        }
        let schema = &tool().input_schema["properties"]["models"]["items"]["properties"];
        assert_eq!(schema["visible"]["type"], "boolean");
        assert_eq!(schema["opacity"]["maximum"], 1.0);
    }

    /// Every form KiCad reads, as it reads them.
    #[test]
    fn written_models_read_as_kicad_reads_them() {
        let read =
            |text: &str| WrittenModel::read(&copperpour_sexp::parse_sexp(text).unwrap()).unwrap();
        assert!(
            !read(r#"(model "a" hide (offset (xyz 0 0 0)))"#).visible,
            "bare hide"
        );
        assert!(!read(r#"(model "a" (hide yes))"#).visible);
        assert!(!read(r#"(model "a" (hide))"#).visible);
        assert!(read(r#"(model "a" (hide no))"#).visible);
        assert!(read(r#"(model "a")"#).visible);
        let model = read(
            r#"(model "a" (opacity 0.25) (at (xyz 1 0 -0.5)) (scale (xyz 2 2 2)) (rotate (xyz 0 0 45)))"#,
        );
        assert_eq!(model.opacity, 0.25);
        assert_eq!(model.offset, [25.4, 0.0, -12.7], "a legacy at is in inches");
        assert_eq!((model.scale, model.rotate), ([2.0; 3], [0.0, 0.0, 45.0]));
    }

    /// `get_footprint_info` lists each model, so a caller can read before it replaces.
    #[tokio::test]
    async fn get_footprint_info_lists_the_models() {
        let tmp = tempfile::tempdir().unwrap();
        let library = tmp.path().join("Test.pretty");
        std::fs::create_dir(&library).unwrap();
        std::fs::write(library.join("Winbond.kicad_mod"), TWO_MODELS).unwrap();
        std::fs::write(library.join("Hidden.kicad_mod"), HIDDEN).unwrap();
        let handler = crate::mcp::handler::McpHandler::new(crate::tools::ServerConfig {
            eager_toolsets: true,
            ..crate::tools::ServerConfig::default()
        })
        .await
        .unwrap();
        let mut models = Vec::new();
        for name in ["Winbond", "Hidden"] {
            let response = handler
                .handle_message(json!({
                    "jsonrpc": "2.0", "id": 715, "method": "tools/call",
                    "params": { "name": "get_footprint_info", "arguments": {
                        "footprint_path": library.join(format!("{name}.kicad_mod")) } }
                }))
                .await
                .unwrap();
            let result = response.result.unwrap();
            assert_ne!(result["isError"], json!(true), "{result}");
            let body: serde_json::Value =
                serde_json::from_str(result["content"][0]["text"].as_str().unwrap()).unwrap();
            models.push(body["models"].clone());
        }
        let model =
            |path: &str, offset: [f64; 3], rotate: [f64; 3], visible: bool, opacity: f64| {
                json!({ "path": path,
                "offset": { "x": offset[0], "y": offset[1], "z": offset[2] },
                "scale": { "x": 1.0, "y": 1.0, "z": 1.0 },
                "rotate": { "x": rotate[0], "y": rotate[1], "z": rotate[2] },
                "visible": visible, "opacity": opacity })
            };
        assert_eq!(
            models[0],
            json!([
                model(STOCK_MODEL, [0.0; 3], [0.0; 3], true, 1.0),
                model(
                    "${KIPRJMOD}/models/extra.step",
                    [0.1, -0.2, 0.0],
                    [0.0, 0.0, 90.0],
                    true,
                    1.0
                )
            ])
        );
        assert_eq!(
            models[1],
            json!([model(STOCK_MODEL, [0.0; 3], [0.0; 3], false, 0.5)])
        );
    }
}
