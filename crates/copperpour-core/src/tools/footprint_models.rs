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
         .kicad_mod footprint while preserving every non-model child.",
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
                            "rotate": xyz_schema([0.0, 0.0, 0.0])
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
}

async fn handle_set_footprint_models(
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
                ToolErrorKind::FileNotFound {
                    path: path.display().to_string(),
                },
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
                ToolErrorKind::FileNotFound {
                    path: path.display().to_string(),
                },
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
        "model_count": prepared.model_count
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
    let mut child_indent = None;
    for (start, end) in find_direct_child_blocks(source, "footprint") {
        let node = copperpour_sexp::parse_sexp(&source[start..end])
            .map_err(|_| invalid("footprint_path", "contains an invalid top-level item"))?;
        if child_indent.is_none() {
            child_indent = indent_before(source, start);
        }
        if node.head() == Some("model") {
            selected.push((start, end));
        }
    }

    let indent = selected
        .first()
        .and_then(|(start, _)| indent_before(source, *start))
        .or(child_indent)
        .unwrap_or_else(|| "  ".to_string());
    let serialized = serialize_models(&request.models, &indent);
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

fn serialize_models(models: &[FootprintModel], indent: &str) -> String {
    models
        .iter()
        .map(serialize_model)
        .collect::<Vec<_>>()
        .join(&format!("\n{indent}"))
}

fn serialize_model(model: &FootprintModel) -> String {
    format!(
        "(model {}\n    (offset (xyz {} {} {}))\n    (scale (xyz {} {} {}))\n    (rotate (xyz {} {} {})))",
        quote_string(&model.path),
        fmt_f64(model.offset.x),
        fmt_f64(model.offset.y),
        fmt_f64(model.offset.z),
        fmt_f64(model.scale.x),
        fmt_f64(model.scale.y),
        fmt_f64(model.scale.z),
        fmt_f64(model.rotate.x),
        fmt_f64(model.rotate.y),
        fmt_f64(model.rotate.z),
    )
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
        assert!(prepared.replacement.contains(
            "(model \"../models/new.step\"\n    (offset (xyz 0 0 0))\n    (scale (xyz 1 1 1))\n    (rotate (xyz 0 0 0)))"
        ));
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
            .contains("(offset (xyz 5.025 -7.925 -1.838))"));
        assert!(prepared.replacement.contains("(scale (xyz -1 1 1))"));
        assert!(prepared.replacement.contains("(rotate (xyz -90 0 0))"));
        assert!(prepared.replacement.contains("(rotate (xyz 90 0 0))"));
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
}
