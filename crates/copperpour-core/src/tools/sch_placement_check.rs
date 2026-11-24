//! `check_schematic_placement`: a static placement check of one saved sheet.
//!
//! Overlaps come from [`overlap_findings`], the same code behind
//! `check_schematic_overlaps`, so the two tools cannot disagree. Pin positions
//! come from [`drawn_symbol_pins`], the selected unit and body style KiCad draws.

use super::sch_analysis::{overlap_findings, OVERLAP_TOLERANCE};
use super::sch_layout_score::read_saved_sheet;
use super::{drawn_symbol_pins, get_path, ToolDef, UNRESOLVED_PIN_GEOMETRY};
use crate::mcp::{error::ToolErrorKind, protocol::CallToolResult};
use crate::tool;
use copperpour_sexp::geometry::{is_on_schematic_pin_grid, SCHEMATIC_PIN_GRID_MM};
use copperpour_sexp::schematic::{extract_wires, parse_at, pin_endpoint, SymbolInstance};
use serde_json::{json, Value};
use std::path::Path;

pub(super) fn definition() -> ToolDef {
    tool!(
        "check_schematic_placement",
        "Check one saved schematic sheet's placement: the overlaps check_schematic_overlaps \
         reports, from the same code, and placed symbol pin tips off KiCad's nominal 1.27 mm \
         grid. Refuses a sheet whose symbol pins cannot all be resolved rather than \
         reporting it clean. Does not open KiCad, follow child sheets or write.",
        json!({
            "type": "object",
            "properties": {
                "schematic": {"type": "string", "description": "Path to the saved .kicad_sch sheet"}
            },
            "required": ["schematic"]
        }),
        |args, _ctx| async move { handle_check_schematic_placement(args) }
    )
}

fn handle_check_schematic_placement(args: &Value) -> anyhow::Result<CallToolResult> {
    let path = get_path(args, "schematic")?;
    Ok(match check_placement(&path) {
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

fn malformed(path: &Path, reason: &str) -> CallToolResult {
    CallToolResult::error_kind(
        ToolErrorKind::MalformedFile {
            path: path.display().to_string(),
            reason: reason.to_owned(),
        },
        format!("Cannot check {}: {reason}", path.display()),
    )
}

fn check_placement(path: &Path) -> Result<Value, CallToolResult> {
    let (_, tree) = read_saved_sheet(path)?;
    let placed =
        drawn_symbol_pins(&tree).ok_or_else(|| malformed(path, UNRESOLVED_PIN_GEOMETRY))?;
    let mut pins: Vec<PinPosition<'_>> = Vec::new();
    for (symbol, unit_pins) in &placed {
        for (pin, transform) in unit_pins {
            let at = pin_endpoint(pin, *transform);
            if pin.number.is_empty() || !at.0.is_finite() || !at.1.is_finite() {
                return Err(malformed(path, UNRESOLVED_PIN_GEOMETRY));
            }
            // Stacked pins sharing a number and position are one connection
            // point, as KiCad reports them once.
            let stacked = pins.iter().any(|seen| {
                std::ptr::eq(seen.symbol, symbol) && seen.number == pin.number && seen.at == at
            });
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
    let (overlaps, bounds_resolved, bounds_unresolved) =
        overlap_findings(&tree, &wires, OVERLAP_TOLERANCE);
    Ok(json!({
        "schematic": path,
        "source": "saved",
        "pin_grid_mm": SCHEMATIC_PIN_GRID_MM,
        "pin_count": pins.len(),
        "off_grid_pin_count": off_grid_pins.len(),
        "off_grid_pins": off_grid_pins,
        "overlap_count": overlaps.len(),
        "overlaps": overlaps,
        "bounds_resolved": bounds_resolved,
        "bounds_unresolved": bounds_unresolved,
    }))
}

#[cfg(test)]
#[path = "sch_placement_check_tests.rs"]
mod tests;
