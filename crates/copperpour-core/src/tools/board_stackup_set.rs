//! `set_board_stackup`: replace the physical stackup of a board KiCad does not
//! hold open (#78).
//!
//! KiCad 10 declares `UpdateBoardStackup` but does not implement it, so the
//! stack is written into the board file as KiCad writes one
//! (`BOARD_STACKUP::FormatBoardStackup`,
//! `pcbnew/board_stackup_manager/board_stackup.cpp`), with the board
//! thickness KiCad sums from it. KiCad reads an entry whose name is not an
//! enabled layer as a dielectric and says nothing
//! (`PCB_IO_KICAD_SEXPR_PARSER::parseBoardStackup`), so kicad-cli first loads
//! and saves a copy, and the write is refused unless KiCad gives back every
//! layer, value and the thickness. Once a KiCad release handles
//! `UpdateBoardStackup`, a live board goes through it and this write stays for
//! a closed one.

use crate::mcp::error::ToolErrorKind;
use crate::mcp::protocol::CallToolResult;
use crate::tool;
use crate::tools::board_stackup::{mm, unspecified, THICKNESS_TOLERANCE_MM};
use crate::tools::pcb_board::{refuse_if_board_open_in_kicad, FILE_ONLY_EDIT_WARNING};
use crate::tools::pcb_board_outline::mm_text;
use crate::tools::{
    file_outcome_uncertain, get_path, invalid_arg, mutation_outcome_uncertain,
    replace_if_unchanged, require_array, BoardAccess, ToolContext, ToolDef,
};
use anyhow::Context;
use copperpour_sexp::layers::{self, CopperStack};
use copperpour_sexp::parser::{parse_sexp, SexpNode};
use copperpour_sexp::title_block::quote;
use copperpour_sexp::writer::{
    apply_edits, direct_children_with_tag, find_block_starts, read_consistent, SexpEdit,
};
use serde_json::{json, Value};
use std::path::Path;

const OPERATION: &str = "set_board_stackup";

/// KiCad's thickness for a new solder mask
/// (`BOARD_STACKUP_ITEM::GetMaskDefaultThickness`).
const MASK_THICKNESS_NM: i64 = 10_000;

/// One ounce of copper as KiCad reads `oz` typed into a dimension field:
/// 1.37 mil (`EDA_UNIT_UTILS::UI::DoubleValueFromString`,
/// `common/eda_units.cpp`).
const OUNCE_NM: f64 = 1.37 * 25_400.0;

/// What a dielectric is made of when the call does not say: KiCad's values
/// for a new one (`BOARD_STACKUP_ITEM::BOARD_STACKUP_ITEM`).
const DIELECTRIC_MATERIAL: &str = "FR4";
const DIELECTRIC_EPSILON_R: f64 = 4.5;
const DIELECTRIC_LOSS_TANGENT: f64 = 0.02;

/// The finishes Board Setup offers (`copperFinishType`,
/// `stackup_predefined_prms.cpp`), less "Not specified" and "User defined".
const FINISHES: [&str; 12] = [
    "ENIG",
    "ENEPIG",
    "HAL SnPb",
    "HAL lead-free",
    "Hard gold",
    "Immersion tin",
    "Immersion nickel",
    "Immersion silver",
    "Immersion gold",
    "HT_OSP",
    "OSP",
    "None",
];

/// KiCad's named solder mask and silkscreen colors (`gbrjobColors`,
/// `stackup_predefined_prms.cpp`), less "Not specified" and "User defined".
const COLORS: [&str; 7] = ["Green", "Red", "Blue", "Purple", "Black", "White", "Yellow"];

/// KiCad's default finish (`BOARD_STACKUP::BOARD_STACKUP`). It writes it like
/// any other, so a stack that names none is given it.
const DEFAULT_FINISH: &str = "None";

/// The first board format whose layer table holds canonical names, a user's
/// name beside them (`20200922` in `pcb_io_kicad_sexpr.h`'s format log;
/// `PCB_IO_KICAD_SEXPR_PARSER::parseLayer`). Before it a copper layer could be
/// named "GND", which KiCad still counts as copper by its place; and the
/// releases that wrote those formats cannot read a stackup they never had.
const CANONICAL_LAYERS_VERSION: u64 = 20200922;

/// εr and loss tangent KiCad gives back are compared to within this, relative
/// above 1: wider than its rounding of them, to ten significant digits or, at
/// 1e-4 and under, sixteen decimals (`FormatDouble2Str`,
/// `common/string_utils.cpp`).
const PROPERTY_TOLERANCE: f64 = 1e-9;

/// KiCad's board reader clamps a length beyond this (`INT_LIMIT` in
/// `pcb_io_kicad_sexpr_parser.cpp`). Held to it, the stack's sum cannot
/// overflow; a sum beyond it KiCad clamps, which the probe reports.
const THICKNESS_LIMIT_NM: i64 = i32::MAX as i64 - 10;

pub(crate) fn tool() -> ToolDef {
    let color = json!({
        "type": "string",
        "pattern": "^(Green|Red|Blue|Purple|Black|White|Yellow|#[0-9A-Fa-f]{6}([0-9A-Fa-f]{2})?)$",
        "description": "One of KiCad's names (Green, Red, Blue, Purple, Black, White, Yellow), or #RRGGBB or #RRGGBBAA"
    });
    let thickness = json!({ "type": "number", "exclusiveMinimum": 0 });
    tool!(
        "set_board_stackup",
        "Replace the physical stackup of a board KiCad does not hold open: each copper \
         layer's thickness, each dielectric's type, thickness, material, εr and loss tangent, \
         the solder mask's color and thickness, the silkscreen color, the copper finish and \
         impedance control; a value left out is kept. The board thickness written is the \
         stack's sum. kicad-cli first loads and saves a copy, and nothing is written unless \
         KiCad reads back every value; without kicad-cli, nothing is. The copper-layer count \
         is not changed.",
        json!({
            "type": "object",
            "properties": {
                "board": { "type": "string", "description": "Path to .kicad_pcb file" },
                "copper_layers": {
                    "type": "array",
                    "minItems": 2,
                    "description": "Top to bottom, one per copper layer of the board (F.Cu, In1.Cu…, B.Cu), each with thickness_mm or weight_oz",
                    "items": {
                        "type": "object",
                        "properties": {
                            "thickness_mm": thickness,
                            "weight_oz": { "type": "number", "exclusiveMinimum": 0, "description": "Read as KiCad reads oz typed into a field: 1 oz is 1.37 mil" }
                        }
                    }
                },
                "dielectrics": {
                    "type": "array",
                    "minItems": 1,
                    "description": "Top to bottom, one between each two copper layers. A material, εr or loss tangent left out is the board's, else KiCad's FR4, 4.5 and 0.02",
                    "items": {
                        "type": "object",
                        "properties": {
                            "type": { "type": "string", "enum": ["core", "prepreg"] },
                            "thickness_mm": thickness,
                            "material": { "type": "string", "minLength": 1 },
                            "epsilon_r": { "type": "number", "exclusiveMinimum": 0 },
                            "loss_tangent": { "type": "number", "minimum": 0 }
                        },
                        "required": ["type", "thickness_mm"]
                    }
                },
                "solder_mask": {
                    "type": "object",
                    "description": "Both sides. A field left out keeps the board's value, or KiCad's default: 0.01 mm, no color",
                    "properties": { "color": color, "thickness_mm": thickness }
                },
                "silkscreen": {
                    "type": "object",
                    "description": "Both sides. A color left out keeps the board's",
                    "properties": { "color": color }
                },
                "finish": { "type": "string", "enum": FINISHES, "description": "KiCad's finish name; left out, the board's is kept" },
                "impedance_controlled": { "type": "boolean", "description": "Left out, the board's is kept" },
                "board_thickness_mm": { "type": "number", "exclusiveMinimum": 0, "description": "A check, not an input: the call is refused unless the stack sums to it within 0.001 mm" }
            },
            "required": ["board", "copper_layers", "dielectrics"]
        }),
        |args, ctx| async move { handle_set_board_stackup(args, ctx).await }
    )
    .with_board_access(BoardAccess::ClosedBoardOnly)
}

async fn handle_set_board_stackup(
    args: &Value,
    ctx: &ToolContext,
) -> anyhow::Result<CallToolResult> {
    let board = get_path(args, "board")?;
    let request = match Request::read(args) {
        Ok(request) => request,
        Err(refusal) => return Ok(refusal),
    };
    if let Some(refusal) = refuse_if_board_open_in_kicad(ctx, &board, "stackup").await? {
        return Ok(refusal);
    }

    let read = read_consistent(&board)?;
    let tree = parse_sexp(&read)?;
    let version = tree.find_str("version").and_then(|v| v.parse::<u64>().ok());
    if version.is_none_or(|version| version < CANONICAL_LAYERS_VERSION) {
        return Ok(plan_blocked(format!(
            "{OPERATION} wrote nothing to '{}': its file format, version {}, is older than \
             {CANONICAL_LAYERS_VERSION}, whose layer table names every layer as KiCad 10 \
             counts it, and the KiCad that wrote it reads no stackup. Open and save the board \
             in KiCad 10, or run kicad-cli pcb upgrade on it, then retry",
            board.display(),
            version.map_or("none".to_string(), |v| v.to_string())
        )));
    }
    let names: Vec<String> = layers::layers(&tree)
        .into_iter()
        .map(|layer| layer.name)
        .collect();
    let current = tree
        .find("setup")
        .and_then(|setup| setup.find("stackup"))
        .map(read_stack);
    let stack = match build(&request, &names, current.as_ref()) {
        Ok(stack) => stack,
        Err(refusal) => return Ok(refusal),
    };
    let thickness = stack.thickness_nm();
    if let Some(asserted) = request.board_thickness_nm {
        if (asserted - thickness).abs() > (THICKNESS_TOLERANCE_MM * 1e6).round() as i64 {
            return Ok(invalid_arg(
                "board_thickness_mm",
                &format!(
                    "the stack sums to {} mm, not {} mm. The written thickness is always the \
                     stack's sum; change a layer, or the assertion",
                    mm_text(thickness),
                    mm_text(asserted)
                ),
            ));
        }
    }
    let next = match edited(&read, &stack, thickness) {
        Ok(next) => next,
        Err(reason) => {
            return Ok(plan_blocked(format!(
                "{OPERATION} wrote nothing to '{}': {reason}",
                board.display()
            )))
        }
    };

    if let Some(reason) =
        kicad_round_trip(&ctx.config.kicad_cli, &board, &next, &stack, thickness).await?
    {
        return Ok(plan_blocked(format!(
            "{OPERATION} wrote nothing to '{}': {reason}",
            board.display()
        )));
    }

    let changed = next != read;
    if changed {
        // KiCad may open the board while kicad-cli loads the copy, and its next
        // save would then discard this write.
        if let Some(refusal) = refuse_if_board_open_in_kicad(ctx, &board, "stackup").await? {
            return Ok(refusal);
        }
        if let Some(refusal) = replace_if_unchanged(&board, "board", OPERATION, &read, next)? {
            return Ok(refusal);
        }
    }

    let written = match read_consistent(&board) {
        Ok(text) => text,
        Err(error) => {
            return Ok(file_outcome_uncertain(
                &board,
                OPERATION,
                format!("the board could not be read back: {error}"),
            ))
        }
    };
    match saved_stack(&written) {
        Ok((saved, saved_thickness)) if saved == stack && saved_thickness == Some(thickness) => {}
        Ok(_) => {
            return Ok(mutation_outcome_uncertain(
                &board,
                OPERATION,
                "the stackup read back is not the stackup written",
            ))
        }
        Err(error) => {
            return Ok(mutation_outcome_uncertain(
                &board,
                OPERATION,
                format!("the board read back does not parse: {error:#}"),
            ))
        }
    }

    let layers: Vec<Value> = stack.entries.iter().map(Entry::json).collect();
    let mut body = json!({
        "success": true,
        "board": board.display().to_string(),
        "changed": changed,
        "copper_layer_count": stack.entries.iter().filter(|e| e.kind() == Kind::Copper).count(),
        "board_thickness_mm": mm(thickness),
        "finish": stack.finish,
        "impedance_controlled": stack.dielectric_constraints,
        // Named as `get_board_stackup` names them; the file says yes or bevelled.
        "edge_connector": match stack.edge_connector.as_deref() {
            None => "none",
            Some("bevelled") => "beveled",
            Some(_) => "plain",
        },
        "has_edge_plating": stack.edge_plating,
        "count": layers.len(),
        "layers": layers,
        "source": "file",
    });
    if changed {
        body["warning"] = json!(FILE_ONLY_EDIT_WARNING);
    }
    Ok(CallToolResult::json(&body))
}

fn plan_blocked(message: String) -> CallToolResult {
    CallToolResult::error_kind(
        ToolErrorKind::PlanBlocked {
            operation: OPERATION.to_string(),
            reasons: vec![message.clone()],
        },
        message,
    )
}

// ─── The request ─────────────────────────────────────────────────────────────

/// A dielectric as the call gives it; a value left out is the board's.
struct DielectricArg {
    kind: String,
    thickness_nm: i64,
    material: Option<String>,
    epsilon_r: Option<f64>,
    loss_tangent: Option<f64>,
}

struct Request {
    copper_nm: Vec<i64>,
    dielectrics: Vec<DielectricArg>,
    mask_color: Option<String>,
    mask_thickness_nm: Option<i64>,
    silk_color: Option<String>,
    finish: Option<String>,
    impedance_controlled: Option<bool>,
    board_thickness_nm: Option<i64>,
}

impl Request {
    fn read(args: &Value) -> Result<Self, CallToolResult> {
        let mut copper_nm = Vec::new();
        for (i, layer) in require_array(args, "copper_layers")?.iter().enumerate() {
            let field = format!("copper_layers[{i}]");
            copper_nm.push(
                match (
                    number(&layer["thickness_mm"], &field)?,
                    number(&layer["weight_oz"], &field)?,
                ) {
                    (Some(thickness), None) => nm(&field, thickness * 1e6)?,
                    (None, Some(weight)) => nm(&field, weight * OUNCE_NM)?,
                    (Some(_), Some(_)) => {
                        return Err(invalid_arg(
                            &field,
                            "give thickness_mm or weight_oz, not both",
                        ))
                    }
                    (None, None) => {
                        return Err(invalid_arg(&field, "give thickness_mm or weight_oz"))
                    }
                },
            );
        }
        let mut dielectrics = Vec::new();
        for (i, layer) in require_array(args, "dielectrics")?.iter().enumerate() {
            let field = format!("dielectrics[{i}]");
            let kind = match layer["type"].as_str() {
                Some(kind @ ("core" | "prepreg")) => kind.to_string(),
                _ => return Err(invalid_arg(&field, "type must be 'core' or 'prepreg'")),
            };
            let thickness = number(&layer["thickness_mm"], &field)?
                .ok_or_else(|| invalid_arg(&field, "thickness_mm is required"))?;
            let material = string(&layer["material"], &field)?;
            if material.is_some_and(unspecified) {
                return Err(invalid_arg(
                    &field,
                    "material, when given, names one; KiCad drops an empty or \"Not specified\" one",
                ));
            }
            let epsilon_r = number(&layer["epsilon_r"], &field)?;
            if epsilon_r.is_some_and(|value| value <= 0.0) {
                return Err(invalid_arg(&field, "epsilon_r must be more than 0"));
            }
            let loss_tangent = number(&layer["loss_tangent"], &field)?;
            if loss_tangent.is_some_and(|value| value < 0.0) {
                return Err(invalid_arg(&field, "loss_tangent must not be negative"));
            }
            dielectrics.push(DielectricArg {
                kind,
                thickness_nm: nm(&field, thickness * 1e6)?,
                material: material.map(str::to_string),
                epsilon_r,
                loss_tangent,
            });
        }
        let mask = object(&args["solder_mask"], "solder_mask")?;
        let silkscreen = object(&args["silkscreen"], "silkscreen")?;
        let finish = string(&args["finish"], "finish")?;
        if let Some(finish) = finish.filter(|finish| !FINISHES.contains(finish)) {
            return Err(invalid_arg(
                "finish",
                &format!(
                    "'{finish}' is not one of KiCad's finishes: {}",
                    FINISHES.join(", ")
                ),
            ));
        }
        Ok(Self {
            copper_nm,
            dielectrics,
            mask_color: string(&mask["color"], "solder_mask.color")?
                .map(|value| color("solder_mask.color", value))
                .transpose()?,
            mask_thickness_nm: number(&mask["thickness_mm"], "solder_mask.thickness_mm")?
                .map(|thickness| nm("solder_mask.thickness_mm", thickness * 1e6))
                .transpose()?,
            silk_color: string(&silkscreen["color"], "silkscreen.color")?
                .map(|value| color("silkscreen.color", value))
                .transpose()?,
            finish: finish.map(str::to_string),
            impedance_controlled: match &args["impedance_controlled"] {
                Value::Null => None,
                Value::Bool(value) => Some(*value),
                _ => return Err(invalid_arg("impedance_controlled", "must be a boolean")),
            },
            board_thickness_nm: number(&args["board_thickness_mm"], "board_thickness_mm")?
                .map(|thickness| nm("board_thickness_mm", thickness * 1e6))
                .transpose()?,
        })
    }
}

/// An optional number, absent or null when not given; a value of another
/// type is refused, as `get_board_stackup` refuses one.
fn number(value: &Value, field: &str) -> Result<Option<f64>, CallToolResult> {
    match value {
        Value::Null => Ok(None),
        value => value
            .as_f64()
            .filter(|n| n.is_finite())
            .map(Some)
            .ok_or_else(|| invalid_arg(field, "a value given must be a finite number")),
    }
}

/// An optional string, as [`number`] takes a number.
fn string<'a>(value: &'a Value, field: &str) -> Result<Option<&'a str>, CallToolResult> {
    match value {
        Value::Null => Ok(None),
        Value::String(text) => Ok(Some(text)),
        _ => Err(invalid_arg(field, "a value given must be a string")),
    }
}

/// An optional object, null when not given.
fn object<'a>(value: &'a Value, field: &str) -> Result<&'a Value, CallToolResult> {
    match value {
        Value::Null | Value::Object(_) => Ok(value),
        _ => Err(invalid_arg(field, "must be an object")),
    }
}

/// A thickness in nanometres, KiCad's board unit, refused if it rounds to none
/// or exceeds what the unit holds.
fn nm(field: &str, nanometres: f64) -> Result<i64, CallToolResult> {
    let rounded = nanometres.round();
    if !rounded.is_finite() || rounded < 1.0 {
        return Err(invalid_arg(
            field,
            "the thickness rounds to zero at KiCad's 1 nm",
        ));
    }
    if rounded > THICKNESS_LIMIT_NM as f64 {
        return Err(invalid_arg(
            field,
            "the thickness is more than KiCad's board unit holds",
        ));
    }
    Ok(rounded as i64)
}

/// A color as KiCad stores one: one of its names as given, a custom color as
/// `#RRGGBBAA` in capitals (`COLOR4D::ToHexString`), opaque when no alpha is
/// given.
fn color(field: &str, value: &str) -> Result<String, CallToolResult> {
    if COLORS.contains(&value) {
        return Ok(value.to_string());
    }
    match value.strip_prefix('#') {
        Some(hex) if matches!(hex.len(), 6 | 8) && hex.bytes().all(|b| b.is_ascii_hexdigit()) => {
            let alpha = if hex.len() == 6 { "FF" } else { "" };
            Ok(format!("#{}{alpha}", hex.to_ascii_uppercase()))
        }
        _ => Err(invalid_arg(
            field,
            &format!(
                "'{value}' is not one of KiCad's colors ({}), nor #RRGGBB or #RRGGBBAA",
                COLORS.join(", ")
            ),
        )),
    }
}

// ─── The stack as KiCad stores it ────────────────────────────────────────────

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Kind {
    Silkscreen,
    Paste,
    Mask,
    Copper,
    Dielectric,
}

/// One `(layer …)` entry: a sub-layer list, since a dielectric can hold
/// several (`addsublayer`); every other entry holds one.
#[derive(Clone, Debug, PartialEq)]
struct Entry {
    name: String,
    type_name: String,
    sublayers: Vec<Sublayer>,
}

#[derive(Clone, Debug, Default)]
struct Sublayer {
    color: Option<String>,
    thickness_nm: Option<i64>,
    locked: bool,
    material: Option<String>,
    epsilon_r: Option<f64>,
    loss_tangent: Option<f64>,
}

/// Two εr or loss tangents as KiCad would write them alike.
fn close(a: Option<f64>, b: Option<f64>) -> bool {
    match (a, b) {
        (Some(a), Some(b)) => (a - b).abs() <= PROPERTY_TOLERANCE * a.abs().max(1.0),
        (a, b) => a.is_none() && b.is_none(),
    }
}

impl PartialEq for Sublayer {
    fn eq(&self, other: &Self) -> bool {
        self.color == other.color
            && self.thickness_nm == other.thickness_nm
            && self.locked == other.locked
            && self.material == other.material
            && close(self.epsilon_r, other.epsilon_r)
            && close(self.loss_tangent, other.loss_tangent)
    }
}

#[derive(Clone, Debug, Default, PartialEq)]
struct Stack {
    entries: Vec<Entry>,
    finish: Option<String>,
    dielectric_constraints: bool,
    edge_connector: Option<String>,
    edge_plating: bool,
}

impl Entry {
    fn kind(&self) -> Kind {
        match self.name.as_str() {
            "F.SilkS" | "B.SilkS" => Kind::Silkscreen,
            "F.Paste" | "B.Paste" => Kind::Paste,
            "F.Mask" | "B.Mask" => Kind::Mask,
            name if layers::is_copper_name(name) => Kind::Copper,
            _ => Kind::Dielectric,
        }
    }

    /// The entry as `get_board_stackup` gives a layer, with the `color` the
    /// file names, which KiCad's API does not serve.
    fn json(&self) -> Value {
        let sub = &self.sublayers[0];
        let text = |value: &Option<String>| value.as_ref().map_or(Value::Null, |v| json!(v));
        let thickness = |sub: &Sublayer| sub.thickness_nm.map(mm);
        let mut out = json!({ "name": self.name });
        match self.kind() {
            Kind::Dielectric => {
                out["type"] = json!("dielectric");
                out["dielectric_type"] = json!(self.type_name);
                out["thickness_mm"] = json!(mm(self
                    .sublayers
                    .iter()
                    .filter_map(|s| s.thickness_nm)
                    .sum()));
                out["sublayers"] = json!(self
                    .sublayers
                    .iter()
                    .map(|sub| json!({
                        "thickness_mm": thickness(sub),
                        "material": text(&sub.material),
                        "epsilon_r": sub.epsilon_r,
                        "loss_tangent": sub.loss_tangent,
                        "thickness_locked": sub.locked,
                    }))
                    .collect::<Vec<_>>());
                out["color"] = text(&sub.color);
            }
            Kind::Copper => {
                out["type"] = json!("copper");
                out["thickness_mm"] = json!(thickness(sub));
            }
            Kind::Mask => {
                out["type"] = json!("soldermask");
                out["thickness_mm"] = json!(thickness(sub));
                out["material"] = text(&sub.material);
                out["epsilon_r"] = json!(sub.epsilon_r);
                out["loss_tangent"] = json!(sub.loss_tangent);
                out["color"] = text(&sub.color);
            }
            Kind::Silkscreen => {
                out["type"] = json!("silkscreen");
                out["material"] = text(&sub.material);
                out["color"] = text(&sub.color);
            }
            Kind::Paste => out["type"] = json!("solderpaste"),
        }
        out
    }
}

impl Stack {
    /// KiCad's sum: every copper, dielectric and solder-mask thickness, each
    /// sub-layer of a dielectric (`BOARD_STACKUP::BuildBoardThicknessFromStackup`).
    fn thickness_nm(&self) -> i64 {
        self.entries
            .iter()
            .filter(|entry| matches!(entry.kind(), Kind::Copper | Kind::Dielectric | Kind::Mask))
            .flat_map(|entry| &entry.sublayers)
            .filter_map(|sub| sub.thickness_nm)
            .sum()
    }

    fn names(&self) -> Vec<&str> {
        self.entries
            .iter()
            .map(|entry| entry.name.as_str())
            .collect()
    }
}

fn text(node: &SexpNode, n: usize) -> Option<String> {
    node.get(n)?.as_str().map(str::to_string)
}

/// A value KiCad treats as given: not empty, not "Not specified"
/// (`IsPrmSpecified`).
fn specified(value: Option<String>) -> Option<String> {
    value.filter(|value| !unspecified(value))
}

fn parse_nm(node: &SexpNode) -> Option<i64> {
    node.get_f64(1).map(|mm| (mm * 1e6).round() as i64)
}

/// A `(stackup …)` block as KiCad's parser reads it: each `(layer …)` entry
/// split into sub-layers at `addsublayer`, and the board-wide settings.
fn read_stack(stackup: &SexpNode) -> Stack {
    let mut stack = Stack::default();
    for child in stackup.children().unwrap_or(&[]).iter().skip(1) {
        let yes = text(child, 1).as_deref() == Some("yes");
        match child.head() {
            Some("layer") => stack.entries.push(read_entry(child)),
            Some("copper_finish") => stack.finish = specified(text(child, 1)),
            Some("dielectric_constraints") => stack.dielectric_constraints = yes,
            Some("edge_connector") => {
                stack.edge_connector = text(child, 1).filter(|value| value != "no")
            }
            Some("edge_plating") => stack.edge_plating = yes,
            _ => {}
        }
    }
    stack
}

fn read_entry(node: &SexpNode) -> Entry {
    let mut entry = Entry {
        name: text(node, 1).unwrap_or_default(),
        type_name: String::new(),
        sublayers: vec![Sublayer::default()],
    };
    for item in node.children().unwrap_or(&[]).iter().skip(2) {
        if item.as_str() == Some("addsublayer") {
            entry.sublayers.push(Sublayer::default());
            continue;
        }
        let sub = entry.sublayers.last_mut().expect("one sub-layer at least");
        match item.head() {
            Some("type") => entry.type_name = text(item, 1).unwrap_or_default(),
            Some("color") => sub.color = specified(text(item, 1)),
            Some("thickness") => {
                sub.thickness_nm = parse_nm(item);
                sub.locked = text(item, 2).as_deref() == Some("locked");
            }
            Some("material") => sub.material = specified(text(item, 1)),
            Some("epsilon_r") => sub.epsilon_r = item.get_f64(1),
            Some("loss_tangent") => sub.loss_tangent = item.get_f64(1),
            _ => {}
        }
    }
    entry
}

/// The stack and the board thickness a saved board holds.
fn saved_stack(board: &str) -> anyhow::Result<(Stack, Option<i64>)> {
    let tree = parse_sexp(board)?;
    let stack = tree
        .find("setup")
        .and_then(|setup| setup.find("stackup"))
        .map(read_stack)
        .context("the board has no (stackup …) block")?;
    let thickness = tree
        .find("general")
        .and_then(|general| general.find("thickness"))
        .and_then(parse_nm);
    Ok((stack, thickness))
}

// ─── Building the new stack ──────────────────────────────────────────────────

/// The board's copper in stack order, F.Cu, In1.Cu … B.Cu, the one shape
/// KiCad gives a board; the layer table lists it by id.
fn copper_stack(names: &[String]) -> Result<Vec<String>, String> {
    let stack = CopperStack::from_layer_names(names.iter().map(String::as_str))
        .map_err(|error| error.to_string())?;
    let inner = (1..stack.count() - 1).map(|n| format!("In{n}.Cu"));
    Ok(std::iter::once("F.Cu".to_string())
        .chain(inner)
        .chain(std::iter::once("B.Cu".to_string()))
        .collect())
}

/// The stack KiCad would hold after this call, top to bottom in the order
/// `BOARD_STACKUP::BuildDefaultStackupList` gives: the board's enabled
/// silkscreen, paste and mask over its copper and dielectrics, and under them
/// on the back. A value the call does not set is the board's, and only the
/// values KiCad writes for each kind are kept (`FormatBoardStackup`).
fn build(
    request: &Request,
    names: &[String],
    current: Option<&Stack>,
) -> Result<Stack, CallToolResult> {
    let copper = copper_stack(names).map_err(|reason| {
        plan_blocked(format!(
            "{OPERATION} wrote nothing: the board's {reason}, the only copper stack KiCad builds"
        ))
    })?;
    if request.copper_nm.len() != copper.len() {
        return Err(invalid_arg(
            "copper_layers",
            &format!(
                "the board has {} copper layers ({}), and {} were given. This tool does not \
                 change the copper-layer count; KiCad's Board Setup does, to an even count, \
                 then retry",
                copper.len(),
                copper.join(", "),
                request.copper_nm.len()
            ),
        ));
    }
    if request.dielectrics.len() + 1 != copper.len() {
        return Err(invalid_arg(
            "dielectrics",
            &format!(
                "{} copper layers need {} dielectrics between them, and {} were given",
                copper.len(),
                copper.len().saturating_sub(1),
                request.dielectrics.len()
            ),
        ));
    }
    let existing =
        |name: &str| current.and_then(|stack| stack.entries.iter().find(|e| e.name == name));
    if let Some(entry) = current
        .into_iter()
        .flat_map(|stack| &stack.entries)
        .find(|entry| entry.sublayers.len() > 1)
    {
        return Err(plan_blocked(format!(
            "{OPERATION} wrote nothing: the board's {} has {} sub-layers, which this tool does \
             not set. Change it in KiCad's Board Setup",
            entry.name,
            entry.sublayers.len()
        )));
    }
    let enabled = |name: &str| names.iter().any(|n| n == name);
    for (field, given, layers) in [
        (
            "solder_mask",
            request.mask_color.is_some() || request.mask_thickness_nm.is_some(),
            ["F.Mask", "B.Mask"],
        ),
        (
            "silkscreen",
            request.silk_color.is_some(),
            ["F.SilkS", "B.SilkS"],
        ),
    ] {
        if given && !layers.iter().any(|layer| enabled(layer)) {
            return Err(invalid_arg(
                field,
                &format!(
                    "the board enables neither {}, so there is none to set",
                    layers.join(" nor ")
                ),
            ));
        }
    }
    // KiCad numbers the dielectrics in the order they stand, counting every
    // entry that is not an enabled layer (`parseBoardStackup`), so a kept
    // value is found by place, not by the name the file gives it.
    let held_dielectrics: Vec<&Entry> = current
        .into_iter()
        .flat_map(|stack| &stack.entries)
        .filter(|entry| !enabled(&entry.name))
        .collect();

    let mut entries = Vec::new();
    let surface = |name: &str, type_name: &str, kind: Kind, entries: &mut Vec<Entry>| {
        if !enabled(name) {
            return;
        }
        let held = existing(name);
        let was = held
            .map(|entry| entry.sublayers[0].clone())
            .unwrap_or_default();
        let sub = match kind {
            Kind::Silkscreen => Sublayer {
                color: request.silk_color.clone().or(was.color),
                material: was.material,
                ..Sublayer::default()
            },
            Kind::Mask => Sublayer {
                color: request.mask_color.clone().or(was.color),
                thickness_nm: request
                    .mask_thickness_nm
                    .or(was.thickness_nm)
                    .or(Some(MASK_THICKNESS_NM)),
                epsilon_r: was.material.as_ref().and(was.epsilon_r),
                loss_tangent: was.material.as_ref().and(was.loss_tangent),
                material: was.material,
                ..Sublayer::default()
            },
            _ => Sublayer::default(),
        };
        entries.push(Entry {
            name: name.to_string(),
            type_name: held
                .map(|entry| entry.type_name.clone())
                .filter(|type_name| !type_name.is_empty())
                .unwrap_or_else(|| type_name.to_string()),
            sublayers: vec![sub],
        });
    };
    surface("F.SilkS", "Top Silk Screen", Kind::Silkscreen, &mut entries);
    surface("F.Paste", "Top Solder Paste", Kind::Paste, &mut entries);
    surface("F.Mask", "Top Solder Mask", Kind::Mask, &mut entries);
    for (i, (name, &thickness_nm)) in copper.iter().zip(&request.copper_nm).enumerate() {
        entries.push(Entry {
            name: name.clone(),
            type_name: existing(name)
                .map(|entry| entry.type_name.clone())
                .filter(|type_name| !type_name.is_empty())
                .unwrap_or_else(|| "copper".to_string()),
            sublayers: vec![Sublayer {
                thickness_nm: Some(thickness_nm),
                ..Sublayer::default()
            }],
        });
        let Some(dielectric) = request.dielectrics.get(i) else {
            continue;
        };
        let name = format!("dielectric {}", i + 1);
        let was = held_dielectrics
            .get(i)
            .map(|entry| entry.sublayers[0].clone())
            .unwrap_or_default();
        entries.push(Entry {
            name,
            type_name: dielectric.kind.clone(),
            sublayers: vec![Sublayer {
                color: was.color,
                thickness_nm: Some(dielectric.thickness_nm),
                locked: was.locked,
                material: dielectric
                    .material
                    .clone()
                    .or(was.material)
                    .or_else(|| Some(DIELECTRIC_MATERIAL.to_string())),
                epsilon_r: dielectric
                    .epsilon_r
                    .or(was.epsilon_r)
                    .or(Some(DIELECTRIC_EPSILON_R)),
                loss_tangent: dielectric
                    .loss_tangent
                    .or(was.loss_tangent)
                    .or(Some(DIELECTRIC_LOSS_TANGENT)),
            }],
        });
    }
    surface("B.Mask", "Bottom Solder Mask", Kind::Mask, &mut entries);
    surface("B.Paste", "Bottom Solder Paste", Kind::Paste, &mut entries);
    surface(
        "B.SilkS",
        "Bottom Silk Screen",
        Kind::Silkscreen,
        &mut entries,
    );

    Ok(Stack {
        entries,
        finish: request
            .finish
            .clone()
            .or_else(|| current.and_then(|stack| stack.finish.clone()))
            .or_else(|| Some(DEFAULT_FINISH.to_string())),
        dielectric_constraints: request
            .impedance_controlled
            .unwrap_or_else(|| current.is_some_and(|stack| stack.dielectric_constraints)),
        edge_connector: current.and_then(|stack| stack.edge_connector.clone()),
        edge_plating: current.is_some_and(|stack| stack.edge_plating),
    })
}

// ─── Writing it ──────────────────────────────────────────────────────────────

/// The `(stackup …)` block in KiCad 10's layout, one item a line, its lines
/// indented from `indent` by `unit` and ended by `newline`. Strings are quoted
/// as KiCad quotes them, since its reader refuses one that runs past its line.
fn format_stack(stack: &Stack, indent: &str, unit: &str, newline: &str) -> String {
    let quoted = quote;
    let entry_indent = format!("{newline}{indent}{unit}");
    let field_indent = format!("{entry_indent}{unit}");
    let mut out = String::from("(stackup");
    for entry in &stack.entries {
        out += &format!("{entry_indent}(layer {}", quoted(&entry.name));
        out += &format!("{field_indent}(type {})", quoted(&entry.type_name));
        for (i, sub) in entry.sublayers.iter().enumerate() {
            if i > 0 {
                out += &format!("{field_indent}addsublayer");
            }
            if let Some(color) = &sub.color {
                out += &format!("{field_indent}(color {})", quoted(color));
            }
            if let Some(thickness) = sub.thickness_nm {
                let locked = if sub.locked { " locked" } else { "" };
                out += &format!("{field_indent}(thickness {}{locked})", mm_text(thickness));
            }
            if let Some(material) = &sub.material {
                out += &format!("{field_indent}(material {})", quoted(material));
                if let Some(epsilon_r) = sub.epsilon_r {
                    out += &format!("{field_indent}(epsilon_r {epsilon_r})");
                }
                if let Some(loss_tangent) = sub.loss_tangent {
                    out += &format!("{field_indent}(loss_tangent {loss_tangent})");
                }
            }
        }
        out += &format!("{entry_indent})");
    }
    if let Some(finish) = &stack.finish {
        out += &format!("{entry_indent}(copper_finish {})", quoted(finish));
    }
    let yes_no = |value: bool| if value { "yes" } else { "no" };
    out += &format!(
        "{entry_indent}(dielectric_constraints {})",
        yes_no(stack.dielectric_constraints)
    );
    if let Some(edge_connector) = &stack.edge_connector {
        out += &format!("{entry_indent}(edge_connector {edge_connector})");
    }
    if stack.edge_plating {
        out += &format!("{entry_indent}(edge_plating yes)");
    }
    out += &format!("{newline}{indent})");
    out
}

/// The whitespace that starts the line holding `offset`.
fn indent_before(content: &str, offset: usize) -> &str {
    let line = content[..offset].rfind('\n').map_or(0, |at| at + 1);
    let text = &content[line..offset];
    &text[..text.len() - text.trim_start_matches([' ', '\t']).len()]
}

/// `board` with `stack` as its `(stackup …)` block, replaced where it stands
/// or made the first item of `(setup …)` as KiCad writes it, and `thickness`
/// as its `(general (thickness …))`. The file's indent and line ends are kept.
fn edited(board: &str, stack: &Stack, thickness: i64) -> Result<String, String> {
    let setup = find_block_starts(board, "setup")
        .into_iter()
        .next()
        .ok_or("the board has no (setup …) block")?;
    let setup_indent = indent_before(board, setup);
    let unit = if setup_indent.is_empty() {
        "\t"
    } else {
        setup_indent
    };
    let newline = if board.contains("\r\n") { "\r\n" } else { "\n" };
    let stack_indent = format!("{setup_indent}{unit}");
    let block = format_stack(stack, &stack_indent, unit, newline);

    let mut edits = Vec::new();
    match direct_children_with_tag(board, "setup", "stackup").first() {
        Some(&(start, end)) => edits.push(SexpEdit::replace(start, end, block)),
        None => edits.push(SexpEdit::insert(
            setup + "(setup".len(),
            format!("{newline}{stack_indent}{block}"),
        )),
    }
    let &(start, end) = direct_children_with_tag(board, "general", "thickness")
        .first()
        .ok_or("the board has no (general (thickness …)), which KiCad writes on every save")?;
    edits.push(SexpEdit::replace(
        start,
        end,
        format!("(thickness {})", mm_text(thickness)),
    ));
    Ok(apply_edits(board.to_string(), edits))
}

// ─── KiCad's own reading ─────────────────────────────────────────────────────

/// `None` when kicad-cli loads `next` and saves it with `stack` and
/// `thickness` unchanged, else why the write is refused. The copy is saved in
/// place by `pcb upgrade --force`, so it is written by KiCad's own reader and
/// writer (`PCBNEW_JOBS_HANDLER::JobUpgrade`), not checked by a second parser.
async fn kicad_round_trip(
    cli: &str,
    board: &Path,
    next: &str,
    stack: &Stack,
    thickness: i64,
) -> anyhow::Result<Option<String>> {
    let dir = tempfile::tempdir()?;
    let copy = dir
        .path()
        .join(board.file_name().context("the board path names no file")?);
    tokio::fs::write(&copy, next).await?;
    if let Err(error) = crate::tools::cli::resave_board(cli, &copy).await {
        return Ok(Some(format!(
            "kicad-cli could not load and save a copy with this stackup, so KiCad's reading of \
             it is unknown: {error:#}"
        )));
    }
    let saved = tokio::fs::read_to_string(&copy).await?;
    let (kicad, kicad_thickness) = match saved_stack(&saved) {
        Ok(read) => read,
        Err(error) => {
            return Ok(Some(format!(
                "kicad-cli saved a copy whose stackup cannot be read: {error:#}"
            )))
        }
    };
    let differences = differences(stack, thickness, &kicad, kicad_thickness);
    Ok((!differences.is_empty()).then(|| {
        format!(
            "KiCad reads this stackup otherwise than written: {}",
            differences.join("; ")
        )
    }))
}

/// Every way `read` differs from `written`, named for the caller.
fn differences(
    written: &Stack,
    thickness: i64,
    read: &Stack,
    read_thickness: Option<i64>,
) -> Vec<String> {
    let mut out = Vec::new();
    if written.names() != read.names() {
        out.push(format!(
            "it holds the layers {:?}, not {:?}",
            read.names(),
            written.names()
        ));
    } else {
        for (ours, theirs) in written.entries.iter().zip(&read.entries) {
            entry_differences(ours, theirs, &mut out);
        }
    }
    let shown = |value: &Option<String>| value.as_deref().unwrap_or("none").to_string();
    let settings = [
        ("copper_finish", shown(&written.finish), shown(&read.finish)),
        (
            "dielectric_constraints",
            written.dielectric_constraints.to_string(),
            read.dielectric_constraints.to_string(),
        ),
        (
            "edge_connector",
            shown(&written.edge_connector),
            shown(&read.edge_connector),
        ),
        (
            "edge_plating",
            written.edge_plating.to_string(),
            read.edge_plating.to_string(),
        ),
    ];
    for (name, ours, theirs) in settings {
        if ours != theirs {
            out.push(format!("{name} is read as {theirs}, written as {ours}"));
        }
    }
    if read_thickness != Some(thickness) {
        out.push(format!(
            "the board thickness is read as {}, written as {} mm",
            read_thickness.map_or("none".to_string(), |nm| format!("{} mm", mm_text(nm))),
            mm_text(thickness)
        ));
    }
    out
}

/// Each value of one entry KiCad reads otherwise, as "F.Mask color is read as
/// …, written as …".
fn entry_differences(ours: &Entry, theirs: &Entry, out: &mut Vec<String>) {
    let name = &ours.name;
    if ours.type_name != theirs.type_name {
        out.push(format!(
            "{name} type is read as {:?}, written as {:?}",
            theirs.type_name, ours.type_name
        ));
    }
    if ours.sublayers.len() != theirs.sublayers.len() {
        out.push(format!(
            "{name} is read with {} sub-layers, written with {}",
            theirs.sublayers.len(),
            ours.sublayers.len()
        ));
        return;
    }
    let shown = |value: &Option<String>| {
        value
            .as_deref()
            .map_or("none".to_string(), |v| format!("{v:?}"))
    };
    let thickness =
        |nm: Option<i64>| nm.map_or("none".to_string(), |nm| format!("{} mm", mm_text(nm)));
    let number = |value: Option<f64>| value.map_or("none".to_string(), |v| v.to_string());
    for (a, b) in ours.sublayers.iter().zip(&theirs.sublayers) {
        if a == b {
            continue;
        }
        let fields = [
            ("color", shown(&a.color), shown(&b.color)),
            (
                "thickness",
                thickness(a.thickness_nm),
                thickness(b.thickness_nm),
            ),
            ("thickness lock", a.locked.to_string(), b.locked.to_string()),
            ("material", shown(&a.material), shown(&b.material)),
        ];
        for (field, ours, theirs) in fields {
            if ours != theirs {
                out.push(format!(
                    "{name} {field} is read as {theirs}, written as {ours}"
                ));
            }
        }
        for (field, ours, theirs) in [
            ("epsilon_r", a.epsilon_r, b.epsilon_r),
            ("loss_tangent", a.loss_tangent, b.loss_tangent),
        ] {
            if !close(ours, theirs) {
                out.push(format!(
                    "{name} {field} is read as {}, written as {}",
                    number(theirs),
                    number(ours)
                ));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mcp::handler::McpHandler;
    use crate::tools::write_hook;
    use crate::tools::ServerConfig;
    use copperpour_sexp::writer::find_block_with_leading_whitespace;
    use std::path::PathBuf;

    /// A KiCad 10 board with no stackup block; see the fixture's README.
    const TWO_LAYER: &str = "board_outline_kicad10.kicad_pcb";
    const FOUR_LAYER: &str = "board_source_divergence_kicad10.kicad_pcb";
    /// Six layers, every line ended CRLF.
    const SIX_LAYER_CRLF: &str = "six_layer_power_planes_kicad10.kicad_pcb";
    /// Two layers with a stackup KiCad 10 saved; see its README.
    const WITH_STACKUP: &str = "board_stackup_kicad10.kicad_pcb";

    /// A change to a request, for a table of cases.
    type Change = Box<dyn Fn(&mut Value)>;
    /// A request for a board, for a table of cases.
    type RequestFor = Box<dyn Fn(&Path) -> Value>;

    fn fixture_text(name: &str) -> String {
        std::fs::read_to_string(
            Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("tests/fixtures")
                .join(name),
        )
        .unwrap()
    }

    fn board_from(text: &str) -> (tempfile::TempDir, PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let board = dir.path().join("board.kicad_pcb");
        std::fs::write(&board, text).unwrap();
        (dir, board)
    }

    fn board(name: &str) -> (tempfile::TempDir, PathBuf) {
        board_from(&fixture_text(name))
    }

    /// A kicad-cli that copies `replacement`, if given, over the board it is
    /// asked to save, `$4` in `pcb upgrade --force <board>`, and exits with
    /// `exit_code`. With none it leaves the board as KiCad would leave a stack
    /// it reads as written.
    fn fake_kicad_cli(dir: &Path, replacement: Option<&Path>, exit_code: i32) -> PathBuf {
        let unix = replacement
            .map(|path| format!("cp \"{}\" \"$4\" || exit 1\n", path.display()))
            .unwrap_or_default();
        let windows = replacement
            .map(|path| {
                format!(
                    "copy /y \"{}\" \"%~4\" >nul || exit /b 1\r\n",
                    path.display()
                )
            })
            .unwrap_or_default();
        crate::tools::cli::test_support::write_script(
            dir,
            "kicad-cli",
            &format!("#!/bin/sh\n{unix}exit {exit_code}\n"),
            &format!("@echo off\r\n{windows}exit /b {exit_code}\r\n"),
        )
    }

    async fn call(arguments: Value) -> Value {
        let dir = tempfile::tempdir().unwrap();
        let cli = crate::tools::cli::test_support::noop_cli(dir.path());
        call_with(&cli.display().to_string(), arguments).await
    }

    async fn call_with(kicad_cli: &str, arguments: Value) -> Value {
        let handler = McpHandler::new(ServerConfig {
            eager_toolsets: true,
            kicad_cli: kicad_cli.to_string(),
            ..Default::default()
        })
        .await
        .unwrap();
        let response = handler
            .handle_message(json!({
                "jsonrpc": "2.0", "id": 78, "method": "tools/call",
                "params": {"name": "set_board_stackup", "arguments": arguments}
            }))
            .await
            .unwrap()
            .result
            .unwrap();
        let mut body: Value =
            serde_json::from_str(response["content"][0]["text"].as_str().unwrap()).unwrap();
        body["is_error"] = response["isError"].clone();
        body
    }

    fn two_layer(board: &Path) -> Value {
        json!({
            "board": board,
            "copper_layers": [{"thickness_mm": 0.035}, {"thickness_mm": 0.035}],
            "dielectrics": [{"type": "core", "thickness_mm": 1.51}]
        })
    }

    fn written(board: &Path) -> (Stack, Option<i64>) {
        saved_stack(&std::fs::read_to_string(board).unwrap()).unwrap()
    }

    fn entry<'a>(stack: &'a Stack, name: &str) -> &'a Sublayer {
        &stack
            .entries
            .iter()
            .find(|e| e.name == name)
            .unwrap()
            .sublayers[0]
    }

    /// `board` with its stackup block and the line before it taken out, and
    /// its board thickness put back to `thickness`.
    fn without_stackup(board: &str, thickness: &str) -> String {
        let (start, _) = direct_children_with_tag(board, "setup", "stackup")[0];
        let (from, to) = find_block_with_leading_whitespace(board, start).unwrap();
        let rest = format!("{}{}", &board[..from], &board[to..]);
        let (start, end) = direct_children_with_tag(&rest, "general", "thickness")[0];
        format!("{}(thickness {thickness}){}", &rest[..start], &rest[end..])
    }

    #[tokio::test]
    async fn a_two_layer_stack_goes_where_the_board_had_none() {
        let (_dir, board) = board(TWO_LAYER);
        let mut request = two_layer(&board);
        request["copper_layers"][1] = json!({"weight_oz": 1});
        request["solder_mask"] = json!({"color": "#1a5c2ecc"});
        request["silkscreen"] = json!({"color": "#ffffff"});
        request["finish"] = json!("HAL lead-free");
        request["board_thickness_mm"] = json!(1.599798);
        let body = call(request).await;
        assert_ne!(body["is_error"], true, "{body}");
        assert_eq!(body["changed"], true);
        assert_eq!(body["source"], "file");
        assert_eq!(body["warning"], FILE_ONLY_EDIT_WARNING);
        assert_eq!(body["board_thickness_mm"], 1.599798);
        assert_eq!(body["copper_layer_count"], 2);
        assert_eq!(body["finish"], "HAL lead-free");
        assert_eq!(body["impedance_controlled"], false);

        let (stack, thickness) = written(&board);
        assert_eq!(
            stack.names(),
            [
                "F.SilkS",
                "F.Paste",
                "F.Mask",
                "F.Cu",
                "dielectric 1",
                "B.Cu",
                "B.Mask",
                "B.Paste",
                "B.SilkS"
            ]
        );
        // 1 oz as KiCad reads it typed into a field, 1.37 mil.
        assert_eq!(entry(&stack, "B.Cu").thickness_nm, Some(34_798));
        assert_eq!(
            entry(&stack, "F.Mask").thickness_nm,
            Some(MASK_THICKNESS_NM)
        );
        assert_eq!(entry(&stack, "B.Mask").color.as_deref(), Some("#1A5C2ECC"));
        assert_eq!(entry(&stack, "F.SilkS").color.as_deref(), Some("#FFFFFFFF"));
        let dielectric = entry(&stack, "dielectric 1");
        assert_eq!(dielectric.material.as_deref(), Some("FR4"));
        assert_eq!(
            (dielectric.epsilon_r, dielectric.loss_tangent),
            (Some(4.5), Some(0.02))
        );
        assert_eq!(thickness, Some(1_599_798));

        let text = std::fs::read_to_string(&board).unwrap();
        assert!(text.contains(
            "\t(setup\n\t\t(stackup\n\t\t\t(layer \"F.SilkS\"\n\t\t\t\t(type \"Top Silk Screen\")"
        ));
        assert_eq!(without_stackup(&text, "1.6"), fixture_text(TWO_LAYER));
    }

    #[tokio::test]
    async fn four_copper_layers_take_three_dielectrics_in_stack_order() {
        let (_dir, board) = board(FOUR_LAYER);
        let body = call(json!({
            "board": board,
            "copper_layers": [{"thickness_mm": 0.035}, {"thickness_mm": 0.0152}, {"thickness_mm": 0.0152}, {"thickness_mm": 0.035}],
            "dielectrics": [
                {"type": "prepreg", "thickness_mm": 0.2104, "epsilon_r": 4.4},
                {"type": "core", "thickness_mm": 1.065, "epsilon_r": 4.6},
                {"type": "prepreg", "thickness_mm": 0.2104, "epsilon_r": 4.4}
            ],
            "silkscreen": {"color": "White"},
            "impedance_controlled": true
        }))
        .await;
        assert_ne!(body["is_error"], true, "{body}");
        let (stack, thickness) = written(&board);
        assert_eq!(entry(&stack, "B.SilkS").color.as_deref(), Some("White"));
        assert_eq!(
            stack.names()[3..10],
            [
                "F.Cu",
                "dielectric 1",
                "In1.Cu",
                "dielectric 2",
                "In2.Cu",
                "dielectric 3",
                "B.Cu"
            ]
        );
        assert_eq!(stack.entries[4].type_name, "prepreg");
        assert_eq!(stack.entries[6].type_name, "core");
        assert!(stack.dielectric_constraints);
        assert_eq!(stack.finish.as_deref(), Some(DEFAULT_FINISH));
        assert_eq!(thickness, Some(1_606_200));
        assert_eq!(body["layers"][6]["dielectric_type"], "core");
        assert_eq!(body["layers"][6]["sublayers"][0]["epsilon_r"], 4.6);
        assert_eq!(body["layers"][6]["sublayers"][0]["material"], "FR4");
    }

    /// Text goes in as KiCad quotes it, so KiCad's reader keeps it whole.
    #[tokio::test]
    async fn a_material_is_quoted_as_kicad_quotes_it() {
        let (_dir, board) = board(TWO_LAYER);
        let material = "FR4 \"TG170\" \\ high\r\nTg";
        let mut request = two_layer(&board);
        request["dielectrics"][0]["material"] = json!(material);
        let body = call(request).await;
        assert_ne!(body["is_error"], true, "{body}");
        let text = std::fs::read_to_string(&board).unwrap();
        assert!(
            text.contains(r#"(material "FR4 \"TG170\" \\ high\r\nTg")"#),
            "{text}"
        );
        assert_eq!(
            entry(&written(&board).0, "dielectric 1")
                .material
                .as_deref(),
            Some(material)
        );
    }

    /// A board without paste and silkscreen layers has no entries for them,
    /// as KiCad builds its stack from the enabled layers.
    #[tokio::test]
    async fn only_the_boards_enabled_layers_are_stacked() {
        let mut text = fixture_text(TWO_LAYER);
        for line in [
            "\t\t(13 \"F.Paste\" user)\n",
            "\t\t(15 \"B.Paste\" user)\n",
            "\t\t(7 \"B.SilkS\" user \"B.Silkscreen\")\n",
        ] {
            assert!(text.contains(line));
            text = text.replace(line, "");
        }
        let (_dir, board) = board_from(&text);
        let body = call(two_layer(&board)).await;
        assert_ne!(body["is_error"], true, "{body}");
        assert_eq!(
            written(&board).0.names(),
            [
                "F.SilkS",
                "F.Mask",
                "F.Cu",
                "dielectric 1",
                "B.Cu",
                "B.Mask"
            ]
        );
    }

    /// A board indented with spaces, as KiCad 6 to 8 wrote one, is written in
    /// its own indent.
    #[tokio::test]
    async fn a_space_indented_board_keeps_its_indent() {
        let original = fixture_text(TWO_LAYER).replace('\t', "  ");
        let (_dir, board) = board_from(&original);
        let body = call(two_layer(&board)).await;
        assert_ne!(body["is_error"], true, "{body}");
        let text = std::fs::read_to_string(&board).unwrap();
        assert!(text.contains("\n  (setup\n    (stackup\n      (layer \"F.SilkS\"\n        (type"));
        assert!(!text.contains('\t'));
        assert_eq!(without_stackup(&text, "1.6"), original);
    }

    /// Only the whitespace that starts `(setup`'s line is its indent, though
    /// other text shares the line.
    #[tokio::test]
    async fn a_setup_sharing_its_line_is_indented_by_its_whitespace() {
        let original = fixture_text(TWO_LAYER).replace("\t)\n\t(setup", "\t) (setup");
        let (_dir, board) = board_from(&original);
        let body = call(two_layer(&board)).await;
        assert_ne!(body["is_error"], true, "{body}");
        let text = std::fs::read_to_string(&board).unwrap();
        assert!(
            text.contains("\t) (setup\n\t\t(stackup\n\t\t\t(layer \"F.SilkS\""),
            "{text}"
        );
        assert_eq!(without_stackup(&text, "1.6"), original);
    }

    /// A file older than canonical layer names is left to KiCad 10 to upgrade:
    /// its copper may be named "GND", and its KiCad reads no stackup.
    #[tokio::test]
    async fn a_board_older_than_kicad_6s_format_is_refused() {
        let text = fixture_text(TWO_LAYER).replace("(version 20260206)", "(version 20171130)");
        let (_dir, board) = board_from(&text);
        let body = call(two_layer(&board)).await;
        assert_eq!(body["error"]["kind"], "plan_blocked", "{body}");
        assert!(
            body["message"]
                .as_str()
                .unwrap()
                .contains("version 20171130"),
            "{body}"
        );
        assert_eq!(std::fs::read_to_string(&board).unwrap(), text);
    }

    /// KiCad numbers dielectrics by place, so a kept value comes from the
    /// dielectric that stands there, whatever the file calls it.
    #[tokio::test]
    async fn a_kept_dielectric_value_is_found_by_place() {
        let text = fixture_text(WITH_STACKUP)
            .replace("(layer \"dielectric 1\"", "(layer \"dielectric 7\"")
            .replace("(material \"FR4\")", "(material \"FR4-TG170\")");
        let (_dir, board) = board_from(&text);
        let body = call(two_layer(&board)).await;
        assert_ne!(body["is_error"], true, "{body}");
        let (stack, _) = written(&board);
        let dielectric = entry(&stack, "dielectric 1");
        assert_eq!(dielectric.material.as_deref(), Some("FR4-TG170"));
        assert_eq!(dielectric.color.as_deref(), Some("FR4 natural"));
    }

    /// A mask or silkscreen asked of a board that has none is refused, not
    /// dropped.
    #[tokio::test]
    async fn a_surface_the_board_lacks_is_refused() {
        let mut text = fixture_text(TWO_LAYER);
        for line in ["\t\t(1 \"F.Mask\" user)\n", "\t\t(3 \"B.Mask\" user)\n"] {
            assert!(text.contains(line));
            text = text.replace(line, "");
        }
        let (_dir, board) = board_from(&text);
        let mut request = two_layer(&board);
        request["solder_mask"] = json!({"color": "Green"});
        let body = call(request).await;
        assert_eq!(body["error"]["kind"], "invalid_argument", "{body}");
        assert!(
            body.to_string().contains("neither F.Mask nor B.Mask"),
            "{body}"
        );
        assert_eq!(std::fs::read_to_string(&board).unwrap(), text);
        let body = call(two_layer(&board)).await;
        assert_ne!(body["is_error"], true, "{body}");
    }

    /// Copper that is not F.Cu, In1.Cu … B.Cu is no stack KiCad builds.
    #[tokio::test]
    async fn an_irregular_copper_set_is_refused() {
        // Four copper layers, an even count, but In2 and In3 with no In1.
        let text = fixture_text(TWO_LAYER).replace(
            "\t\t(2 \"B.Cu\" signal)\n",
            "\t\t(2 \"B.Cu\" signal)\n\t\t(6 \"In2.Cu\" signal)\n\t\t(8 \"In3.Cu\" signal)\n",
        );
        let (_dir, board) = board_from(&text);
        let copper = vec![json!({"thickness_mm": 0.035}); 4];
        let dielectrics = vec![json!({"type": "core", "thickness_mm": 0.5}); 3];
        let body =
            call(json!({"board": board, "copper_layers": copper, "dielectrics": dielectrics}))
                .await;
        assert_eq!(body["error"]["kind"], "plan_blocked", "{body}");
        assert!(body.to_string().contains("In2.Cu"), "{body}");
        assert_eq!(std::fs::read_to_string(&board).unwrap(), text);
    }

    /// KiCad opening the board while kicad-cli loads the copy is caught by the
    /// check before the write, which asks KiCad as well as its lock. The shared
    /// writer refuses the lock too, in its own words; this pins the check.
    #[tokio::test]
    async fn a_board_kicad_opens_during_the_probe_is_refused_untouched() {
        let (dir, board) = board(TWO_LAYER);
        let lock = copperpour_sexp::writer::kicad_editor_lock_path(&board).unwrap();
        let cli = crate::tools::cli::test_support::write_script(
            dir.path(),
            "kicad-cli",
            &format!("#!/bin/sh\ntouch \"{}\"\nexit 0\n", lock.display()),
            &format!(
                "@echo off\r\ntype nul > \"{}\"\r\nexit /b 0\r\n",
                lock.display()
            ),
        );
        let body = call_with(&cli.display().to_string(), two_layer(&board)).await;
        assert_eq!(body["error"]["kind"], "unsafe_file_fallback", "{body}");
        assert!(
            body["message"]
                .as_str()
                .unwrap()
                .contains("its contents cannot prove"),
            "{body}"
        );
        assert_eq!(
            std::fs::read_to_string(&board).unwrap(),
            fixture_text(TWO_LAYER)
        );
    }

    /// A board without the blocks KiCad always writes is refused untouched.
    #[tokio::test]
    async fn a_board_without_setup_or_thickness_is_refused() {
        let fixture = fixture_text(TWO_LAYER);
        let (start, _) = direct_children_with_tag(&fixture, "kicad_pcb", "setup")[0];
        let (from, to) = find_block_with_leading_whitespace(&fixture, start).unwrap();
        let no_setup = format!("{}{}", &fixture[..from], &fixture[to..]);
        let no_thickness = fixture.replace("\t\t(thickness 1.6)\n", "");
        for (text, says) in [
            (no_setup, "no (setup …)"),
            (no_thickness, "no (general (thickness"),
        ] {
            let (_dir, board) = board_from(&text);
            let body = call(two_layer(&board)).await;
            assert_eq!(body["error"]["kind"], "plan_blocked", "{body}");
            assert!(body["message"].as_str().unwrap().contains(says), "{body}");
            assert_eq!(std::fs::read_to_string(&board).unwrap(), text);
        }
    }

    #[tokio::test]
    async fn a_crlf_board_keeps_crlf_and_tabs() {
        let (_dir, board) = board(SIX_LAYER_CRLF);
        let copper = vec![json!({"thickness_mm": 0.035}); 6];
        let dielectrics = vec![json!({"type": "core", "thickness_mm": 0.3}); 5];
        let body =
            call(json!({"board": board, "copper_layers": copper, "dielectrics": dielectrics}))
                .await;
        assert_ne!(body["is_error"], true, "{body}");
        let text = std::fs::read_to_string(&board).unwrap();
        assert_eq!(text.matches('\n').count(), text.matches("\r\n").count());
        assert!(text.contains("\r\n\t\t(stackup\r\n\t\t\t(layer \"F.SilkS\"\r\n"));
        assert_eq!(without_stackup(&text, "1.6"), fixture_text(SIX_LAYER_CRLF));
    }

    #[tokio::test]
    async fn what_the_call_leaves_out_stays_as_the_board_has_it() {
        // The dielectric's values made other than KiCad's defaults.
        let text = fixture_text(WITH_STACKUP)
            .replace("(material \"FR4\")", "(material \"FR4-TG170\")")
            .replace("(loss_tangent 0.02)", "(loss_tangent 0.018)");
        let (_dir, board) = board_from(&text);
        let mut request = two_layer(&board);
        request["dielectrics"][0]["thickness_mm"] = json!(1.5);
        let body = call(request).await;
        assert_ne!(body["is_error"], true, "{body}");
        assert_eq!(
            (&body["edge_connector"], &body["has_edge_plating"]),
            (&json!("beveled"), &json!(true))
        );
        let (stack, thickness) = written(&board);
        let mask = entry(&stack, "B.Mask");
        assert_eq!(mask.color.as_deref(), Some("Green"));
        assert_eq!(mask.thickness_nm, Some(12_000));
        assert_eq!(mask.material.as_deref(), Some("Epoxy"));
        assert_eq!(
            (mask.epsilon_r, mask.loss_tangent),
            (Some(3.5), Some(0.025))
        );
        let silk = entry(&stack, "F.SilkS");
        assert_eq!(
            (silk.color.as_deref(), silk.material.as_deref()),
            (Some("White"), Some("Liquid Ink"))
        );
        let dielectric = entry(&stack, "dielectric 1");
        assert_eq!(dielectric.color.as_deref(), Some("FR4 natural"));
        assert!(dielectric.locked);
        assert_eq!(dielectric.thickness_nm, Some(1_500_000));
        assert_eq!(dielectric.material.as_deref(), Some("FR4-TG170"));
        assert_eq!(
            (dielectric.epsilon_r, dielectric.loss_tangent),
            (Some(4.6), Some(0.018))
        );
        assert_eq!(stack.finish.as_deref(), Some("ENIG"));
        assert!(stack.dielectric_constraints);
        assert_eq!(stack.edge_connector.as_deref(), Some("bevelled"));
        assert!(stack.edge_plating);
        assert_eq!(thickness, Some(1_594_000));
    }

    /// The fixture is KiCad's own save, so a call restating its stack writes
    /// the same text: the block is KiCad's layout.
    #[tokio::test]
    async fn the_stack_a_board_holds_is_written_as_kicad_wrote_it() {
        let (_dir, board) = board(WITH_STACKUP);
        let mut request = two_layer(&board);
        request["dielectrics"][0] =
            json!({"type": "core", "thickness_mm": 1.488, "epsilon_r": 4.6});
        let body = call(request).await;
        assert_ne!(body["is_error"], true, "{body}");
        assert_eq!(body["changed"], false);
        assert!(body["warning"].is_null(), "{body}");
        assert_eq!(
            std::fs::read_to_string(&board).unwrap(),
            fixture_text(WITH_STACKUP)
        );
    }

    /// Each refused before anything is written, the board left byte for byte.
    #[tokio::test]
    async fn malformed_requests_are_refused_and_write_nothing() {
        let cases: Vec<(&str, Change, &str)> = vec![
            (
                "three copper layers on two",
                Box::new(|r| r["copper_layers"] = json!(vec![json!({"thickness_mm": 0.035}); 3])),
                "Board Setup does, to an even count",
            ),
            (
                "two dielectrics on two layers",
                Box::new(|r| {
                    r["dielectrics"] = json!(vec![json!({"type": "core", "thickness_mm": 0.8}); 2])
                }),
                "2 copper layers need 1",
            ),
            (
                "thickness and weight",
                Box::new(|r| r["copper_layers"][0]["weight_oz"] = json!(1)),
                "not both",
            ),
            (
                "neither",
                Box::new(|r| r["copper_layers"][0] = json!({})),
                "copper_layers[0]",
            ),
            (
                "thickness assertion",
                Box::new(|r| r["board_thickness_mm"] = json!(1.5)),
                "sums to 1.6 mm, not 1.5 mm",
            ),
            ("finish", Box::new(|r| r["finish"] = json!("HASL")), ""),
            (
                "color",
                Box::new(|r| r["solder_mask"] = json!({"color": "green"})),
                "",
            ),
            (
                "zero thickness",
                Box::new(|r| r["dielectrics"][0]["thickness_mm"] = json!(0)),
                "",
            ),
            (
                "below a nanometre",
                Box::new(|r| r["copper_layers"][0]["thickness_mm"] = json!(1e-7)),
                "rounds to zero",
            ),
            (
                "beyond KiCad's unit",
                Box::new(|r| r["dielectrics"][0]["thickness_mm"] = json!(2148)),
                "thickness is more than",
            ),
            (
                "the issue's copper_finish",
                Box::new(|r| r["copper_finish"] = json!("ENIG")),
                "",
            ),
            (
                "a misspelt εr",
                Box::new(|r| r["dielectrics"][0]["epsilon"] = json!(4.6)),
                "",
            ),
            (
                "an unspecified material",
                Box::new(|r| r["dielectrics"][0]["material"] = json!("Not specified")),
                "drops an empty or",
            ),
            (
                "dielectric type",
                Box::new(|r| r["dielectrics"][0]["type"] = json!("air")),
                "",
            ),
        ];
        for (name, change, says) in cases {
            let (_dir, board) = board(TWO_LAYER);
            let mut request = two_layer(&board);
            change(&mut request);
            let body = call(request).await;
            assert_eq!(body["error"]["kind"], "invalid_argument", "{name}: {body}");
            assert!(body.to_string().contains(says), "{name}: {body}");
            assert_eq!(
                std::fs::read_to_string(&board).unwrap(),
                fixture_text(TWO_LAYER),
                "{name}"
            );
        }
    }

    /// The handler holds the schema's domain rules for a call that does not
    /// pass through the dispatch.
    #[tokio::test]
    async fn a_direct_call_is_held_to_the_schemas_rules() {
        let ctx = crate::tools::pcb_board::board_mock::ctx_talking_to(String::new());
        let cases: Vec<(&str, Change, &str)> = vec![
            (
                "finish",
                Box::new(|r| r["finish"] = json!("HASL")),
                "finish",
            ),
            (
                "color",
                Box::new(|r| r["solder_mask"] = json!({"color": "green"})),
                "solder_mask.color",
            ),
            (
                "hex color",
                Box::new(|r| r["silkscreen"] = json!({"color": "#12345"})),
                "silkscreen.color",
            ),
            (
                "εr",
                Box::new(|r| r["dielectrics"][0]["epsilon_r"] = json!(0)),
                "dielectrics[0]",
            ),
            (
                "loss tangent",
                Box::new(|r| r["dielectrics"][0]["loss_tangent"] = json!(-0.1)),
                "dielectrics[0]",
            ),
            (
                "material",
                Box::new(|r| r["dielectrics"][0]["material"] = json!(" ")),
                "dielectrics[0]",
            ),
            (
                "dielectric type",
                Box::new(|r| r["dielectrics"][0]["type"] = json!("air")),
                "dielectrics[0]",
            ),
            (
                "a boolean as text",
                Box::new(|r| r["impedance_controlled"] = json!("yes")),
                "impedance_controlled",
            ),
            (
                "a finish as a number",
                Box::new(|r| r["finish"] = json!(5)),
                "finish",
            ),
            (
                "a number as text",
                Box::new(|r| r["board_thickness_mm"] = json!("1.6")),
                "board_thickness_mm",
            ),
            (
                "a mask that is no object",
                Box::new(|r| r["solder_mask"] = json!("Green")),
                "solder_mask",
            ),
            (
                "a weight as text beside a thickness",
                Box::new(|r| r["copper_layers"][0]["weight_oz"] = json!("1")),
                "copper_layers[0]",
            ),
        ];
        for (name, change, field) in cases {
            let (_dir, board) = board(TWO_LAYER);
            let mut request = two_layer(&board);
            change(&mut request);
            let result = handle_set_board_stackup(&request, &ctx).await.unwrap();
            let shown = format!("{:?}", result.content);
            assert!(result.is_error, "{name}: {shown}");
            assert!(
                shown.contains("invalid_argument") && shown.contains(field),
                "{name}: {shown}"
            );
            assert_eq!(
                std::fs::read_to_string(&board).unwrap(),
                fixture_text(TWO_LAYER),
                "{name}"
            );
        }
    }

    #[tokio::test]
    async fn a_dielectric_with_sub_layers_is_left_to_board_setup() {
        let text = fixture_text(WITH_STACKUP).replace(
            "(thickness 1.488 locked)",
            "(thickness 1.0 locked)\n\t\t\t\taddsublayer\n\t\t\t\t(thickness 0.488)",
        );
        let (_dir, board) = board_from(&text);
        let body = call(two_layer(&board)).await;
        assert_eq!(body["error"]["kind"], "plan_blocked", "{body}");
        assert!(
            body.to_string().contains("dielectric 1 has 2 sub-layers"),
            "{body}"
        );
        assert_eq!(std::fs::read_to_string(&board).unwrap(), text);
    }

    /// KiCad's save of the copy holding another stack than the one written,
    /// as when it reads a name as a dielectric, refuses the write.
    #[tokio::test]
    async fn a_stack_kicad_reads_otherwise_is_not_written() {
        let (dir, board) = board(WITH_STACKUP);
        let kicad = dir.path().join("as-kicad-read.kicad_pcb");
        std::fs::copy(&board, &kicad).unwrap();
        let cli = fake_kicad_cli(dir.path(), Some(&kicad), 0);
        let mut request = two_layer(&board);
        request["dielectrics"][0]["thickness_mm"] = json!(1.5);
        let body = call_with(&cli.display().to_string(), request).await;
        assert_eq!(body["error"]["kind"], "plan_blocked", "{body}");
        let message = body["message"].as_str().unwrap();
        assert!(
            message.contains("KiCad reads this stackup otherwise"),
            "{message}"
        );
        assert!(
            message.contains("dielectric 1 thickness is read as 1.488 mm, written as 1.5 mm"),
            "{message}"
        );
        assert!(
            message.contains("the board thickness is read as 1.582 mm"),
            "{message}"
        );
        assert_eq!(
            std::fs::read_to_string(&board).unwrap(),
            fixture_text(WITH_STACKUP)
        );
    }

    /// A copy KiCad saves with other layers, or with no stack to read, refuses
    /// the write as well.
    #[tokio::test]
    async fn a_copy_kicad_saves_without_the_stack_is_not_written() {
        let fixture = fixture_text(WITH_STACKUP);
        let paste = "\t\t\t(layer \"F.Paste\"\n\t\t\t\t(type \"Top Solder Paste\")\n\t\t\t)\n";
        assert!(fixture.contains(paste));
        for (saved, says) in [
            (fixture.replace(paste, ""), "it holds the layers"),
            (fixture_text(TWO_LAYER), "whose stackup cannot be read"),
        ] {
            let (dir, board) = board(WITH_STACKUP);
            let kicad = dir.path().join("as-kicad-read.kicad_pcb");
            std::fs::write(&kicad, saved).unwrap();
            let cli = fake_kicad_cli(dir.path(), Some(&kicad), 0);
            let body = call_with(&cli.display().to_string(), two_layer(&board)).await;
            assert_eq!(body["error"]["kind"], "plan_blocked", "{body}");
            assert!(body["message"].as_str().unwrap().contains(says), "{body}");
            assert_eq!(std::fs::read_to_string(&board).unwrap(), fixture);
        }
    }

    #[tokio::test]
    async fn without_a_working_kicad_cli_nothing_is_written() {
        let dir = tempfile::tempdir().unwrap();
        let failing = fake_kicad_cli(dir.path(), None, 3).display().to_string();
        for cli in [String::new(), failing] {
            let (_dir, board) = board(TWO_LAYER);
            let body = call_with(&cli, two_layer(&board)).await;
            assert_eq!(body["error"]["kind"], "plan_blocked", "{cli:?}: {body}");
            assert!(
                body.to_string()
                    .contains("kicad-cli could not load and save a copy"),
                "{body}"
            );
            assert_eq!(
                std::fs::read_to_string(&board).unwrap(),
                fixture_text(TWO_LAYER)
            );
        }
    }

    #[tokio::test]
    async fn a_board_saved_after_the_read_is_left_as_saved() {
        let (_dir, board) = board(TWO_LAYER);
        let saved = fixture_text(FOUR_LAYER);
        write_hook::set(saved.clone());
        let body = call(two_layer(&board)).await;
        assert!(!write_hook::armed());
        assert_eq!(body["error"]["kind"], "conflict", "{body}");
        assert_eq!(std::fs::read_to_string(&board).unwrap(), saved);
    }

    /// Refused before the probe and again before the write, since KiCad may
    /// open the board while kicad-cli loads its copy; with a kicad-cli that
    /// works, either check alone holds this test.
    #[tokio::test]
    async fn a_board_kicad_holds_is_refused_untouched() {
        use crate::tools::pcb_board::board_mock::{ctx_talking_to, spawn_kicad_holding_boards};
        let (dir, board) = board(TWO_LAYER);
        let server = spawn_kicad_holding_boards(&[board.as_path()], |_| None);
        let mut ctx = ctx_talking_to(server.address().to_string());
        ctx.config.kicad_cli = fake_kicad_cli(dir.path(), None, 0).display().to_string();
        let result = handle_set_board_stackup(&two_layer(&board), &ctx)
            .await
            .unwrap();
        assert!(result.is_error);
        assert!(
            format!("{:?}", result.content).contains("holds this board open"),
            "{:?}",
            result.content
        );
        assert_eq!(
            std::fs::read_to_string(&board).unwrap(),
            fixture_text(TWO_LAYER)
        );
    }

    /// Live, against a real kicad-cli: each stack is written, and KiCad's own
    /// load and save of the result keeps its stackup and thickness byte for
    /// byte.
    ///
    ///     KICAD_CLI=… cargo test -p copperpour-core --lib board_stackup_set::tests::live -- --ignored
    #[tokio::test]
    #[ignore = "needs a real kicad-cli"]
    async fn live_kicad_saves_the_written_stack_unchanged() {
        let cli = std::env::var("KICAD_CLI").unwrap_or_else(|_| "kicad-cli".to_string());
        let four = |board: &Path| {
            json!({
                "board": board,
                "copper_layers": [{"thickness_mm": 0.035}, {"weight_oz": 0.5}, {"weight_oz": 0.5}, {"thickness_mm": 0.035}],
                "dielectrics": [
                    {"type": "prepreg", "thickness_mm": 0.2104, "epsilon_r": 4.4, "loss_tangent": 0.018},
                    {"type": "core", "thickness_mm": 1.065, "material": "FR4 \"TG170\" \\ high\r\nTg", "epsilon_r": 4.6},
                    {"type": "prepreg", "thickness_mm": 0.2104, "epsilon_r": 4.4, "loss_tangent": 0.018}
                ],
                "solder_mask": {"color": "#1A5C2ECC", "thickness_mm": 0.015},
                "silkscreen": {"color": "White"},
                "finish": "ENIG",
                "impedance_controlled": true
            })
        };
        let no_paste = fixture_text(TWO_LAYER)
            .replace("\t\t(13 \"F.Paste\" user)\n", "")
            .replace("\t\t(15 \"B.Paste\" user)\n", "");
        let cases: Vec<(&str, String, RequestFor)> = vec![
            ("no stackup", fixture_text(TWO_LAYER), Box::new(two_layer)),
            ("four layers", fixture_text(FOUR_LAYER), Box::new(four)),
            (
                "over a stackup",
                fixture_text(WITH_STACKUP),
                Box::new(two_layer),
            ),
            ("no paste layers", no_paste, Box::new(two_layer)),
        ];
        for (name, text, request) in cases {
            let (_dir, board) = board_from(&text);
            let body = call_with(&cli, request(&board)).await;
            assert_ne!(body["is_error"], true, "{name}: {body}");
            let ours = std::fs::read_to_string(&board).unwrap();
            crate::tools::cli::resave_board(&cli, &board).await.unwrap();
            let kicads = std::fs::read_to_string(&board).unwrap();
            let block = |text: &str| {
                let (start, end) = direct_children_with_tag(text, "setup", "stackup")[0];
                let (t0, t1) = direct_children_with_tag(text, "general", "thickness")[0];
                (text[start..end].to_string(), text[t0..t1].to_string())
            };
            assert_eq!(block(&kicads), block(&ours), "{name}");
        }

        // A sum past what KiCad's reader holds: KiCad clamps it, and the probe
        // refuses the write.
        let (_dir, board) = board(TWO_LAYER);
        let mut request = two_layer(&board);
        request["dielectrics"][0]["thickness_mm"] = json!(2147.47);
        let body = call_with(&cli, request).await;
        assert_eq!(body["error"]["kind"], "plan_blocked", "{body}");
        assert!(
            body["message"]
                .as_str()
                .unwrap()
                .contains("board thickness is read as"),
            "{body}"
        );
        assert_eq!(
            std::fs::read_to_string(&board).unwrap(),
            fixture_text(TWO_LAYER)
        );
    }
}
