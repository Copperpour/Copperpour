//! Static checks of one saved schematic sheet.

use super::sch_analysis::{overlap_findings, OVERLAP_TOLERANCE};
use super::sch_layout_score::read_saved_sheet;
use super::{drawn_symbol_pins, get_path, invalid_arg, ToolDef, UNRESOLVED_PIN_GEOMETRY};
use crate::mcp::{error::ToolErrorKind, protocol::CallToolResult};
use crate::tool;
use copperpour_sexp::geometry::{is_on_schematic_pin_grid, round6, SCHEMATIC_PIN_GRID_MM};
use copperpour_sexp::schematic::{extract_wires, parse_at, pin_endpoint, SymbolInstance};
use serde::Deserialize;
use serde_json::{json, Value};
use std::path::Path;

#[derive(Clone, Copy, Deserialize)]
#[serde(rename_all = "snake_case")]
enum Alignment {
    Row,
    Column,
}

impl Alignment {
    fn name_and_offset(self, a: (f64, f64), b: (f64, f64)) -> (&'static str, f64) {
        match self {
            Self::Row => ("row", round6((a.1 - b.1).abs())),
            Self::Column => ("column", round6((a.0 - b.0).abs())),
        }
    }
}

#[derive(Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
struct PinSelector {
    symbol_uuid: String,
    pin_number: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct AlignmentPair {
    a: PinSelector,
    b: PinSelector,
    alignment: Alignment,
}

pub(super) fn definition() -> ToolDef {
    let pin = json!({
        "type": "object",
        "properties": {
            "symbol_uuid": {"type": "string", "minLength": 1},
            "pin_number": {"type": "string", "minLength": 1}
        },
        "required": ["symbol_uuid", "pin_number"]
    });
    tool!(
        "check_placement",
        "Check one saved schematic sheet for the same overlaps as check_schematic_overlaps, \
         placed symbol pin tips off the nominal 1.27 mm grid, and explicit pin pairs that \
         do not share a row or column. Does not open KiCad, follow child sheets or write.",
        json!({
            "type": "object",
            "properties": {
                "schematic": {"type": "string", "description": "Path to the saved .kicad_sch sheet"},
                "alignment_pairs": {
                    "type": ["array", "null"],
                    "description": "Pin pairs with an expected alignment. Row means same Y; column means same X.",
                    "items": {
                        "type": "object",
                        "properties": {
                            "a": pin,
                            "b": pin,
                            "alignment": {"enum": ["row", "column"]}
                        },
                        "required": ["a", "b", "alignment"]
                    }
                }
            },
            "required": ["schematic"]
        }),
        |args, _ctx| async move { handle_check_placement(args) }
    )
}

fn handle_check_placement(args: &Value) -> anyhow::Result<CallToolResult> {
    let path = get_path(args, "schematic")?;
    let pairs = if args["alignment_pairs"].is_null() {
        Vec::new()
    } else {
        match serde_json::from_value(args["alignment_pairs"].clone()) {
            Ok(pairs) => pairs,
            Err(err) => return Ok(invalid_arg("alignment_pairs", &err.to_string())),
        }
    };
    Ok(match check_placement(&path, &pairs) {
        Ok(report) => CallToolResult::json(&report),
        Err(err) => err,
    })
}

struct PinPosition<'a> {
    symbol: &'a SymbolInstance,
    number: &'a str,
    at: (f64, f64),
}

impl PinPosition<'_> {
    fn json(&self) -> Value {
        json!({
            "reference": self.symbol.reference,
            "symbol_uuid": self.symbol.uuid,
            "unit": self.symbol.unit,
            "pin_number": self.number,
            "x": self.at.0,
            "y": self.at.1,
        })
    }
}

fn pair_pin<'a, 'sheet>(
    pins: &'a [PinPosition<'sheet>],
    selector: &PinSelector,
    index: usize,
    side: &str,
) -> Result<&'a PinPosition<'sheet>, CallToolResult> {
    let error = |reason| invalid_arg(&format!("alignment_pairs[{index}].{side}"), reason);
    let mut matches = pins.iter().filter(|pin| {
        pin.symbol.uuid.as_deref() == Some(selector.symbol_uuid.as_str())
            && pin.number == selector.pin_number
    });
    let first = matches
        .next()
        .ok_or_else(|| error("pin not found on the saved sheet"))?;
    if matches.any(|pin| !std::ptr::eq(pin.symbol, first.symbol) || pin.at != first.at) {
        return Err(error("pin does not identify a unique placed endpoint"));
    }
    Ok(first)
}

fn check_placement(path: &Path, pairs: &[AlignmentPair]) -> Result<Value, CallToolResult> {
    let (_, tree) = read_saved_sheet(path)?;
    let malformed = |reason: &str| {
        CallToolResult::error_kind(
            ToolErrorKind::MalformedFile {
                path: path.display().to_string(),
                reason: reason.to_owned(),
            },
            format!("Cannot check {}: {reason}", path.display()),
        )
    };
    let placed = drawn_symbol_pins(&tree).ok_or_else(|| malformed(UNRESOLVED_PIN_GEOMETRY))?;
    let pins: Vec<_> = placed
        .iter()
        .flat_map(|(symbol, pins)| {
            pins.iter().map(move |(pin, transform)| PinPosition {
                symbol,
                number: &pin.number,
                at: pin_endpoint(pin, *transform),
            })
        })
        .collect();
    if pins
        .iter()
        .any(|pin| pin.number.is_empty() || !pin.at.0.is_finite() || !pin.at.1.is_finite())
    {
        return Err(malformed(UNRESOLVED_PIN_GEOMETRY));
    }
    let wires = extract_wires(&tree);
    if wires.len() != tree.find_all("wire").len()
        || wires
            .iter()
            .any(|w| ![w.x1, w.y1, w.x2, w.y2].into_iter().all(f64::is_finite))
    {
        return Err(malformed("one or more wires have unreadable endpoints"));
    }
    if ["label", "global_label", "hierarchical_label"]
        .into_iter()
        .flat_map(|kind| tree.find_all(kind))
        .any(|label| {
            !parse_at(label).is_some_and(|(x, y, angle)| {
                [x, y, angle].into_iter().all(f64::is_finite)
            })
        })
    {
        return Err(malformed("one or more labels have unreadable anchors"));
    }

    let mut misaligned_pairs = Vec::new();
    for (index, pair) in pairs.iter().enumerate() {
        if pair.a == pair.b {
            return Err(invalid_arg(
                &format!("alignment_pairs[{index}]"),
                "expected two distinct pins",
            ));
        }
        let a = pair_pin(&pins, &pair.a, index, "a")?;
        let b = pair_pin(&pins, &pair.b, index, "b")?;
        let (alignment, offset_mm) = pair.alignment.name_and_offset(a.at, b.at);
        if offset_mm > 0.0 {
            misaligned_pairs.push(json!({
                "pair_index": index,
                "a": a.json(),
                "b": b.json(),
                "alignment": alignment,
                "offset_mm": offset_mm,
            }));
        }
    }

    let off_grid_pins: Vec<_> = pins
        .iter()
        .filter(|pin| {
            !is_on_schematic_pin_grid(pin.at.0) || !is_on_schematic_pin_grid(pin.at.1)
        })
        .map(PinPosition::json)
        .collect();
    let (overlaps, bounds_resolved, bounds_unresolved) =
        overlap_findings(&tree, &wires, OVERLAP_TOLERANCE);
    Ok(json!({
        "schematic": path,
        "source": "saved",
        "pin_grid_mm": SCHEMATIC_PIN_GRID_MM,
        "pin_count": pins.len(),
        "off_grid_pins": off_grid_pins,
        "alignment_pair_count": pairs.len(),
        "misaligned_pairs": misaligned_pairs,
        "overlap_count": overlaps.len(),
        "overlaps": overlaps,
        "bounds_resolved": bounds_resolved,
        "bounds_unresolved": bounds_unresolved,
    }))
}

#[cfg(test)]
mod tests;
