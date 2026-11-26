//! `check_placement`: a static placement check of one saved schematic sheet.
//!
//! Overlaps come from [`overlap_findings`], the same code behind
//! `check_schematic_overlaps`, at its default tolerance. Pin positions
//! come from [`drawn_symbol_pins`], the selected unit and body style KiCad draws.

use super::sch_analysis::{overlap_findings, OVERLAP_TOLERANCE};
use super::sch_layout_score::read_saved_sheet;
use super::{
    drawn_symbol_pins, get_path, invalid_arg, PlacedPinsByReference, ToolDef,
    UNRESOLVED_PIN_GEOMETRY,
};
use crate::mcp::{error::ToolErrorKind, protocol::CallToolResult};
use crate::tool;
use copperpour_sexp::geometry::{is_on_schematic_pin_grid, round6, SCHEMATIC_PIN_GRID_MM};
use copperpour_sexp::schematic::{extract_wires, parse_at, pin_endpoint, SymbolInstance};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::path::Path;

#[derive(Clone, Copy, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
enum Alignment {
    Row,
    Column,
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
            "symbol_uuid": {"type": "string", "minLength": 1, "description": "Saved placed symbol UUID"},
            "pin_number": {"type": "string", "minLength": 1, "description": "Pin number on that placed unit and body style"}
        },
        "required": ["symbol_uuid", "pin_number"]
    });
    tool!(
        "check_placement",
        "Check one saved schematic sheet's placement: the overlaps check_schematic_overlaps \
         reports, from the same code, and placed symbol pin tips off KiCad's nominal 1.27 mm \
         grid. Optionally check caller-specified pin pairs for row or column alignment. \
         Refuses unresolved pin geometry or a pair that does not name two unique pins. \
         Does not open KiCad, follow child sheets or write.",
        json!({
            "type": "object",
            "properties": {
                "schematic": {"type": "string", "description": "Path to the saved .kicad_sch sheet"},
                "alignment_pairs": {
                    "type": ["array", "null"],
                    "description": "Pairs to check. Row means equal pin-tip Y; column means equal pin-tip X. Omit or pass null or [] to check no pairs.",
                    "items": {
                        "type": "object",
                        "properties": {
                            "a": pin,
                            "b": pin,
                            "alignment": {"type": "string", "enum": ["row", "column"]}
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
    let pairs =
        match serde_json::from_value::<Option<Vec<AlignmentPair>>>(args["alignment_pairs"].clone())
        {
            Ok(pairs) => pairs.unwrap_or_default(),
            Err(err) => return Ok(invalid_arg("alignment_pairs", &err.to_string())),
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

fn stale_pin(target: String, reason: &str) -> CallToolResult {
    let message = format!("Cannot resolve {target}: {reason}. Check the saved sheet and retry.");
    CallToolResult::error_kind(
        ToolErrorKind::StaleTarget {
            target,
            reason: reason.to_owned(),
        },
        message,
    )
}

fn ambiguous_pin(target: String, candidates: Vec<String>) -> CallToolResult {
    let message = format!(
        "Cannot resolve {target}: {} matches on the saved sheet. \
         Correct the duplicate identity in the sheet or select another pin.",
        candidates.len()
    );
    CallToolResult::error_kind(
        ToolErrorKind::AmbiguousTarget { target, candidates },
        message,
    )
}

fn pair_pin<'a, 'sheet>(
    placed: &PlacedPinsByReference,
    pins: &'a [PinPosition<'sheet>],
    selector: &PinSelector,
    field: &str,
) -> Result<&'a PinPosition<'sheet>, CallToolResult> {
    let uuid = field.to_owned() + ".symbol_uuid";
    if uuid::Uuid::parse_str(&selector.symbol_uuid).is_err() {
        return Err(invalid_arg(&uuid, "expected a saved symbol UUID"));
    }
    let mut symbols = placed
        .iter()
        .enumerate()
        .filter(|(_, (symbol, _))| symbol.uuid.as_deref() == Some(selector.symbol_uuid.as_str()));
    let Some(first) = symbols.next() else {
        return Err(stale_pin(uuid, "symbol UUID not found on the saved sheet"));
    };
    if let Some(second) = symbols.next() {
        let candidates = [first, second]
            .into_iter()
            .chain(symbols)
            .map(|(index, (symbol, _))| {
                format!(
                    "symbol #{}: {} unit {}, UUID {}, origin ({}, {}) mm",
                    index + 1,
                    symbol.reference,
                    symbol.unit,
                    selector.symbol_uuid,
                    symbol.x,
                    symbol.y
                )
            })
            .collect();
        return Err(ambiguous_pin(uuid, candidates));
    }
    let number = field.to_owned() + ".pin_number";
    if selector.pin_number.is_empty() {
        return Err(invalid_arg(&number, "expected a non-empty pin number"));
    }
    let mut matching = pins.iter().filter(|pin| {
        pin.symbol.uuid.as_deref() == Some(selector.symbol_uuid.as_str())
            && pin.number == selector.pin_number
    });
    let Some(pin) = matching.next() else {
        return Err(stale_pin(number, "pin not found on the placed symbol"));
    };
    if let Some(second) = matching.next() {
        let candidates = [pin, second]
            .into_iter()
            .chain(matching)
            .map(|pin| {
                format!(
                    "{} unit {}, UUID {}, pin {} at ({}, {}) mm",
                    pin.symbol.reference,
                    pin.symbol.unit,
                    selector.symbol_uuid,
                    pin.number,
                    pin.at.0,
                    pin.at.1
                )
            })
            .collect();
        return Err(ambiguous_pin(number, candidates));
    }
    Ok(pin)
}

fn malformed(path: &Path, reason: &str) -> CallToolResult {
    CallToolResult::error_kind(
        ToolErrorKind::MalformedFile {
            path: path.display().to_string(),
            reason: reason.to_owned(),
        },
        format!("Cannot check {}: {reason}", path.display()),
    )
}

fn check_placement(path: &Path, pairs: &[AlignmentPair]) -> Result<Value, CallToolResult> {
    let (_, tree) = read_saved_sheet(path)?;
    let placed =
        drawn_symbol_pins(&tree).ok_or_else(|| malformed(path, UNRESOLVED_PIN_GEOMETRY))?;
    let mut pins: Vec<PinPosition<'_>> = Vec::new();
    for (symbol, unit_pins) in &placed {
        let start = pins.len();
        for (pin, transform) in unit_pins {
            let at = pin_endpoint(pin, *transform);
            if pin.number.is_empty() || !at.0.is_finite() || !at.1.is_finite() {
                return Err(malformed(path, UNRESOLVED_PIN_GEOMETRY));
            }
            // Stacked pins sharing a number and position are one connection
            // point, as KiCad reports them once.
            let stacked = pins[start..]
                .iter()
                .any(|seen| seen.number == pin.number && seen.at == at);
            if !stacked {
                pins.push(PinPosition {
                    symbol,
                    number: &pin.number,
                    at,
                });
            }
        }
    }
    let wires = extract_wires(&tree);
    if wires.len() != tree.find_all("wire").len()
        || wires
            .iter()
            .any(|w| ![w.x1, w.y1, w.x2, w.y2].into_iter().all(f64::is_finite))
    {
        return Err(malformed(
            path,
            "one or more wires have unreadable endpoints",
        ));
    }
    if ["label", "global_label", "hierarchical_label"]
        .into_iter()
        .flat_map(|kind| tree.find_all(kind))
        .any(|label| {
            !parse_at(label)
                .is_some_and(|(x, y, angle)| [x, y, angle].into_iter().all(f64::is_finite))
        })
    {
        return Err(malformed(
            path,
            "one or more labels have unreadable anchors",
        ));
    }

    let off_grid_pins: Vec<_> = pins
        .iter()
        .filter(|pin| !is_on_schematic_pin_grid(pin.at.0) || !is_on_schematic_pin_grid(pin.at.1))
        .map(PinPosition::json)
        .collect();
    let mut misaligned_pairs = Vec::new();
    for (index, pair) in pairs.iter().enumerate() {
        let field = format!("alignment_pairs[{index}]");
        if pair.a == pair.b {
            return Err(invalid_arg(&field, "expected two distinct pin selectors"));
        }
        let a = pair_pin(&placed, &pins, &pair.a, &format!("{field}.a"))?;
        let b = pair_pin(&placed, &pins, &pair.b, &format!("{field}.b"))?;
        let (left, right) = match pair.alignment {
            Alignment::Row => (a.at.1, b.at.1),
            Alignment::Column => (a.at.0, b.at.0),
        };
        // Pin endpoints are already rounded by the shared schematic transform.
        if left != right {
            let offset_mm = round6((left - right).abs());
            if !offset_mm.is_finite() {
                return Err(malformed(path, "pin alignment offset is not finite"));
            }
            misaligned_pairs.push(json!({
                "pair_index": index,
                "alignment": pair.alignment,
                "offset_mm": offset_mm,
                "a": a.json(),
                "b": b.json(),
            }));
        }
    }
    let (overlaps, bounds_resolved, bounds_unresolved) =
        overlap_findings(&tree, &wires, OVERLAP_TOLERANCE);
    Ok(json!({
        "schematic": path,
        "source": "saved",
        "pin_grid_mm": SCHEMATIC_PIN_GRID_MM,
        "pin_count": pins.len(),
        "off_grid_pin_count": off_grid_pins.len(),
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
#[path = "sch_placement_check_tests.rs"]
mod tests;
