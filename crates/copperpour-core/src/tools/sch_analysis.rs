//! `sch_analysis` toolset — net connectivity, pin queries, trace paths and orphan detection.
//!
//! All operations are read-only S-expression analysis. Connectivity — the net
//! graph and what counts as attached at a point — lives in `sch_connectivity`.

use crate::mcp::protocol::CallToolResult;
use crate::tool;
use crate::tools::reference_hint::component_not_found;
use crate::tools::sch_connectivity::{
    label_roots, net_graph_for, saved_net_labels, ConnectivityIndex, PinNetNames, UnitNotation,
};
use crate::tools::{
    get_path, invalid_arg, is_power_symbol_reference, opt_f64, opt_positive_f64,
    placed_pins_by_reference, require_f64, require_str, resolved_placed_pins_by_reference,
    PlacedPins, ToolContext, ToolDef, UNRESOLVED_PIN_GEOMETRY,
};
use copperpour_schematic_editor as cse;
use copperpour_sexp::{
    geometry::{point_on_segment, points_coincident},
    schematic::{
        extract_all_net_labels, extract_buses, extract_labels, extract_sheet_pins,
        extract_symbol_instances, extract_wires, find_lib_symbol, read_schematic,
        symbol_geometry_for_instance, Label, LabelKind, LibPin, SymbolBounds, SymbolInstance, Wire,
    },
    text_bounds::{text_extents, TextExtent, TextOwner},
};
use serde_json::json;
use std::collections::{BTreeSet, HashMap, HashSet};

pub fn tools() -> Vec<ToolDef> {
    vec![
        tool!(
            "list_schematic_wires",
            "List all wire segments in a schematic with start/end coordinates and UUIDs.",
            json!({ "type": "object",
                "properties": { "schematic": { "type": "string" } },
                "required": ["schematic"] }),
            |args, ctx| async move { handle_list_wires(args, ctx).await }
        ),
        tool!(
            "list_schematic_nets",
            "List all distinct net names derived from net labels, global labels, and power symbols.",
            json!({ "type": "object",
                "properties": { "schematic": { "type": "string" } },
                "required": ["schematic"] }),
            |args, ctx| async move { handle_list_nets(args, ctx).await }
        ),
        tool!(
            "list_schematic_labels",
            "List all net, global, and hierarchical labels with their positions, \
             net names, types (NetLabel, GlobalLabel, HierarchicalLabel), and UUIDs \
             (usable with batch_delete).",
            json!({ "type": "object",
                "properties": { "schematic": { "type": "string" } },
                "required": ["schematic"] }),
            |args, ctx| async move { handle_list_labels(args, ctx).await }
        ),
        tool!(
            "get_net_connections",
            "Get all pins and labels on a named net. Drawn label attachment uses saved \
             schematic coordinates and KiCad's one-unit allowance.",
            json!({ "type": "object",
                "properties": {
                    "schematic": { "type": "string" },
                    "net": { "type": "string", "description": "Net name to query" }
                },
                "required": ["schematic", "net"] }),
            |args, ctx| async move { handle_get_net_connections(args, ctx).await }
        ),
        tool!(
            "get_net_connectivity",
            "Return wires, labels, and T-junction locations for a named net. A nearby label \
             that KiCad leaves detached does not put the wire on that net.",
            json!({ "type": "object",
                "properties": {
                    "schematic": { "type": "string" },
                    "net": { "type": "string" }
                },
                "required": ["schematic", "net"] }),
            |args, ctx| async move { handle_get_net_connectivity(args, ctx).await }
        ),
        tool!(
            "get_pin_connections",
            "Get the primary net name at a specific pin using saved-coordinate label \
             attachment. Global power takes precedence over local power.",
            json!({ "type": "object",
                "properties": {
                    "schematic": { "type": "string" },
                    "reference": { "type": "string" },
                    "pin_number": { "type": "string" }
                },
                "required": ["schematic", "reference", "pin_number"] }),
            |args, ctx| async move { handle_get_pin_connections(args, ctx).await }
        ),
        tool!(
            "get_pin_net_name",
            "Return the primary net name at a specific pin using saved-coordinate label \
             attachment. Global power takes precedence over local power.",
            json!({ "type": "object",
                "properties": {
                    "schematic": { "type": "string" },
                    "reference": { "type": "string" },
                    "pin_number": { "type": "string" }
                },
                "required": ["schematic", "reference", "pin_number"] }),
            |args, ctx| async move { handle_get_pin_connections(args, ctx).await }
        ),
        tool!(
            "get_component_nets",
            "Get the primary net name for each pin using saved-coordinate label attachment. \
             Global power takes precedence over local power.",
            json!({ "type": "object",
                "properties": {
                    "schematic": { "type": "string" },
                    "reference": { "type": "string" }
                },
                "required": ["schematic", "reference"] }),
            |args, ctx| async move { handle_get_component_nets(args, ctx).await }
        ),
        tool!(
            "get_net_components",
            "Get all components and pins on a named net, using saved-coordinate label attachment.",
            json!({ "type": "object",
                "properties": {
                    "schematic": { "type": "string" },
                    "net": { "type": "string" }
                },
                "required": ["schematic", "net"] }),
            |args, ctx| async move { handle_get_net_components(args, ctx).await }
        ),
        tool!(
            "trace_from_point",
            "Return nearby wires, labels, component pins and junction dots, and the primary net name at \
             the saved coordinate. An item in labels_here need not belong to that net. \
             Hierarchical sheet pins and no-connect flags are not reported, so an empty \
             pins_here does not prove a wire dangles.",
            json!({ "type": "object",
                "properties": {
                    "schematic": { "type": "string" },
                    "x": { "type": "number" }, "y": { "type": "number" },
                    "tolerance": {
                        "type": "number", "exclusiveMinimum": 0, "default": 0.05
                    }
                },
                "required": ["schematic", "x", "y"] }),
            |args, ctx| async move { handle_trace_from_point(args, ctx).await }
        ),
        tool!(
            "find_orphan_items",
            "Find dangling wire ends, floating labels, and unconnected pin endpoints. \
             Pins, sheet pins, junctions, and no-connect flags all count as connections.",
            json!({ "type": "object",
                "properties": {
                    "schematic": { "type": "string" },
                    "tolerance": {
                        "type": "number", "exclusiveMinimum": 0, "default": 0.05
                    }
                },
                "required": ["schematic"] }),
            |args, ctx| async move { handle_find_orphan_items(args, ctx).await }
        ),
        tool!(
            "find_shorted_nets",
            "Detect accidentally merged nets — distinct net names that KiCad nets together, \
             through a wire path they share or through a name that joins their segments.",
            json!({ "type": "object",
                "properties": { "schematic": { "type": "string" } },
                "required": ["schematic"] }),
            |args, ctx| async move { handle_find_shorted_nets(args, ctx).await }
        ),
        tool!(
            "find_single_pin_nets",
            "Find nets that reach at most one pin — often a missing counterpart, an orphan \
             label, or a stub left by a deleted component. Component pins and hierarchical \
             sheet pins count; a power symbol's own pin names the rail rather than consuming \
             it and does not. Reports the pin and label counts, and every label kind that \
             named the net. A net named by a global label or power symbol is checked across \
             the saved hierarchy of the project that owns the sheet, counting each pin once \
             per sheet instance as KiCad's netlist does, so a sheet placed twice gives its \
             pin twice: with a second pin it is left out and named in cross_sheet_resolved. \
             A net that only a hierarchical label carries off this sheet, or any net when \
             the owning project or its hierarchy cannot be established, is flagged \
             cross_sheet_unverified.",
            json!({ "type": "object",
                "properties": { "schematic": { "type": "string" } },
                "required": ["schematic"] }),
            |args, ctx| async move { handle_find_single_pin_nets(args, ctx).await }
        ),
        tool!(
            "get_connected_items",
            "Get wires, attached labels, and other components on the given component's nets. \
             Reports primary names separately from attached labels. A nearby label that KiCad \
             leaves detached is not attached by proximity.",
            json!({
                "type": "object",
                "properties": {
                    "schematic": { "type": "string", "description": "Path to .kicad_sch file" },
                    "reference": { "type": "string", "description": "Component reference designator (e.g. 'R1')" }
                },
                "required": ["schematic", "reference"]
            }),
            |args, ctx| async move { handle_get_connected_items(args, ctx).await }
        ),
    ]
}

// ─── Handlers ─────────────────────────────────────────────────────────────────

async fn handle_list_wires(
    args: &serde_json::Value,
    _ctx: &ToolContext,
) -> anyhow::Result<CallToolResult> {
    let sch_path = get_path(args, "schematic")?;
    let sch = cse::Schematic::load(&sch_path)?;
    let items: Vec<serde_json::Value> = sch.wires.iter()
        .map(|w| json!({ "x1": w.start.0, "y1": w.start.1, "x2": w.end.0, "y2": w.end.1, "uuid": w.uuid }))
        .collect();
    Ok(CallToolResult::json(
        &json!({ "count": items.len(), "wires": items }),
    ))
}

async fn handle_list_nets(
    args: &serde_json::Value,
    _ctx: &ToolContext,
) -> anyhow::Result<CallToolResult> {
    let sch_path = get_path(args, "schematic")?;
    let (_, tree) = read_schematic(&sch_path)?;
    // Power symbols name nets too — the tool has always said so.
    let mut nets: Vec<String> = extract_all_net_labels(&tree)
        .into_iter()
        .map(|l| l.net)
        .collect();
    nets.sort();
    nets.dedup();
    Ok(CallToolResult::json(
        &json!({ "count": nets.len(), "nets": nets }),
    ))
}

/// A label's entry in a net query's answer: the tool's own `fields`, and the
/// label's `uuid`, the one `list_schematic_labels` lists and `batch_delete`
/// takes. A power symbol's pseudo-label carries the symbol's UUID, and deleting
/// by that removes the symbol, so its entry gets none.
fn label_entry(label: &Label, mut fields: serde_json::Value) -> serde_json::Value {
    if label.kind != LabelKind::PowerSymbol {
        if let Some(uuid) = &label.uuid {
            fields["uuid"] = json!(uuid);
        }
    }
    fields
}

async fn handle_list_labels(
    args: &serde_json::Value,
    _ctx: &ToolContext,
) -> anyhow::Result<CallToolResult> {
    let sch_path = get_path(args, "schematic")?;
    let sch = cse::Schematic::load(&sch_path)?;
    let entry = |kind: &str, text: &str, at: &cse::At, uuid: &str| json!({ "net": text, "type": kind, "x": at.x, "y": at.y, "rotation": at.rotation.unwrap_or(0.0), "uuid": uuid });
    let mut items: Vec<serde_json::Value> = Vec::new();
    for l in sch.labels.iter() {
        items.push(entry("NetLabel", &l.text, &l.at, &l.uuid));
    }
    for g in sch.global_labels.iter() {
        items.push(entry("GlobalLabel", &g.text, &g.at, &g.uuid));
    }
    for h in sch.hierarchical_labels.iter() {
        items.push(entry("HierarchicalLabel", &h.text, &h.at, &h.uuid));
    }
    Ok(CallToolResult::json(
        &json!({ "count": items.len(), "labels": items }),
    ))
}

/// Refuse a net name that no label or power symbol on this sheet spells.
///
/// The tools that take a net by name find it through its labels, and a name
/// matching none came back as a net with nothing on it: a success no caller
/// could tell from a real net with no pins. KiCad's netlist puts a local
/// label's sheet path before its name (`/SDA`) and names a net with no label
/// after one of its pins (`Net-(R1-Pad2)`), so a name copied from it misses.
/// The refusal offers the name here that differs only by that path or by
/// case.
fn unknown_net(net: &str, labels: &[Label]) -> Option<CallToolResult> {
    if labels.iter().any(|label| label.net == net) {
        return None;
    }
    let bare = net.rsplit('/').next().unwrap_or(net);
    let near: BTreeSet<&str> = labels
        .iter()
        .map(|label| label.net.as_str())
        .filter(|name| name.eq_ignore_ascii_case(net) || name.eq_ignore_ascii_case(bare))
        .collect();
    let mut reason = format!("no label or power symbol on this sheet names net '{net}'");
    if !near.is_empty() {
        let near: Vec<String> = near.iter().map(|name| format!("'{name}'")).collect();
        reason.push_str(&format!(" (did you mean {}?)", near.join(" or ")));
    }
    reason.push_str(
        ". list_schematic_nets lists the names on this sheet; a net with no label or \
         power symbol has none, and get_connected_items or trace_from_point reach it",
    );
    Some(invalid_arg("net", &reason))
}

async fn handle_get_net_connections(
    args: &serde_json::Value,
    _ctx: &ToolContext,
) -> anyhow::Result<CallToolResult> {
    let sch_path = get_path(args, "schematic")?;
    let net = match require_str(args, "net") {
        Ok(v) => v.to_string(),
        Err(e) => return Ok(e),
    };
    let (_, tree) = read_schematic(&sch_path)?;
    // Every placed symbol, or a refusal: a symbol whose library entry is
    // missing contributes no pins, and the list would come back short.
    let Some(placed) = resolved_placed_pins_by_reference(&tree) else {
        return Ok(CallToolResult::error(format!(
            "Cannot list the pins on net '{net}': {UNRESOLVED_PIN_GEOMETRY}"
        )));
    };
    let wires = extract_wires(&tree);
    let labels = saved_net_labels(&tree);
    if let Some(refusal) = unknown_net(&net, &labels) {
        return Ok(refusal);
    }
    let mut g = net_graph_for(&tree, &wires, &labels);
    // The net is its roots, not its name: one net can carry several names, and
    // its labels are every label on those roots, whichever name each spells.
    let net_roots = g.roots_named(&net);
    let by_root = label_roots(&mut g, &labels);
    let matching: Vec<_> = by_root
        .iter()
        .filter(|(root, _)| net_roots.contains(root))
        .map(|(_, l)| {
            label_entry(
                l,
                json!({ "net": l.net, "type": format!("{:?}", l.kind), "x": l.x, "y": l.y }),
            )
        })
        .collect();
    let connected_points = g.count_points_on_roots(&net_roots);
    // Power symbols and PWR_FLAGs name a net but are not nodes on it; KiCad's
    // netlist leaves them out.
    let pins: Vec<serde_json::Value> = placed
        .iter()
        .filter(|(instance, _)| !is_power_symbol_reference(&instance.reference))
        .flat_map(|(instance, pins)| {
            pins.iter().map(move |(pin, transform)| (&instance.reference, pin, transform))
        })
        .filter_map(|(reference, pin, transform)| {
            let (x, y) = g.pin_position(pin, *transform);
            net_roots.contains(&g.root_at(x, y)).then(|| {
                let (px, py) = copperpour_sexp::schematic::pin_endpoint(pin, *transform);
                json!({ "reference": reference, "pin": pin.number, "name": pin.name, "x": px, "y": py })
            })
        })
        .collect();
    Ok(CallToolResult::json(
        &json!({ "net": net, "label_count": matching.len(), "labels": matching, "pins": pins, "connected_points": connected_points }),
    ))
}

async fn handle_get_net_connectivity(
    args: &serde_json::Value,
    _ctx: &ToolContext,
) -> anyhow::Result<CallToolResult> {
    let sch_path = get_path(args, "schematic")?;
    let net = match require_str(args, "net") {
        Ok(v) => v.to_string(),
        Err(e) => return Ok(e),
    };
    let (_, tree) = read_schematic(&sch_path)?;
    let wires = extract_wires(&tree);
    let labels = saved_net_labels(&tree);
    if let Some(refusal) = unknown_net(&net, &labels) {
        return Ok(refusal);
    }
    let mut g = net_graph_for(&tree, &wires, &labels);
    let net_roots = g.roots_named(&net);
    let net_wire_objs: Vec<_> = wires
        .iter()
        .filter(|w| net_roots.contains(&g.root_at(w.x1, w.y1)))
        .cloned()
        .collect();
    let net_wires: Vec<_> = net_wire_objs
        .iter()
        .map(|w| json!({ "x1": w.x1, "y1": w.y1, "x2": w.x2, "y2": w.y2, "uuid": w.uuid }))
        .collect();
    let net_labels: Vec<_> = labels
        .iter()
        .filter(|l| l.net == net)
        .map(|l| {
            label_entry(
                l,
                json!({ "type": format!("{:?}", l.kind), "x": l.x, "y": l.y }),
            )
        })
        .collect();
    let t_junctions = copperpour_sexp::schematic::find_t_junctions(&net_wire_objs, 0.01);
    Ok(CallToolResult::json(&json!({
        "net": net,
        "wires": net_wires,
        "labels": net_labels,
        "t_junctions": t_junctions.iter().map(|(x,y)| json!({"x": x, "y": y})).collect::<Vec<_>>()
    })))
}

async fn handle_get_pin_connections(
    args: &serde_json::Value,
    _ctx: &ToolContext,
) -> anyhow::Result<CallToolResult> {
    let sch_path = get_path(args, "schematic")?;
    let reference = match require_str(args, "reference") {
        Ok(v) => v.to_string(),
        Err(e) => return Ok(e),
    };
    let pin_number = match require_str(args, "pin_number") {
        Ok(v) => v.to_string(),
        Err(e) => return Ok(e),
    };
    let (_, tree) = read_schematic(&sch_path)?;
    let pin = match PlacedPins::new(&tree).resolve(&reference, &pin_number) {
        Ok(pin) => pin,
        Err(refusal) => {
            return Ok(refusal.result(
                "inspect",
                "Give placed symbols unique references, or inspect the intended candidate by position with trace_from_point.",
            ))
        }
    };
    let (px, py) = pin.endpoint();
    let wires = extract_wires(&tree);
    let labels = saved_net_labels(&tree);
    let mut g = net_graph_for(&tree, &wires, &labels);
    let pin_nets = PinNetNames::build(&tree, &mut g, UnitNotation::for_schematic(&sch_path));
    let (x, y) = g.pin_position(&pin.pin, pin.t);
    let net = pin_nets.net_of_pin(&mut g, &pin.pin, x, y);
    Ok(CallToolResult::json(
        &json!({ "reference": reference, "pin": pin_number, "pin_x": px, "pin_y": py, "net": net }),
    ))
}

async fn handle_get_component_nets(
    args: &serde_json::Value,
    _ctx: &ToolContext,
) -> anyhow::Result<CallToolResult> {
    let sch_path = get_path(args, "schematic")?;
    let reference = match require_str(args, "reference") {
        Ok(v) => v.to_string(),
        Err(e) => return Ok(e),
    };
    let (_, tree) = read_schematic(&sch_path)?;
    let instances = extract_symbol_instances(&tree);
    let wires = extract_wires(&tree);
    let labels = saved_net_labels(&tree);
    if !instances
        .iter()
        .any(|instance| instance.reference == reference)
    {
        return Err(anyhow::anyhow!(component_not_found(&sch_path, &reference)));
    }
    let mut g = net_graph_for(&tree, &wires, &labels);
    let pin_nets = PinNetNames::build(&tree, &mut g, UnitNotation::for_schematic(&sch_path));
    let pins: Vec<serde_json::Value> = placed_pins_by_reference(&tree)
        .into_iter()
        .filter(|(instance, _)| instance.reference == reference)
        .flat_map(|(_, pins)| pins)
        .map(|(pin, transform)| {
            let (px, py) = copperpour_sexp::schematic::pin_endpoint(&pin, transform);
            let (x, y) = g.pin_position(&pin, transform);
            let net = pin_nets.net_of_pin(&mut g, &pin, x, y);
            json!({ "pin": pin.number, "name": pin.name, "x": px, "y": py, "net": net })
        })
        .collect();
    Ok(CallToolResult::json(
        &json!({ "reference": reference, "pins": pins }),
    ))
}

async fn handle_get_net_components(
    args: &serde_json::Value,
    _ctx: &ToolContext,
) -> anyhow::Result<CallToolResult> {
    let sch_path = get_path(args, "schematic")?;
    let net = match require_str(args, "net") {
        Ok(v) => v.to_string(),
        Err(e) => return Ok(e),
    };
    let (_, tree) = read_schematic(&sch_path)?;
    let wires = extract_wires(&tree);
    let labels = saved_net_labels(&tree);
    if let Some(refusal) = unknown_net(&net, &labels) {
        return Ok(refusal);
    }
    let mut g = net_graph_for(&tree, &wires, &labels);
    let net_roots = g.roots_named(&net);
    let result: Vec<serde_json::Value> = placed_pins_by_reference(&tree)
        .into_iter()
        .filter_map(|(instance, pins)| {
            let connected: Vec<_> = pins
                .into_iter()
                .filter_map(|(pin, transform)| {
                    let (px, py) = g.pin_position(&pin, transform);
                    if net_roots.contains(&g.root_at(px, py)) {
                        Some(json!({ "pin": pin.number, "name": pin.name }))
                    } else {
                        None
                    }
                })
                .collect();
            if connected.is_empty() {
                None
            } else {
                Some(json!({ "reference": instance.reference, "value": instance.value, "pins": connected }))
            }
        })
        .collect();
    Ok(CallToolResult::json(
        &json!({ "net": net, "components": result }),
    ))
}

async fn handle_trace_from_point(
    args: &serde_json::Value,
    _ctx: &ToolContext,
) -> anyhow::Result<CallToolResult> {
    let sch_path = get_path(args, "schematic")?;
    let x = match require_f64(args, "x") {
        Ok(v) => v,
        Err(e) => return Ok(e),
    };
    let y = match require_f64(args, "y") {
        Ok(v) => v,
        Err(e) => return Ok(e),
    };
    // A zero or negative tolerance makes every coincidence test below answer
    // "nothing here" while the net graph still names a net at its saved
    // coordinate — a successful response asserting a net at a
    // point it also reports as bare. Refused rather than answered, as
    // `find_orphan_items` refuses it.
    let tol = match opt_positive_f64(args, "tolerance") {
        Ok(value) => value.unwrap_or(0.05),
        Err(e) => return Ok(e),
    };
    let (_, tree) = read_schematic(&sch_path)?;
    let wires = extract_wires(&tree);
    let buses = extract_buses(&tree);
    // Use raw marks for the proximity report and saved pin tips for net membership.
    let labels = extract_all_net_labels(&tree);
    let mut g = net_graph_for(&tree, &wires, &saved_net_labels(&tree));
    let on_wire: Vec<_> = wires
        .iter()
        .filter(|w| {
            points_coincident(x, y, w.x1, w.y1, tol)
                || points_coincident(x, y, w.x2, w.y2, tol)
                || point_on_segment(x, y, w.x1, w.y1, w.x2, w.y2, tol)
        })
        .map(|w| json!({ "x1": w.x1, "y1": w.y1, "x2": w.x2, "y2": w.y2 }))
        .collect();
    let at_label: Vec<_> = labels
        .iter()
        .filter(|l| points_coincident(x, y, l.x, l.y, tol))
        .map(|l| label_entry(l, json!({ "net": l.net, "type": format!("{:?}", l.kind) })))
        .collect();
    // A pin and a junction dot sit on a point as readily as a wire or a label,
    // and this is the tool whose whole promise is what is there (#539). Both
    // come from `ConnectivityIndex`, which owns what attaches at a point: a
    // second scan here is exactly how the connectivity tools drifted apart
    // before, and the index is also unit-aware, so a multi-unit symbol
    // contributes only the unit actually placed (#35).
    let index = ConnectivityIndex::build(&tree, &wires, &buses, &labels, tol);
    let detail = index.point_detail(x, y);
    let at_pin: Vec<_> = detail
        .pins
        .iter()
        .map(|placed| {
            json!({
                "reference": placed.reference,
                "pin": placed.pin.number,
                "pin_name": placed.pin.name,
                "electrical_type": placed.pin.electrical_type,
                "x": placed.at.0,
                "y": placed.at.1
            })
        })
        .collect();
    let at_junction: Vec<_> = detail
        .junctions
        .iter()
        .map(|&(jx, jy)| json!({ "x": jx, "y": jy }))
        .collect();
    Ok(CallToolResult::json(&json!({
        "x": x,
        "y": y,
        "net": g.net_at(x, y),
        "wires_here": on_wire,
        "labels_here": at_label,
        "pins_here": at_pin,
        "junctions_here": at_junction
    })))
}

pub(super) const ORPHAN_TOLERANCE: f64 = 0.05;

async fn handle_find_orphan_items(
    args: &serde_json::Value,
    _ctx: &ToolContext,
) -> anyhow::Result<CallToolResult> {
    let sch_path = get_path(args, "schematic")?;
    let tolerance = opt_f64(args, "tolerance").unwrap_or(ORPHAN_TOLERANCE);
    if !tolerance.is_finite() || tolerance <= 0.0 {
        return Ok(CallToolResult::error(
            "Invalid argument 'tolerance': must be finite and positive",
        ));
    }

    let (_, tree) = read_schematic(&sch_path)?;
    let wires = extract_wires(&tree);
    let buses = extract_buses(&tree);
    // Every net name, so the index is the same one every other tool builds. A
    // power symbol's pseudo-label sits on the pin already indexed, so feeding
    // them changes no coincidence answer below.
    let labels = extract_all_net_labels(&tree);
    let index = ConnectivityIndex::build(&tree, &wires, &buses, &labels, tolerance);
    let all = orphan_findings(&index);
    Ok(CallToolResult::json(&json!({
        "orphan_count": all.len(),
        "orphans": all,
        "tolerance": tolerance
    })))
}

pub(super) fn orphan_findings(index: &ConnectivityIndex<'_>) -> Vec<serde_json::Value> {
    let mut all: Vec<serde_json::Value> = Vec::new();

    // A wire end is dangling only when nothing terminates it. Ending on a
    // component or hierarchical sheet pin is the normal case.
    for (x, y, wire_uuid) in index.floating_wire_ends() {
        all.push(json!({
            "type": "dangling_wire_end",
            "x": x,
            "y": y,
            "wire_uuid": wire_uuid
        }));
    }

    // Labels connect anywhere along a wire, not only at its endpoint, or
    // directly on a bare symbol pin. Only real labels are reported: a power
    // symbol that connects to nothing is an unconnected pin, below, and
    // reporting it twice would be a second answer to one question.
    for label in index
        .labels()
        .iter()
        .filter(|label| label.kind != LabelKind::PowerSymbol)
    {
        if !index.on_wire(label.x, label.y)
            && !index.on_bus(label.x, label.y)
            && !index.has_pin(label.x, label.y)
        {
            all.push(json!({
                "type": "floating_label",
                "net": label.net,
                "x": label.x,
                "y": label.y
            }));
        }
    }

    // Report the unconnected pins promised by the tool description. A pin
    // sitting mid-wire connects only through a junction dot (#104).
    for placed in index.placed_pins() {
        let (x, y) = placed.at;
        if placed.pin.electrical_type == "no_connect" || index.has_no_connect(x, y) {
            continue;
        }
        if !index.attaches_pin(x, y) {
            all.push(json!({
                "type": "unconnected_pin",
                "reference": placed.reference,
                "pin": placed.pin.number,
                "pin_name": placed.pin.name,
                "x": x,
                "y": y
            }));
        }
    }

    all
}

async fn handle_find_shorted_nets(
    args: &serde_json::Value,
    _ctx: &ToolContext,
) -> anyhow::Result<CallToolResult> {
    let sch_path = get_path(args, "schematic")?;
    let (_, tree) = read_schematic(&sch_path)?;
    let wires = extract_wires(&tree);
    let labels = saved_net_labels(&tree);
    let mut g = net_graph_for(&tree, &wires, &labels);
    let mut root_nets: HashMap<(i64, i64), Vec<&str>> = HashMap::new();
    for (root, label) in label_roots(&mut g, &labels) {
        root_nets.entry(root).or_default().push(label.net.as_str());
    }
    let shorts: Vec<serde_json::Value> = root_nets
        .into_values()
        .filter_map(|mut nets| {
            nets.sort();
            nets.dedup();
            if nets.len() > 1 {
                Some(json!({ "shorted_nets": nets }))
            } else {
                None
            }
        })
        .collect();
    Ok(CallToolResult::json(
        &json!({ "short_count": shorts.len(), "shorts": shorts }),
    ))
}

/// A point that counts as reaching a net.
enum Connection<'a> {
    ComponentPin {
        reference: &'a str,
        pin: &'a LibPin,
        x: f64,
        y: f64,
    },
    /// A hierarchical sheet pin: whatever the net meets on the other side.
    SheetPin { x: f64, y: f64 },
}

impl Connection<'_> {
    fn to_json(&self) -> serde_json::Value {
        match self {
            Connection::ComponentPin {
                reference,
                pin,
                x,
                y,
            } => json!({
                "type": "component_pin",
                "reference": reference,
                "pin": pin.number,
                "pin_name": pin.name,
                "x": x,
                "y": y
            }),
            Connection::SheetPin { x, y } => json!({ "type": "sheet_pin", "x": x, "y": y }),
        }
    }
}

/// Whether a label of this kind can carry its net off the sheet being read.
///
/// A local `NetLabel` cannot: it names a net within one sheet, so a count taken
/// from that sheet is the whole answer. The other three can, and a reader has
/// to know which it is holding. `GlobalLabel` and a power symbol — which KiCAD
/// makes a global label out of — join every same-named net in the project;
/// `HierarchicalLabel` continues through the parent's sheet pin.
fn reaches_beyond_this_sheet(kind: &LabelKind) -> bool {
    match kind {
        LabelKind::NetLabel => false,
        LabelKind::GlobalLabel | LabelKind::HierarchicalLabel | LabelKind::PowerSymbol => true,
    }
}

/// Component pins on each net a global label or power symbol names, for one
/// sheet. Those names join nets across the whole project, so these counts are
/// what another sheet adds to a net `find_single_pin_nets` sees on its own.
fn global_net_pin_counts(tree: &copperpour_sexp::SexpNode) -> HashMap<String, usize> {
    let wires = extract_wires(tree);
    let labels = saved_net_labels(tree);
    let mut graph = net_graph_for(tree, &wires, &labels);
    let mut pins_by_root: HashMap<(i64, i64), usize> = HashMap::new();
    for (instance, pins) in placed_pins_by_reference(tree) {
        if is_power_symbol_reference(&instance.reference) {
            continue;
        }
        for (pin, transform) in pins {
            let (x, y) = graph.pin_position(&pin, transform);
            *pins_by_root.entry(graph.root_at(x, y)).or_default() += 1;
        }
    }
    let mut roots_by_net: HashMap<String, HashSet<(i64, i64)>> = HashMap::new();
    for (root, label) in label_roots(&mut graph, &labels) {
        if matches!(label.kind, LabelKind::GlobalLabel | LabelKind::PowerSymbol) {
            roots_by_net
                .entry(label.net.clone())
                .or_default()
                .insert(root);
        }
    }
    roots_by_net
        .into_iter()
        .map(|(net, roots)| {
            let pins = roots.iter().filter_map(|root| pins_by_root.get(root)).sum();
            (net, pins)
        })
        .collect()
}

/// What the rest of a project adds to the nets global labels and power
/// symbols name, counted the way KiCad's netlist counts them: once per sheet
/// instance, not once per sheet file.
struct ProjectPins {
    /// Distinct sheet files read across every top-level sheet's hierarchy.
    sheet_files: usize,
    /// How often the saved hierarchy places the sheet the tool was given.
    own_instances: usize,
    /// Per net name, the component pins on every other sheet file, each
    /// multiplied by the number of times that file is placed.
    elsewhere: HashMap<String, usize>,
}

/// [`ProjectPins`] for the project that owns `sch_path`.
///
/// The project is the one [`crate::tools::resolve_schematic_ownership`]
/// proves; every top-level sheet it lists is walked, because eeschema joins
/// their nets as one design. `None` when there is no owning project, when
/// ownership is ambiguous or unproven, when a hierarchy cannot be walked, or
/// when no walk places `sch_path`: then nothing is resolved across sheets and
/// the per-sheet answer stands, flagged as before.
fn project_pins(sch_path: &std::path::Path) -> Option<ProjectPins> {
    let ownership = crate::tools::resolve_schematic_ownership(sch_path).ok()??;
    let own = std::fs::canonicalize(sch_path).ok()?;
    let mut sheet_files: HashSet<std::path::PathBuf> = HashSet::new();
    let mut own_instances = 0;
    let mut elsewhere: HashMap<String, usize> = HashMap::new();
    for top in &ownership.top_level_sheets {
        let hierarchy = crate::tools::sch_hierarchy::load_saved_hierarchy(&top.schematic).ok()?;
        let mut placements = vec![0usize; hierarchy.files.len()];
        for instance in &hierarchy.instances {
            placements[instance.file] += 1;
        }
        for (file, placed) in hierarchy.files.iter().zip(placements) {
            sheet_files.insert(file.canonical.clone());
            if file.canonical == own {
                own_instances += placed;
            } else if placed > 0 {
                for (net, pins) in global_net_pin_counts(&file.tree) {
                    *elsewhere.entry(net).or_default() += pins * placed;
                }
            }
        }
    }
    (own_instances > 0).then_some(ProjectPins {
        sheet_files: sheet_files.len(),
        own_instances,
        elsewhere,
    })
}

/// Nets that reach at most one pin.
///
/// Zero counts as well as one: a label whose net reaches nothing is the orphan
/// label or the stub left by a deleted component that the review skill sends
/// this tool looking for, and `find_orphan_items` reports the dangling wire end
/// without ever naming the net. `pin_count` tells the two apart.
///
/// This counted *label instances* per net name, so every net drawn the ordinary
/// way — one label on a wire that reaches two or more pins — was reported, and
/// the defect the tool advertises passed through unreported as soon as its net
/// carried a second label. The count comes from the shared net graph now, and
/// the label count stays beside it as its own field: a net with no label at all
/// is a different smell.
///
/// A power symbol's own pin is not a connection here, though it is one to
/// `find_orphan_items`, which reports an unwired `#PWR01`. The divergence is
/// deliberate: the rail that reaches exactly one component pin is what this
/// tool is for, and counting the symbol that named it would hide every one of
/// them.
///
/// The answer is per sheet, so a net named by a global or hierarchical label
/// may well continue on another one, and the report has to say so rather than
/// leave the caller to infer it. `cross_sheet_unverified` is that statement:
/// true when any label naming the net can carry it off this sheet, which is as
/// far as a single-sheet reader can go. Reporting the net anyway is deliberate
/// — a rail that reaches one pin here is worth showing — but the flag marks it
/// as a lead rather than a finding.
///
/// # Response compatibility
///
/// Every field this tool used to return is still returned and still means the
/// same thing: `single_pin_net_count`, and per net `net`, `x`, `y`, and `type`.
/// `type` remains the kind of the *first* label found, so a consumer matching
/// on it is unaffected. It is also why the other two fields exist: local labels
/// are extracted first, so a net carrying both a local and a global label
/// reports `NetLabel` and says nothing about the global one. `label_types`
/// carries every distinct kind, sorted; `cross_sheet_unverified` is the same
/// evidence as one boolean.
///
/// The additive fields are `label_types`, `cross_sheet_unverified`,
/// `pin_count`, `label_count`, and `pins`.
///
/// What *does* change is which nets appear, because that was the defect: the
/// membership rule went from "exactly one label instance names it" to "the net
/// reaches at most one pin". A consumer reading `single_pin_net_count` as a
/// defect count gets a smaller, truer number and needs no migration; one that
/// had learned to ignore this tool's noise can stop. No consumer can keep the
/// old set, since it was wrong in both directions.
async fn handle_find_single_pin_nets(
    args: &serde_json::Value,
    _ctx: &ToolContext,
) -> anyhow::Result<CallToolResult> {
    let sch_path = get_path(args, "schematic")?;
    let (_, tree) = read_schematic(&sch_path)?;
    let wires = extract_wires(&tree);
    let labels = saved_net_labels(&tree);
    let mut graph = net_graph_for(&tree, &wires, &labels);

    // Every connection point, under the graph root that carries it. A sheet
    // holds hundreds and reports a handful, so nothing is rendered until a net
    // qualifies.
    let placed = placed_pins_by_reference(&tree);
    let mut pins_by_root: HashMap<(i64, i64), Vec<Connection>> = HashMap::new();
    for (instance, pins) in &placed {
        if is_power_symbol_reference(&instance.reference) {
            continue;
        }
        for (pin, transform) in pins {
            let (px, py) = graph.pin_position(pin, *transform);
            let (x, y) = copperpour_sexp::schematic::pin_endpoint(pin, *transform);
            pins_by_root
                .entry(graph.root_at(px, py))
                .or_default()
                .push(Connection::ComponentPin {
                    reference: &instance.reference,
                    pin,
                    x,
                    y,
                });
        }
    }
    for (x, y) in extract_sheet_pins(&tree) {
        pins_by_root
            .entry(graph.root_at(x, y))
            .or_default()
            .push(Connection::SheetPin { x, y });
    }

    // Each net name, the roots its labels sit on, and the first label to name
    // it. Two labels of one name on segments that never meet are still one net
    // to KiCAD, so the roots pool.
    struct NamedNet<'a> {
        roots: HashSet<(i64, i64)>,
        label_count: usize,
        first_label: &'a Label,
        /// Every kind that named this net, not just the first. Sorted, so the
        /// field reads the same on two runs over one sheet.
        kinds: BTreeSet<String>,
        cross_sheet_unverified: bool,
    }
    let mut by_net: HashMap<&str, NamedNet> = HashMap::new();
    for (root, label) in label_roots(&mut graph, &labels) {
        let named = by_net
            .entry(label.net.as_str())
            .or_insert_with(|| NamedNet {
                roots: HashSet::new(),
                label_count: 0,
                first_label: label,
                kinds: BTreeSet::new(),
                cross_sheet_unverified: false,
            });
        named.roots.insert(root);
        named.label_count += 1;
        named.kinds.insert(format!("{:?}", label.kind));
        named.cross_sheet_unverified |= reaches_beyond_this_sheet(&label.kind);
    }

    // HashMap order is not an answer: two runs of one binary over one sheet
    // would report the same nets in different orders.
    let mut by_net: Vec<(&str, NamedNet)> = by_net.into_iter().collect();
    by_net.sort_by_key(|(net, _)| *net);

    // A net a global label or power symbol names continues on every sheet
    // that names it too. Read the rest of the project once, and only when a
    // net here needs it. On a six-sheet project all 30 per-sheet findings
    // were such leads.
    let joins_globally = |named: &NamedNet| {
        named.kinds.contains("GlobalLabel") || named.kinds.contains("PowerSymbol")
    };
    let mut project: Option<Option<ProjectPins>> = None;
    let mut resolved: Vec<&str> = Vec::new();
    let mut singles: Vec<serde_json::Value> = Vec::new();
    for (net, named) in &by_net {
        let mut on_net = named
            .roots
            .iter()
            .filter_map(|root| pins_by_root.get(root))
            .flatten();
        let reached = on_net.next();
        if on_net.next().is_some() {
            continue;
        }
        let mut cross_sheet_unverified = named.cross_sheet_unverified;
        if joins_globally(named) {
            if let Some(pins) = project.get_or_insert_with(|| project_pins(&sch_path)) {
                // This sheet's pin is in the design once per placement of
                // this sheet, as every other sheet's pins are.
                let here = usize::from(reached.is_some()) * pins.own_instances;
                let elsewhere = pins.elsewhere.get(*net).copied().unwrap_or(0);
                if here + elsewhere >= 2 {
                    resolved.push(net);
                    continue;
                }
                // Checked project-wide; only a hierarchical label can still
                // carry it somewhere this did not look.
                cross_sheet_unverified = named.kinds.contains("HierarchicalLabel");
            }
        }
        let pins: Vec<serde_json::Value> = reached.map(Connection::to_json).into_iter().collect();
        singles.push(json!({
            "net": net,
            "x": named.first_label.x,
            "y": named.first_label.y,
            "type": format!("{:?}", named.first_label.kind),
            "label_types": named.kinds,
            "cross_sheet_unverified": cross_sheet_unverified,
            "pin_count": pins.len(),
            "label_count": named.label_count,
            "pins": pins
        }));
    }

    let mut response = json!({ "single_pin_net_count": singles.len(), "nets": singles });
    if let Some(Some(pins)) = &project {
        response["sheets_checked"] = json!(pins.sheet_files);
        response["cross_sheet_resolved"] = json!(resolved);
    }
    Ok(CallToolResult::json(&response))
}

async fn handle_get_connected_items(
    args: &serde_json::Value,
    _ctx: &ToolContext,
) -> anyhow::Result<CallToolResult> {
    let sch_path = get_path(args, "schematic")?;
    let reference = match require_str(args, "reference") {
        Ok(v) => v.to_string(),
        Err(e) => return Ok(e),
    };

    let (_, tree) = read_schematic(&sch_path)?;
    let instances = extract_symbol_instances(&tree);
    let wires = extract_wires(&tree);
    let labels = saved_net_labels(&tree);
    if !instances
        .iter()
        .any(|instance| instance.reference == reference)
    {
        return Ok(CallToolResult::error(component_not_found(
            &sch_path, &reference,
        )));
    }
    let placed_pins = placed_pins_by_reference(&tree);
    let mut g = net_graph_for(&tree, &wires, &labels);

    // The nets this component's pins are on, held by identity: a net can carry
    // more than one name, and one can carry none at all. Keying this by name
    // dropped the labels spelling a net its other way, and dropped an unnamed
    // net's items entirely.
    let mut connected_roots: HashSet<(i64, i64)> = HashSet::new();
    let mut connected_nets: BTreeSet<String> = BTreeSet::new();
    for (_, pins) in placed_pins
        .iter()
        .filter(|(instance, _)| instance.reference == reference)
    {
        for (pin, transform) in pins {
            let (px, py) = g.pin_position(pin, *transform);
            let root = g.root_at(px, py);
            connected_roots.insert(root);
            if let Some(net) = g.name_of_root(root) {
                connected_nets.insert(net);
            }
        }
    }

    // Find all wires, labels, and components on those nets
    let connected_wires: Vec<serde_json::Value> = wires
        .iter()
        .filter(|w| connected_roots.contains(&g.root_at(w.x1, w.y1)))
        .map(|w| json!({ "x1": w.x1, "y1": w.y1, "x2": w.x2, "y2": w.y2, "uuid": w.uuid }))
        .collect();

    let connected_labels: Vec<serde_json::Value> = label_roots(&mut g, &labels)
        .into_iter()
        .filter(|(root, _)| connected_roots.contains(root))
        .map(|(_, l)| {
            label_entry(
                l,
                json!({ "net": l.net, "type": format!("{:?}", l.kind), "x": l.x, "y": l.y }),
            )
        })
        .collect();

    // Find other components on the same nets (excluding the queried one)
    let connected_components: Vec<serde_json::Value> = placed_pins
        .iter()
        .filter(|(instance, _)| instance.reference != reference)
        .filter_map(|(instance, pins)| {
            let matching_pins: Vec<_> = pins
                .iter()
                .filter_map(|(pin, transform)| {
                    let (px, py) = g.pin_position(pin, *transform);
                    if connected_roots.contains(&g.root_at(px, py)) {
                        Some(json!({ "pin": pin.number, "name": pin.name }))
                    } else {
                        None
                    }
                })
                .collect();
            if matching_pins.is_empty() {
                None
            } else {
                Some(json!({ "reference": instance.reference, "value": instance.value, "connected_pins": matching_pins }))
            }
        })
        .collect();

    Ok(CallToolResult::json(&json!({
        "reference": reference,
        "nets": connected_nets,
        "connected_wires": connected_wires.len(),
        "wires": connected_wires,
        "labels": connected_labels,
        "connected_components": connected_components
    })))
}

pub(super) const OVERLAP_TOLERANCE: f64 = 0.5;

pub(super) async fn handle_check_overlaps(
    args: &serde_json::Value,
    _ctx: &ToolContext,
) -> anyhow::Result<CallToolResult> {
    let sch_path = get_path(args, "schematic")?;
    let tol = opt_f64(args, "tolerance").unwrap_or(OVERLAP_TOLERANCE);
    if !tol.is_finite() || tol < 0.0 {
        return Ok(CallToolResult::error_kind(
            crate::mcp::error::ToolErrorKind::InvalidArgument {
                field: "tolerance".to_string(),
                reason: "must be a finite, non-negative number".to_string(),
            },
            "Argument 'tolerance' must be a finite, non-negative number.",
        ));
    }
    let (_, tree) = read_schematic(&sch_path)?;
    let wires = extract_wires(&tree);
    let (mut all, bounds_resolved, bounds_unresolved) = overlap_findings(&tree, &wires, tol);
    let pin_text = pin_text_findings(&tree, &wires);
    all.extend(pin_text.findings);
    Ok(CallToolResult::json(&json!({
        "overlap_count": all.len(),
        "overlaps": all,
        "bounds_resolved": bounds_resolved,
        "bounds_unresolved": bounds_unresolved,
        "pin_text_unresolved": pin_text.unresolved
    })))
}

pub(super) fn overlap_findings(
    tree: &copperpour_sexp::SexpNode,
    wires: &[Wire],
    tol: f64,
) -> (Vec<serde_json::Value>, usize, Vec<String>) {
    overlap_findings_with_field_clearance(tree, wires, tol, false)
}

pub(super) fn overlap_findings_with_field_clearance(
    tree: &copperpour_sexp::SexpNode,
    wires: &[Wire],
    tol: f64,
    include_power_references: bool,
) -> (Vec<serde_json::Value>, usize, Vec<String>) {
    schematic_overlaps(tree, wires, tol, &[], include_power_references)
}

/// The unordered UUID pairs of overlapping components touched by an edit.
pub(super) fn component_overlap_pairs(
    tree: &copperpour_sexp::SexpNode,
    changed_uuids: &[&str],
) -> BTreeSet<[String; 2]> {
    schematic_overlaps(tree, &[], OVERLAP_TOLERANCE, changed_uuids, false)
        .0
        .into_iter()
        .filter(|finding| finding["type"] == "component_overlap")
        .filter_map(|finding| {
            let ids = &finding["_participants"];
            let mut pair = [ids[0].as_str()?.to_owned(), ids[1].as_str()?.to_owned()];
            pair.sort_unstable();
            Some(pair)
        })
        .collect()
}

/// The pen and text offset ratio the pin text check assumes: KiCad's
/// defaults (`DEFAULT_LINE_WIDTH_MILS`, `m_TextOffsetRatio`), as for the field
/// and label text this check measures; the project file is not read.
const PIN_TEXT_PEN_MM: f64 = 0.1524;
const PIN_TEXT_OFFSET_RATIO: f64 = 0.15;

/// Wires entering a drawn pin name or number (#842), and the symbols whose
/// pin text could not all be measured, with why.
pub(super) struct PinTextFindings {
    pub findings: Vec<serde_json::Value>,
    pub unresolved: Vec<serde_json::Value>,
}

/// `wire_pin_text_overlap` findings: a wire segment whose interior enters
/// the run of a pin name or number as `SCH_PIN::PlotPinTexts` draws it. A
/// wire along a run's edge, or one ending on the pin's tip outside the run,
/// does not count; a wire attached to the pin that runs on into its text
/// does. Read-only, and separate from the clearance reports of the
/// authoring tools.
pub(super) fn pin_text_findings(
    tree: &copperpour_sexp::SexpNode,
    wires: &[Wire],
) -> PinTextFindings {
    use copperpour_sexp::schematic::{lib_pin_nodes_for_body_style, parse_lib_pin};
    use copperpour_sexp::text_bounds::{pin_text_bounds, PinNameChoice, PinText};
    let lib_symbols = tree
        .find("lib_symbols")
        .map(|node| node.find_all("symbol"))
        .unwrap_or_default();
    let mut runs: Vec<(SymbolInstance, PinText, Option<String>)> = Vec::new();
    let mut unresolved = Vec::new();
    for node in tree.find_all("symbol") {
        let Some(instance) = copperpour_sexp::schematic::symbol_instance(node) else {
            continue;
        };
        let mut reasons = BTreeSet::new();
        match find_lib_symbol(&lib_symbols, &instance) {
            None => {
                reasons.insert("unresolved_symbol_definition");
            }
            Some(lib) => {
                let style = super::symbol_body_style(node);
                let placed_pins = node.find_all("pin");
                let placed = |key: &str| -> HashMap<&str, &str> {
                    placed_pins
                        .iter()
                        .filter_map(|pin| {
                            let value = match key {
                                "alternate" => pin.find("alternate")?.get(1)?.as_str()?,
                                _ => pin.find_str(key)?,
                            };
                            Some((pin.get(1)?.as_str()?, value))
                        })
                        .collect()
                };
                let alternates = placed("alternate");
                let pin_uuids = placed("uuid");
                // A saved pin uuid names a graphical pin only when no other
                // drawn pin shares its number.
                let mut drawn = HashMap::<String, usize>::new();
                for pin in lib_pin_nodes_for_body_style(lib, instance.unit, style)
                    .into_iter()
                    .filter_map(parse_lib_pin)
                {
                    *drawn.entry(pin.number).or_default() += 1;
                }
                let choice = move |number: &str| {
                    Some(
                        alternates
                            .get(number)
                            .map_or(PinNameChoice::Base, |name| PinNameChoice::Alternate(name)),
                    )
                };
                for text in pin_text_bounds(
                    lib,
                    &instance,
                    style,
                    PIN_TEXT_PEN_MM,
                    PIN_TEXT_OFFSET_RATIO,
                    choice,
                    true,
                ) {
                    match text {
                        Ok(text) => {
                            let uuid = (drawn.get(&text.number) == Some(&1))
                                .then(|| pin_uuids.get(text.number.as_str()))
                                .flatten()
                                .map(|uuid| uuid.to_string());
                            runs.push((instance.clone(), text, uuid));
                        }
                        Err(reason) => {
                            reasons.insert(reason);
                        }
                    }
                }
            }
        }
        if !reasons.is_empty() {
            unresolved.push(json!({
                "reference": instance.reference,
                "unit": instance.unit,
                "symbol_uuid": instance.uuid,
                "reasons": reasons,
            }));
        }
    }
    let round = copperpour_sexp::geometry::round6;
    let mut findings = Vec::new();
    for wire in wires {
        let (from, to) = ((wire.x1, wire.y1), (wire.x2, wire.y2));
        if from == to {
            continue;
        }
        for (instance, text, pin_uuid) in &runs {
            if text.run.segment_enters(from, to) {
                findings.push(json!({
                    "type": "wire_pin_text_overlap",
                    "a": wire_json(wire),
                    "b": {
                        "kind": "pin_text",
                        "reference": instance.reference,
                        "unit": instance.unit,
                        "symbol_uuid": instance.uuid,
                        "pin_number": text.number,
                        "pin_uuid": pin_uuid,
                        "tip_x": round(text.tip.0),
                        "tip_y": round(text.tip.1),
                        "role": text.role.as_str(),
                        "text": text.text
                    },
                    "bounds_b": {
                        "x_min": round(text.run.min_x),
                        "y_min": round(text.run.min_y),
                        "x_max": round(text.run.max_x),
                        "y_max": round(text.run.max_y)
                    }
                }));
            }
        }
    }
    PinTextFindings {
        findings,
        unresolved,
    }
}

fn schematic_overlaps(
    tree: &copperpour_sexp::SexpNode,
    wires: &[Wire],
    tol: f64,
    changed_uuids: &[&str],
    include_power_references: bool,
) -> (Vec<serde_json::Value>, usize, Vec<String>) {
    let instances = tree.find_all("symbol").into_iter().filter_map(|node| {
        Some((
            copperpour_sexp::schematic::symbol_instance(node)?,
            super::symbol_body_style(node),
        ))
    });
    let lib_symbols = tree
        .find("lib_symbols")
        .map(|node| node.find_all("symbol"))
        .unwrap_or_default();

    // Compare the actual selected-unit drawings and pins. Distinct origins
    // can still place two large symbols on top of each other; the old origin
    // comparison missed that normal collision shape entirely.
    let placements = instances
        .map(|(instance, style)| {
            let library = find_lib_symbol(&lib_symbols, &instance);
            let placed = library.and_then(|symbol| {
                let geometry = symbol_geometry_for_instance(symbol, &instance)?;
                Some((geometry.envelope()?, geometry))
            });
            let unresolved = placed.is_none()
                || !changed_uuids.is_empty()
                    && library.is_some_and(|symbol| {
                        copperpour_sexp::schematic::has_unmeasured_library_text(
                            symbol,
                            instance.unit,
                            style,
                        )
                    });
            (instance, placed, unresolved)
        })
        .collect::<Vec<_>>();
    let mut comp_overlaps: Vec<serde_json::Value> = Vec::new();
    let bounds_json = |bounds: SymbolBounds| {
        json!({
            "x_min": bounds.min_x,
            "y_min": bounds.min_y,
            "x_max": bounds.max_x,
            "y_max": bounds.max_y
        })
    };
    let changed = |uuid: Option<&str>| uuid.is_some_and(|id| changed_uuids.contains(&id));
    let symbol_matches = |instance: &SymbolInstance| changed(instance.uuid.as_deref());
    let text_matches = |text: &TextExtent| match &text.owner {
        TextOwner::Field { symbol_uuid, .. } => changed(symbol_uuid.as_deref()),
        TextOwner::Label { uuid, .. } => changed(uuid.as_deref()),
    };
    let identify = |mut finding: serde_json::Value, a: Option<&str>, b: Option<&str>| {
        if !changed_uuids.is_empty() {
            finding["_participants"] = json!([a, b]);
        }
        finding
    };
    for (index, (a, a_placed, _)) in placements.iter().enumerate() {
        for (b, b_placed, _) in &placements[index + 1..] {
            if !changed_uuids.is_empty() && !symbol_matches(a) && !symbol_matches(b) {
                continue;
            }
            match (a_placed, b_placed) {
                (Some((a_bounds, a_geometry)), Some((b_bounds, b_geometry))) => {
                    // Pin-tip contact is a normal connection, so the drawings
                    // and pins are compared, not their envelopes.
                    if !a_bounds.separated(*b_bounds) && a_geometry.collides(b_geometry) {
                        let (overlap_x, overlap_y) = a_bounds.overlap_depth(*b_bounds);
                        comp_overlaps.push(identify(
                            json!({
                                "type": "component_overlap",
                                "a": a.reference,
                                "unit_a": a.unit,
                                "b": b.reference,
                                "unit_b": b.unit,
                                "overlap_x_mm": overlap_x,
                                "overlap_y_mm": overlap_y,
                                "bounds_a": bounds_json(*a_bounds),
                                "bounds_b": bounds_json(*b_bounds),
                                "detection": "symbol_geometry"
                            }),
                            a.uuid.as_deref(),
                            b.uuid.as_deref(),
                        ));
                    }
                }
                // Preserve a useful conservative check when an old or damaged
                // schematic lacks the embedded definition. The response names
                // every such fallback below so it cannot look authoritative.
                _ if points_coincident(a.x, a.y, b.x, b.y, tol) => {
                    let mut finding = identify(
                        json!({
                            "type": "component_overlap",
                            "a": a.reference,
                            "unit_a": a.unit,
                            "b": b.reference,
                            "unit_b": b.unit,
                            "x": a.x,
                            "y": a.y,
                            "detection": "origin_fallback"
                        }),
                        a.uuid.as_deref(),
                        b.uuid.as_deref(),
                    );
                    if !changed_uuids.is_empty() {
                        finding["_anchors"] = json!([[a.x, a.y], [b.x, b.y]]);
                    }
                    comp_overlaps.push(finding);
                }
                _ => {}
            }
        }
    }

    // The structural extractor already combines local, global, and
    // hierarchical labels.
    let all_labels = extract_labels(tree);
    let mut label_overlaps: Vec<serde_json::Value> = Vec::new();
    for (i, a) in all_labels.iter().enumerate() {
        for b in &all_labels[i + 1..] {
            if !changed_uuids.is_empty()
                && !changed(a.uuid.as_deref())
                && !changed(b.uuid.as_deref())
            {
                continue;
            }
            if points_coincident(a.x, a.y, b.x, b.y, tol) && a.net != b.net {
                let mut finding = identify(
                    json!({
                        "type": "label_overlap", "net_a": a.net, "net_b": b.net, "x": a.x, "y": a.y
                    }),
                    a.uuid.as_deref(),
                    b.uuid.as_deref(),
                );
                if !changed_uuids.is_empty() {
                    finding["_anchors"] = json!([[a.x, a.y], [b.x, b.y]]);
                }
                label_overlaps.push(finding);
            }
        }
    }

    // Label and field text against other text and other symbols' bodies
    // (#111): text at its ink, and outlines and flags at their drawn line as
    // bodies are, so that outlines which only meet do not overlap. A symbol's
    // own fields may sit on its own body: stock Device:R puts its Value inside
    // it. Two labels of different nets at one anchor are already a
    // label_overlap.
    let texts = text_extents(tree);
    let mut text_overlaps: Vec<serde_json::Value> = Vec::new();
    let mut push_text = |a: serde_json::Value, a_box: SymbolBounds, b: serde_json::Value, b_box| {
        let (overlap_x, overlap_y) = a_box.overlap_depth(b_box);
        text_overlaps.push(json!({
            "type": "text_overlap",
            "a": a,
            "b": b,
            "overlap_x_mm": overlap_x,
            "overlap_y_mm": overlap_y,
            "bounds_a": bounds_json(a_box),
            "bounds_b": bounds_json(b_box)
        }));
    };
    for (index, a) in texts.iter().enumerate() {
        for b in &texts[index + 1..] {
            if !changed_uuids.is_empty() && !text_matches(a) && !text_matches(b) {
                continue;
            }
            let already_reported = match (&a.owner, &b.owner) {
                (
                    TextOwner::Label {
                        net: net_a,
                        at: (ax, ay),
                        ..
                    },
                    TextOwner::Label {
                        net: net_b,
                        at: (bx, by),
                        ..
                    },
                ) => net_a != net_b && points_coincident(*ax, *ay, *bx, *by, tol),
                _ => false,
            };
            if !already_reported && a.compared.interiors_intersect(b.compared) {
                push_text(text_json(a), a.bounds, text_json(b), b.bounds);
            }
        }
        for (instance, placed, _) in &placements {
            if !changed_uuids.is_empty() && !text_matches(a) && !symbol_matches(instance) {
                continue;
            }
            let Some(body) = placed.as_ref().and_then(|(_, geometry)| geometry.body) else {
                continue;
            };
            if !is_own_field(a, instance) && a.compared.interiors_intersect(body) {
                let symbol = json!({
                    "kind": "symbol_body",
                    "reference": instance.reference,
                    "unit": instance.unit,
                    "symbol_uuid": instance.uuid
                });
                push_text(text_json(a), a.bounds, symbol, body);
            }
        }
    }

    let wire_texts = texts
        .iter()
        .filter_map(|text| {
            let kind = match &text.owner {
                TextOwner::Label { .. } => "wire_label_overlap",
                TextOwner::Field {
                    field,
                    is_power_symbol,
                    ..
                } => match (field.as_str(), is_power_symbol) {
                    ("Value", true) => "wire_label_overlap",
                    ("Reference" | "Value", false) => "wire_field_overlap",
                    ("Reference", true) if include_power_references => "wire_field_overlap",
                    _ => return None,
                },
            };
            Some((text, kind))
        })
        .collect::<Vec<_>>();

    // A wire drawn through a symbol's body reads as part of the symbol, so
    // what it connects is hidden; it is reported with the body it crosses. A
    // wire along an edge or ending on a pin tip outside the body is contact,
    // and so is one that leaves the symbol's own pin tip outward, though a
    // drawing that reaches past the tip may clip it.
    let mut wire_overlaps: Vec<serde_json::Value> = Vec::new();
    let mut wire_text_overlaps: Vec<serde_json::Value> = Vec::new();
    for wire in wires {
        let (from, to) = ((wire.x1, wire.y1), (wire.x2, wire.y2));
        let wire_matches = changed(wire.uuid.as_deref());
        for (instance, placed, _) in &placements {
            if !changed_uuids.is_empty() && !wire_matches && !symbol_matches(instance) {
                continue;
            }
            let Some((_, geometry)) = placed else {
                continue;
            };
            let Some(body) = geometry.body else {
                continue;
            };
            if body.segment_enters(from, to) && !geometry.wire_leaves_pin(from, to) {
                wire_overlaps.push(json!({
                    "type": "wire_overlap",
                    "a": wire_json(wire),
                    "b": {
                        "kind": "symbol_body",
                        "reference": instance.reference,
                        "unit": instance.unit,
                        "symbol_uuid": instance.uuid
                    },
                    "bounds_b": bounds_json(body)
                }));
            }
        }
        for (text, kind) in &wire_texts {
            if !changed_uuids.is_empty() && !wire_matches && !text_matches(text) {
                continue;
            }
            if from != to && text.wire_crossing_bounds().segment_enters(from, to) {
                wire_text_overlaps.push(json!({
                    "type": kind,
                    "a": wire_json(wire),
                    "b": text_json(text),
                    "bounds_b": bounds_json(text.reported_run())
                }));
            }
        }
    }

    let mut all = comp_overlaps;
    all.extend(label_overlaps);
    all.extend(text_overlaps);
    all.extend(wire_overlaps);
    all.extend(wire_text_overlaps);
    let bounds_unresolved = placements
        .iter()
        .filter(|(_, _, unresolved)| *unresolved)
        .map(|(instance, _, _)| instance.reference.clone())
        .collect::<Vec<_>>();
    let bounds_resolved = placements.len() - bounds_unresolved.len();
    (all, bounds_resolved, bounds_unresolved)
}

/// Clearance findings introduced by the given wires, symbols or labels, in
/// the overlap check's shape. Both snapshots must come from the same revision.
#[derive(Default)]
pub struct ClearanceReport {
    pub findings: Vec<serde_json::Value>,
    pub bounds_unresolved: Vec<String>,
}

pub fn added_clearance_findings(
    before: &copperpour_sexp::SexpNode,
    after: &copperpour_sexp::SexpNode,
    changed_uuids: &[&str],
) -> ClearanceReport {
    if changed_uuids.is_empty() {
        return ClearanceReport::default();
    }
    let changed = |uuid: Option<&str>| uuid.is_some_and(|id| changed_uuids.contains(&id));
    let symbols_changed = [before, after].into_iter().any(|tree| {
        tree.find_all("symbol")
            .iter()
            .any(|symbol| changed(symbol.find_str("uuid")))
    });
    let findings = |tree| {
        let wires = extract_wires(tree);
        let (mut result, _, mut unresolved) =
            schematic_overlaps(tree, &wires, 0.5, changed_uuids, false);
        if symbols_changed {
            let (contacts, unresolved_pins) =
                super::sch_pin_contacts::clearance_pin_contacts(tree, &wires);
            for reference in unresolved_pins {
                if !unresolved.contains(&reference) {
                    unresolved.push(reference);
                }
            }
            for contact in contacts {
                let pins = contact["pins"]
                    .as_array()
                    .expect("pin contact lists its pins");
                if pins.iter().any(|pin| changed(pin["symbol_uuid"].as_str())) {
                    result.push(json!({
                        "type": "pin_contact",
                        "point": contact["point"],
                        "pins": pins,
                    }));
                }
            }
        }
        for (i, a) in wires.iter().enumerate() {
            for b in &wires[i + 1..] {
                if (changed(a.uuid.as_deref()) || changed(b.uuid.as_deref()))
                    && super::sch_layout_score::collinear_overlap(a, b)
                {
                    result.push(json!({
                        "type": "collinear_overlap",
                        "a": wire_json(a),
                        "b": wire_json(b),
                    }));
                }
            }
        }
        (result, unresolved)
    };
    let key = |finding: &serde_json::Value| {
        let mut finding = finding.clone();
        let object = finding.as_object_mut().expect("finding is an object");
        if object.contains_key("_anchors") {
            object.remove("x");
            object.remove("y");
        }
        for side in ["a", "b"] {
            if let Some(wire) = object.get_mut(side).filter(|item| item["kind"] == "wire") {
                let start = (wire["x1"].as_f64(), wire["y1"].as_f64());
                let end = (wire["x2"].as_f64(), wire["y2"].as_f64());
                if start > end {
                    (wire["x1"], wire["y1"]) = (json!(end.0), json!(end.1));
                    (wire["x2"], wire["y2"]) = (json!(start.0), json!(start.1));
                }
            }
        }
        if object["type"] == "pin_contact" {
            // Compare physical pin occurrences, not their sheet positions.
            // A move can preserve a contact even when its center changes.
            object.remove("point");
            if let Some(pins) = object
                .get_mut("pins")
                .and_then(serde_json::Value::as_array_mut)
            {
                pins.sort_by_cached_key(serde_json::Value::to_string);
            }
        }
        let swap = match object["type"].as_str() {
            Some("component_overlap" | "label_overlap") => object
                .get("_participants")
                .and_then(serde_json::Value::as_array)
                .is_some_and(|ids| ids[0].as_str() > ids[1].as_str()),
            Some("text_overlap" | "collinear_overlap") => {
                fn side_key(side: &serde_json::Value) -> ([Option<&str>; 8], Option<u64>) {
                    (
                        [
                            "kind",
                            "uuid",
                            "symbol_uuid",
                            "field",
                            "reference",
                            "label_type",
                            "net",
                            "text",
                        ]
                        .map(|key| side[key].as_str()),
                        side["unit"].as_u64(),
                    )
                }
                let a = &object["a"];
                let b = &object["b"];
                side_key(a)
                    .cmp(&side_key(b))
                    .then_with(|| {
                        let (a, b, keys) = if object["type"] == "collinear_overlap" {
                            (a, b, ["x1", "y1", "x2", "y2"])
                        } else {
                            (
                                &object["bounds_a"],
                                &object["bounds_b"],
                                ["x_min", "y_min", "x_max", "y_max"],
                            )
                        };
                        keys.into_iter()
                            .map(|key| {
                                a[key]
                                    .as_f64()
                                    .unwrap_or_default()
                                    .total_cmp(&b[key].as_f64().unwrap_or_default())
                            })
                            .find(|order| !order.is_eq())
                            .unwrap_or(std::cmp::Ordering::Equal)
                    })
                    .is_gt()
            }
            _ => false,
        };
        if swap {
            for (a, b) in [
                ("a", "b"),
                ("unit_a", "unit_b"),
                ("net_a", "net_b"),
                ("bounds_a", "bounds_b"),
            ] {
                if let (Some(a_value), Some(b_value)) =
                    (object.get(a).cloned(), object.get(b).cloned())
                {
                    object.insert(a.into(), b_value);
                    object.insert(b.into(), a_value);
                }
            }
            for field in ["_participants", "_anchors"] {
                if let Some(pair) = object
                    .get_mut(field)
                    .and_then(serde_json::Value::as_array_mut)
                {
                    pair.swap(0, 1);
                }
            }
        }
        finding.to_string()
    };
    let (previous, _) = findings(before);
    let previous: HashSet<_> = previous.iter().map(key).collect();
    let (mut added, bounds_unresolved) = findings(after);
    added.retain(|finding| !previous.contains(&key(finding)));
    for finding in &mut added {
        let object = finding.as_object_mut().expect("finding is an object");
        object.remove("_participants");
        object.remove("_anchors");
        if let Some(pins) = object
            .get_mut("pins")
            .and_then(serde_json::Value::as_array_mut)
        {
            for pin in pins {
                pin.as_object_mut()
                    .expect("contact pin is an object")
                    .remove("_local_tip");
            }
        }
    }
    ClearanceReport {
        findings: added,
        bounds_unresolved,
    }
}

#[cfg(test)]
mod clearance_tests {
    use super::*;
    use copperpour_sexp::{parse_sexp, SexpNode};

    const SOURCE: &str =
        include_str!("../../tests/fixtures/power_orientation_kicad10/power_orientation.kicad_sch");
    const BODY_CROSSINGS: &str =
        include_str!("../../tests/fixtures/wire_through_symbol_kicad10.kicad_sch");
    const R1_CROSSING_WIRE: &str = "7f228b0f-7e2f-4061-a35f-d496b115f231";
    const Y1_CROSSING_WIRE: &str = "c3143ef8-21cb-4f0b-a9c8-fe7b58052256";

    fn wire_crossing_snapshots() -> (SexpNode, SexpNode) {
        let after = parse_sexp(BODY_CROSSINGS).unwrap();
        let mut before = after.clone();
        let SexpNode::List(items) = &mut before else {
            panic!("expected a schematic list");
        };
        let count = items.len();
        items.retain(|node| {
            node.head() != Some("wire")
                || !matches!(
                    node.find_str("uuid"),
                    Some(R1_CROSSING_WIRE | Y1_CROSSING_WIRE)
                )
        });
        assert_eq!(items.len(), count - 2);
        (before, after)
    }

    #[test]
    fn wire_body_crossings_report_only_named_changes() {
        let (before, after) = wire_crossing_snapshots();
        // Either crossing is new. Only the one whose wire was named belongs
        // in the report; the other cannot be subtracted as a pre-existing finding.
        for (wire, reference) in [(R1_CROSSING_WIRE, "R1"), (Y1_CROSSING_WIRE, "Y1")] {
            let report = added_clearance_findings(&before, &after, &[wire]);
            let crossings: Vec<_> = report
                .findings
                .iter()
                .filter(|finding| finding["type"] == "wire_overlap")
                .collect();
            assert_eq!(crossings.len(), 1, "{:?}", report.findings);
            assert_eq!(crossings[0]["a"]["uuid"], wire);
            assert_eq!(crossings[0]["b"]["reference"], reference);
        }
    }

    #[test]
    fn an_empty_change_set_reports_no_clearance_findings() {
        let (before, after) = wire_crossing_snapshots();
        assert!(
            added_clearance_findings(&before, &after, &[R1_CROSSING_WIRE])
                .findings
                .iter()
                .any(|finding| finding["type"] == "wire_overlap")
        );
        let report = added_clearance_findings(&before, &after, &[]);
        assert!(report.findings.is_empty(), "{:?}", report.findings);
        assert!(report.bounds_unresolved.is_empty());
    }

    #[test]
    fn a_changed_first_label_reports_its_overlap_with_an_unchanged_label() {
        let before = parse_sexp(
            r#"(kicad_sch
                (label "LONG_SIGNAL_B" (at 21.27 20 0)
                    (effects (font (size 1.27 1.27))) (uuid "b")))"#,
        )
        .unwrap();
        let after = parse_sexp(
            r#"(kicad_sch
                (label "LONG_SIGNAL_A" (at 20 20 0)
                    (effects (font (size 1.27 1.27))) (uuid "a"))
                (label "LONG_SIGNAL_B" (at 21.27 20 0)
                    (effects (font (size 1.27 1.27))) (uuid "b")))"#,
        )
        .unwrap();
        let findings = added_clearance_findings(&before, &after, &["a"]).findings;
        assert_eq!(findings.len(), 1, "{findings:?}");
        assert_eq!(findings[0]["type"], "text_overlap");
        assert_eq!(findings[0]["a"]["uuid"], "a");
        assert_eq!(findings[0]["b"]["uuid"], "b");
    }

    #[test]
    fn a_text_body_overlap_is_attributed_to_each_changed_side() {
        let after_source = r#"(kicad_sch
            (lib_symbols (symbol "test:Box"
                (symbol "Box_0_1" (rectangle (start -5 4) (end 5 -4)))))
            (symbol (lib_id "test:Box") (at 24 20 0) (unit 1)
                (uuid "body") (property "Reference" "U1"))
            (label "SIGNAL" (at 20 20 0)
                (effects (font (size 1.27 1.27))) (uuid "label")))"#;
        let after = parse_sexp(after_source).unwrap();
        for (id, previous) in [
            (
                "label",
                after_source.replace("(at 20 20 0)", "(at 20 40 0)"),
            ),
            ("body", after_source.replace("(at 24 20 0)", "(at 24 40 0)")),
        ] {
            let before = parse_sexp(&previous).unwrap();
            let findings = added_clearance_findings(&before, &after, &[id]).findings;
            assert_eq!(findings.len(), 1, "{id}: {findings:?}");
            assert_eq!(findings[0]["type"], "text_overlap");
            assert_eq!(findings[0]["a"]["uuid"], "label");
            assert_eq!(findings[0]["b"]["symbol_uuid"], "body");
        }
    }

    #[test]
    fn only_a_changed_wire_can_introduce_a_collinear_overlap() {
        let source = r#"(kicad_sch
            (wire (pts (xy 10 19) (xy 16 19)) (uuid "a"))
            (wire (pts (xy 12 25) (xy 17 25)) (uuid "b"))
            (label "SIG" (at 50 50 0) (uuid "label")))"#;
        let before = parse_sexp(source).unwrap();
        let after = parse_sexp(
            &source
                .replace("(xy 10 19) (xy 16 19)", "(xy 10 20) (xy 16 20)")
                .replace("(xy 12 25) (xy 17 25)", "(xy 12 20) (xy 17 20)")
                .replace("(at 50 50 0)", "(at 60 50 0)"),
        )
        .unwrap();
        // Both wire positions in the pair can authorize the finding. A label
        // edit must not report the new overlap between unrelated wires.
        for id in ["a", "b"] {
            let findings = added_clearance_findings(&before, &after, &[id]).findings;
            assert_eq!(findings.len(), 1, "{id}: {findings:?}");
            assert_eq!(findings[0]["type"], "collinear_overlap");
            assert_eq!(findings[0]["a"]["uuid"], "a");
            assert_eq!(findings[0]["b"]["uuid"], "b");
        }
        let findings = added_clearance_findings(&before, &after, &["label"]).findings;
        assert!(findings.is_empty(), "{findings:?}");
    }

    #[test]
    fn moving_a_symbol_onto_another_pin_reports_the_contact() {
        let before = parse_sexp(SOURCE).unwrap();
        let mut after = cse::Schematic::from_source("power_orientation.kicad_sch", SOURCE).unwrap();
        let ground = after.symbols.by_reference_mut("#PWR02").unwrap();
        ground.at.x = 68.58;
        ground.at.y = 66.04;
        let uuid = ground.uuid.clone();
        let after = parse_sexp(&after.to_source()).unwrap();
        let findings = added_clearance_findings(&before, &after, &[&uuid]).findings;
        let contacts: Vec<_> = findings
            .iter()
            .filter(|f| f["type"] == "pin_contact")
            .collect();
        assert_eq!(contacts.len(), 1, "{findings:?}");
        assert_eq!(contacts[0]["point"], json!({"x": 68.58, "y": 66.04}));
    }

    #[test]
    fn a_label_edit_does_not_need_unrelated_pin_geometry() {
        let mut sch = cse::Schematic::from_source("power_orientation.kicad_sch", SOURCE).unwrap();
        sch.symbols.by_reference_mut("J3").unwrap().lib_id = "unresolved:Connector".into();
        let before = parse_sexp(&sch.to_source()).unwrap();
        let uuid = sch.add_label("OTHER", 114.3, 121.92).uuid.clone();
        let after = parse_sexp(&sch.to_source()).unwrap();
        let report = added_clearance_findings(&before, &after, &[&uuid]);
        assert_eq!(report.bounds_unresolved, ["J3"]);
        let findings = report.findings;
        assert!(
            findings.iter().any(|f| f["type"] == "label_overlap"),
            "{findings:?}"
        );
    }

    #[test]
    fn wire_field_crossings_report_only_changed_wires_or_symbols() {
        const LANES: &str =
            include_str!("../../tests/fixtures/direct_lanes_kicad10/direct_lanes.kicad_sch");
        let wire = "c9ac38c6-043c-4795-93f9-b053e790821e";
        let symbol = "2d93183c-1dda-4390-8f0b-b713d830dc03";
        let mut sch = cse::Schematic::from_source("direct_lanes.kicad_sch", LANES).unwrap();
        assert!(sch.wires.remove_by_uuid(wire).is_some());
        let before = parse_sexp(&sch.to_source()).unwrap();
        let after = parse_sexp(LANES).unwrap();
        let crossings = |uuid| {
            added_clearance_findings(&before, &after, &[uuid])
                .findings
                .into_iter()
                .filter(|f| f["type"] == "wire_field_overlap")
                .collect::<Vec<_>>()
        };
        for changed in [wire, symbol] {
            let findings = crossings(changed);
            assert_eq!(findings.len(), 2, "{findings:?}");
            assert!(findings
                .iter()
                .all(|f| f["a"]["uuid"] == wire && f["b"]["symbol_uuid"] == symbol));
        }
        assert!(crossings("676cb32a-f015-4eb4-8a3f-5b9364d0e345").is_empty());
    }

    #[test]
    fn library_text_in_the_active_style_is_reported_even_with_a_pin_envelope() {
        let source = r#"(kicad_sch
          (lib_symbols (symbol "test:Text"
            (symbol "Text_0_0"
              (pin passive line (at -5.08 0 0) (length 2.54) (name "~") (number "1")))
            (symbol "Text_1_2" (text "MARK" (at 0 0 0) (effects (font (size 1 1)))))))
          (symbol (lib_id "test:Text") (at 30 30 0) (unit 1) (body_style 1)
            (uuid "u1") (property "Reference" "U1"))
          (symbol (lib_id "test:Text") (at 50 50 0) (unit 1) (body_style 2)
            (uuid "u2") (property "Reference" "U2")))"#;
        let tree = parse_sexp(source).unwrap();
        let report = added_clearance_findings(&tree, &tree, &["u2"]);
        assert_eq!(report.bounds_unresolved, ["U2"]);
        let (_, resolved, unresolved) = overlap_findings(&tree, &[], 0.5);
        assert_eq!(resolved, 2);
        assert!(unresolved.is_empty());
    }

    #[test]
    fn reordering_symbols_with_unknown_bounds_does_not_reintroduce_origin_overlap() {
        let mut sch = cse::Schematic::from_source("power_orientation.kicad_sch", SOURCE).unwrap();
        let j3 = sch.symbols.by_reference_mut("J3").unwrap();
        j3.lib_id = "missing:Symbol".into();
        j3.at.x = 63.6;
        j3.at.y = 124.46;
        let before = parse_sexp(&sch.to_source()).unwrap();
        let j3 = sch.symbols.remove_by_reference("J3").unwrap();
        let uuid = j3.uuid.clone();
        sch.symbols.push(j3);
        let after = parse_sexp(&sch.to_source()).unwrap();
        let report = added_clearance_findings(&before, &after, &[&uuid]);
        assert!(
            !report
                .findings
                .iter()
                .any(|f| f["detection"] == "origin_fallback"),
            "{:?}",
            report.findings
        );
    }

    #[test]
    fn a_second_physical_contact_between_the_same_pin_numbers_is_new() {
        let source = r#"(kicad_sch
          (lib_symbols (symbol "test:Dup"
            (symbol "Dup_0_1"
              (pin passive line (at 0 0 0) (length 0) (name "~") (number "1"))
              (pin passive line (at -2.54 0 0) (length 0) (name "~") (number "1")))))
          (symbol (lib_id "test:Dup") (at 20 20 0) (unit 1)
            (uuid "a") (property "Reference" "J1"))
          (symbol (lib_id "test:Dup") (at 20 20 0) (unit 1) (mirror y)
            (uuid "b") (property "Reference" "J2")))"#;
        let before = parse_sexp(source).unwrap();
        let after = parse_sexp(&source.replace(" (mirror y)", "")).unwrap();
        let report = added_clearance_findings(&before, &after, &["b"]);
        let contacts: Vec<_> = report
            .findings
            .iter()
            .filter(|f| f["type"] == "pin_contact")
            .collect();
        assert_eq!(contacts.len(), 1, "{:?}", report.findings);
        assert_eq!(contacts[0]["point"], json!({"x": 17.46, "y": 20.0}));
        assert!(contacts[0]["pins"][0].get("_local_tip").is_none());
    }

    #[test]
    fn moving_a_pin_within_a_continuing_contact_does_not_report_a_new_contact() {
        let source = r#"(kicad_sch
          (lib_symbols (symbol "test:Pin"
            (pin passive line (at 0 0 0) (length 0) (name "~") (number "1"))))
          (symbol (lib_id "test:Pin") (at 100 100 0)
            (uuid "a") (property "Reference" "J1"))
          (symbol (lib_id "test:Pin") (at 100.005 100 0)
            (uuid "b") (property "Reference" "J2")))"#;
        let before = parse_sexp(source).unwrap();
        let after = parse_sexp(&source.replace("100.005", "99.995")).unwrap();
        let report = added_clearance_findings(&before, &after, &["b"]);
        assert!(
            !report.findings.iter().any(|f| f["type"] == "pin_contact"),
            "{:?}",
            report.findings
        );
    }

    #[test]
    fn a_contact_point_is_within_reach_of_every_pin_in_its_group() {
        let source = r#"(kicad_sch
          (lib_symbols
            (symbol "test:A"
              (pin passive line (at 0 0 0) (length 0) (name "~") (number "1")))
            (symbol "test:B"
              (pin passive line (at 0 0 0) (length 0) (name "~") (number "1"))
              (pin passive line (at 0 -0.016 0) (length 0) (name "~") (number "1")))
            (symbol "test:C"
              (pin passive line (at 0 0 0) (length 0) (name "~") (number "1"))
              (pin passive line (at 0 0.016 0) (length 0) (name "~") (number "1"))))
          (symbol (lib_id "test:A") (at 100 100 0)
            (uuid "a") (property "Reference" "J1"))
          (symbol (lib_id "test:B") (at 100.007 99.992 0)
            (uuid "b") (property "Reference" "J2"))
          (symbol (lib_id "test:C") (at 100.014 99.992 0)
            (uuid "c") (property "Reference" "J3")))"#;
        let before = parse_sexp(source).unwrap();
        let after = parse_sexp(
            &source.replace("(at 100.014 99.992 0)", "(at 100.014 99.992 0) (mirror x)"),
        )
        .unwrap();
        let report = added_clearance_findings(&before, &after, &["c"]);
        let contacts: Vec<_> = report
            .findings
            .iter()
            .filter(|f| f["type"] == "pin_contact")
            .collect();
        assert_eq!(contacts.len(), 1, "{:?}", report.findings);
        assert_eq!(contacts[0]["point"], json!({"x": 100.007, "y": 100.008}));
    }

    #[test]
    fn only_new_findings_involving_changed_uuids_are_returned() {
        let before = parse_sexp(SOURCE).unwrap();
        let mut after = cse::Schematic::from_source("power_orientation.kicad_sch", SOURCE).unwrap();
        let label = after.add_label("OTHER", 114.3, 121.92).uuid.clone();
        let with_label = parse_sexp(&after.to_source()).unwrap();
        let findings = added_clearance_findings(&before, &with_label, &[&label]).findings;
        assert!(
            findings
                .iter()
                .any(|finding| finding["type"] == "label_overlap"),
            "{findings:?}"
        );
        let wire = after.add_wire(175.26, 152.4, 176.53, 152.4).uuid.clone();
        let with_wire = parse_sexp(&after.to_source()).unwrap();
        assert!(
            added_clearance_findings(&with_label, &with_wire, &[&label, &wire])
                .findings
                .is_empty()
        );
        assert!(added_clearance_findings(&before, &with_wire, &[&wire])
            .findings
            .is_empty());
    }

    #[test]
    fn distinct_labels_with_the_same_name_do_not_hide_a_new_overlap() {
        let mut sch = cse::Schematic::from_source("power_orientation.kicad_sch", SOURCE).unwrap();
        sch.add_label("A", 220.98, 180.34);
        let first = sch.add_label("B", 220.98, 180.34).uuid.clone();
        let second = sch.add_label("B", 228.6, 180.34).uuid.clone();
        let before = parse_sexp(&sch.to_source()).unwrap();
        let moved = sch
            .labels
            .iter_mut()
            .find(|label| label.uuid == second)
            .unwrap();
        moved.at.x = 220.98;
        let after = parse_sexp(&sch.to_source()).unwrap();
        let report = added_clearance_findings(&before, &after, &[&first, &second]);
        let label_overlaps: Vec<_> = report
            .findings
            .iter()
            .filter(|f| f["type"] == "label_overlap")
            .collect();
        assert_eq!(label_overlaps.len(), 1, "{:?}", report.findings);
        assert!(label_overlaps[0].get("_participants").is_none());
    }

    #[test]
    fn reordering_a_label_does_not_reintroduce_an_anchor_overlap() {
        let mut sch = cse::Schematic::from_source("power_orientation.kicad_sch", SOURCE).unwrap();
        let uuid = sch.add_label("A", 220.98, 180.34).uuid.clone();
        sch.add_label("B", 221.08, 180.34);
        let before = parse_sexp(&sch.to_source()).unwrap();
        let mut a = sch
            .labels
            .iter()
            .find(|label| label.uuid == uuid)
            .unwrap()
            .clone();
        sch.labels.retain(|label| label.uuid != uuid);
        a.set_rotation(180.0);
        sch.labels.push(a);
        let after = parse_sexp(&sch.to_source()).unwrap();
        let report = added_clearance_findings(&before, &after, &[&uuid]);
        assert!(
            !report.findings.iter().any(|f| f["type"] == "label_overlap"),
            "{:?}",
            report.findings
        );
    }

    #[test]
    fn a_new_collinear_overlap_names_both_wires() {
        let mut sch = cse::Schematic::from_source("power_orientation.kicad_sch", SOURCE).unwrap();
        let old = sch.add_wire(220.98, 180.34, 226.06, 180.34).uuid.clone();
        let before = parse_sexp(&sch.to_source()).unwrap();
        let new = sch.add_wire(228.6, 180.34, 223.52, 180.34).uuid.clone();
        let after = parse_sexp(&sch.to_source()).unwrap();
        let report = added_clearance_findings(&before, &after, &[&new]);
        let [finding] = report.findings.as_slice() else {
            panic!("expected one collinear overlap: {:?}", report.findings);
        };
        assert_eq!(finding["type"], "collinear_overlap");
        assert_eq!(finding["a"]["uuid"], old);
        assert_eq!(finding["b"]["uuid"], new);
    }

    #[test]
    fn reversing_a_wire_does_not_reintroduce_a_collinear_overlap() {
        let mut sch = cse::Schematic::from_source("power_orientation.kicad_sch", SOURCE).unwrap();
        sch.add_wire(220.98, 180.34, 226.06, 180.34);
        let wire = sch.add_wire(228.6, 180.34, 223.52, 180.34).clone();
        let before = parse_sexp(&sch.to_source()).unwrap();
        let reversed = sch.wires.iter_mut().find(|w| w.uuid == wire.uuid).unwrap();
        std::mem::swap(&mut reversed.start, &mut reversed.end);
        let after = parse_sexp(&sch.to_source()).unwrap();
        assert!(added_clearance_findings(&before, &after, &[&wire.uuid])
            .findings
            .is_empty());
    }

    #[test]
    fn reordering_symbols_does_not_reintroduce_a_pin_contact() {
        const CONTACT: &str =
            include_str!("../../tests/fixtures/pin_contact_kicad10/pin_contact.kicad_sch");
        let before = parse_sexp(CONTACT).unwrap();
        let mut sch = cse::Schematic::from_source("pin_contact.kicad_sch", CONTACT).unwrap();
        // J1 follows R1 in the fixture; put it before R1 so the contact's
        // raw participant order actually changes.
        let r1 = sch.symbols.remove_by_reference("R1").unwrap();
        let uuid = r1.uuid.clone();
        sch.symbols.push(r1);
        let after = parse_sexp(&sch.to_source()).unwrap();
        let participants = |tree| {
            super::super::sch_pin_contacts::clearance_pin_contacts(tree, &[])
                .0
                .into_iter()
                .find(|contact| {
                    contact["pins"]
                        .as_array()
                        .unwrap()
                        .iter()
                        .any(|pin| pin["symbol_uuid"] == uuid)
                })
                .unwrap()["pins"]
                .clone()
        };
        assert_ne!(participants(&before), participants(&after));
        let report = added_clearance_findings(&before, &after, &[&uuid]);
        assert!(
            !report.findings.iter().any(|f| f["type"] == "pin_contact"),
            "{:?}",
            report.findings
        );
    }
}

/// Whether `text` is one of `instance`'s own fields: the same placed symbol by
/// its uuid, or by reference and unit where either has none.
fn is_own_field(text: &TextExtent, instance: &SymbolInstance) -> bool {
    let TextOwner::Field {
        reference,
        unit,
        symbol_uuid,
        ..
    } = &text.owner
    else {
        return false;
    };
    match (symbol_uuid, &instance.uuid) {
        (Some(field_of), Some(uuid)) => field_of == uuid,
        _ => *reference == instance.reference && *unit == instance.unit,
    }
}

fn wire_json(wire: &Wire) -> serde_json::Value {
    json!({
        "kind": "wire",
        "uuid": wire.uuid,
        "x1": wire.x1,
        "y1": wire.y1,
        "x2": wire.x2,
        "y2": wire.y2
    })
}

/// The label or field whose text an overlap involves.
fn text_json(text: &TextExtent) -> serde_json::Value {
    match &text.owner {
        TextOwner::Label {
            kind, net, uuid, ..
        } => json!({
            "kind": "label",
            "label_type": match kind {
                LabelKind::GlobalLabel => "global_label",
                LabelKind::HierarchicalLabel => "hierarchical_label",
                _ => "net_label",
            },
            "net": net,
            "uuid": uuid,
            "text": text.text
        }),
        TextOwner::Field {
            reference,
            unit,
            symbol_uuid,
            field,
            ..
        } => json!({
            "kind": "field",
            "reference": reference,
            "unit": unit,
            "symbol_uuid": symbol_uuid,
            "field": field,
            "text": text.text
        }),
    }
}

#[cfg(test)]
mod orphan_item_tests {
    use super::*;
    use crate::tools::ServerConfig;
    use std::io::Write;
    use std::sync::Arc;

    /// Run the registered tool against a temporary schematic, exactly as the
    /// MCP dispatch layer does after selecting its `ToolDef`.
    async fn call_result(schematic: &str, mut args: serde_json::Value) -> CallToolResult {
        let mut file = tempfile::NamedTempFile::with_suffix(".kicad_sch").unwrap();
        file.write_all(schematic.as_bytes()).unwrap();
        file.flush().unwrap();

        args["schematic"] = json!(file.path().to_str().unwrap());
        let definition = tools()
            .into_iter()
            .find(|tool| tool.name == "find_orphan_items")
            .unwrap();
        let context = ToolContext::new(
            ServerConfig {
                kicad_cli: String::new(),
                kicad_binary: String::new(),
                ipc_address: String::new(),
                project_dir: None,
                jlcpcb_db_path: None,
                auto_load_toolsets: false,
                eager_toolsets: false,
            },
            Arc::new(crate::router::ToolRouter::new()),
        );
        (definition.handler)(&args, Arc::new(context))
            .await
            .unwrap()
    }

    async fn call(schematic: &str, args: serde_json::Value) -> serde_json::Value {
        let result = call_result(schematic, args).await;
        assert!(!result.is_error, "find_orphan_items failed");
        let crate::mcp::protocol::ToolContent::Text { text } = &result.content[0] else {
            panic!("expected text content");
        };
        serde_json::from_str(text).unwrap()
    }

    /// One wire from (90,100) to a zero-length pin of U1 at (100,100), a `SIG`
    /// label mid-segment, a stray `ORPHAN` label, and U2 with nothing on its pin.
    fn schematic(extra: &str) -> String {
        format!(
            r#"(kicad_sch
  (version 20260306)
  (generator "eeschema")
  (uuid "root")
  (lib_symbols
    (symbol "Test:P"
      (symbol "P_1_1"
        (pin passive line (at 0 0 0) (length 0) (name "A") (number "1"))
      )
    )
  )
  (wire (pts (xy 90 100) (xy 100 100)) (uuid "w1"))
  (label "SIG" (at 95 100 0))
  (label "ORPHAN" (at 200 200 0))
  (symbol (lib_id "Test:P") (at 100 100 0) (unit 1) (uuid "u1")
    (property "Reference" "U1" (at 100 100 0))
    (property "Value" "P" (at 100 100 0))
  )
  (symbol (lib_id "Test:P") (at 150 150 0) (unit 1) (uuid "u2")
    (property "Reference" "U2" (at 150 150 0))
    (property "Value" "P" (at 150 150 0))
  )
{extra}  (sheet_instances (path "/" (page "1")))
)
"#
        )
    }

    async fn orphans(extra: &str) -> Vec<serde_json::Value> {
        let body = call(&schematic(extra), json!({})).await;
        body["orphans"].as_array().unwrap().clone()
    }

    fn of_type<'a>(items: &'a [serde_json::Value], kind: &str) -> Vec<&'a serde_json::Value> {
        items.iter().filter(|item| item["type"] == kind).collect()
    }

    #[tokio::test]
    async fn a_wire_ending_on_a_pin_is_not_dangling() {
        let items = orphans("").await;
        let dangling = of_type(&items, "dangling_wire_end");
        assert_eq!(dangling.len(), 1, "only the free end counts: {items:?}");
        assert_eq!(dangling[0]["x"], 90.0);
        assert_eq!(dangling[0]["wire_uuid"], "w1");
    }

    #[tokio::test]
    async fn a_label_mid_segment_is_not_floating() {
        let items = orphans("").await;
        let floating = of_type(&items, "floating_label");
        assert_eq!(floating.len(), 1, "only ORPHAN floats: {items:?}");
        assert_eq!(floating[0]["net"], "ORPHAN");
    }

    #[tokio::test]
    async fn a_pin_with_nothing_on_it_is_reported() {
        let items = orphans("").await;
        let pins = of_type(&items, "unconnected_pin");
        assert_eq!(pins.len(), 1, "U1's pin is wired: {items:?}");
        assert_eq!(pins[0]["reference"], "U2");
        assert_eq!(pins[0]["pin"], "1");
        assert_eq!(pins[0]["pin_name"], "A");
    }

    /// The reported #249 case: a label directly on a pin without a wire is a
    /// legal KiCAD connection and connects both items.
    #[tokio::test]
    async fn a_label_on_a_bare_pin_connects_both_items() {
        let items = orphans("  (label \"NC_SIG\" (at 150 150 0))\n").await;
        let floating = of_type(&items, "floating_label");
        assert_eq!(floating.len(), 1, "NC_SIG is on U2's pin: {items:?}");
        assert_eq!(floating[0]["net"], "ORPHAN");
        assert!(
            of_type(&items, "unconnected_pin").is_empty(),
            "the label connects U2: {items:?}"
        );
    }

    #[tokio::test]
    async fn a_no_connect_flag_exempts_its_pin() {
        let items = orphans("  (no_connect (at 150 150) (uuid \"nc1\"))\n").await;
        assert!(
            of_type(&items, "unconnected_pin").is_empty(),
            "no-connect covers U2: {items:?}"
        );
    }

    #[tokio::test]
    async fn an_intrinsically_no_connect_pin_is_exempt() {
        let no_connect_pin = schematic("").replace(
            "(pin passive line (at 0 0 0)",
            "(pin no_connect line (at 0 0 0)",
        );
        let body = call(&no_connect_pin, json!({})).await;
        let items = body["orphans"].as_array().unwrap();
        assert!(
            of_type(items, "unconnected_pin").is_empty(),
            "library no-connect pins are intentional: {items:?}"
        );
    }

    /// A multi-unit symbol must contribute only the pins from the placed unit.
    #[tokio::test]
    async fn another_unit_does_not_contribute_a_phantom_pin() {
        let schematic = r#"(kicad_sch
  (version 20260306)
  (generator "eeschema")
  (uuid "root")
  (lib_symbols
    (symbol "Test:D"
      (symbol "D_1_1"
        (pin passive line (at 0 0 0) (length 0) (name "A") (number "1"))
      )
      (symbol "D_2_1"
        (pin passive line (at 10 0 0) (length 0) (name "B") (number "2"))
      )
    )
  )
  (wire (pts (xy 90 100) (xy 100 100)) (uuid "w1"))
  (label "SIG" (at 90 100 0))
  (symbol (lib_id "Test:D") (at 100 100 0) (unit 1) (uuid "u1")
    (property "Reference" "U1" (at 100 100 0))
    (property "Value" "D" (at 100 100 0))
  )
  (sheet_instances (path "/" (page "1")))
)
"#;
        let body = call(schematic, json!({})).await;
        assert_eq!(body["orphan_count"], 0, "unit 2 is not placed here: {body}");
    }

    #[tokio::test]
    async fn a_wire_ending_on_a_sheet_pin_is_not_dangling() {
        let sheet = r#"  (sheet (at 80 95) (size 10 10) (uuid "s1")
    (property "Sheetname" "sub" (at 80 95 0))
    (property "Sheetfile" "sub.kicad_sch" (at 80 95 0))
    (pin "SIG" input (at 90 100 180) (uuid "sp1"))
  )
"#;
        let items = orphans(sheet).await;
        assert!(
            of_type(&items, "dangling_wire_end").is_empty(),
            "both ends terminate: {items:?}"
        );
    }

    #[tokio::test]
    async fn tolerance_must_be_positive() {
        for tolerance in [0.0, -0.05] {
            let result = call_result(&schematic(""), json!({ "tolerance": tolerance })).await;
            assert!(result.is_error, "accepted tolerance {tolerance}");
        }
    }
}

#[cfg(test)]
mod tool_call_support {
    use super::*;
    use crate::tools::ServerConfig;
    use std::io::Write;
    use std::sync::Arc;

    /// Run a tool by name against a temp file holding `sch`, exactly as the MCP
    /// dispatch layer does after selecting its `ToolDef`. Returns the result as
    /// it comes back, so a refusal can be asserted as a refusal.
    pub(super) async fn call_result(
        sch: &str,
        tool: &str,
        args: serde_json::Value,
    ) -> CallToolResult {
        let mut f = tempfile::NamedTempFile::with_suffix(".kicad_sch").unwrap();
        f.write_all(sch.as_bytes()).unwrap();
        f.flush().unwrap();
        call_result_at(f.path(), tool, args).await
    }

    /// The same against an existing file, so one test can chain calls on it.
    /// `sch_batch` is searched too, for its writers.
    pub(super) async fn call_result_at(
        path: &std::path::Path,
        tool: &str,
        mut args: serde_json::Value,
    ) -> CallToolResult {
        args["schematic"] = json!(path.to_str().unwrap());
        let def = tools()
            .into_iter()
            .chain(crate::tools::sch_batch::tools())
            .chain(crate::tools::sch_wiring::tools())
            .find(|t| t.name == tool)
            .unwrap();
        let ctx = ToolContext::new(
            ServerConfig::default(),
            Arc::new(crate::router::ToolRouter::new()),
        );
        (def.handler)(&args, Arc::new(ctx)).await.unwrap()
    }

    /// The body of a call that must have succeeded.
    pub(super) async fn call(sch: &str, tool: &str, args: serde_json::Value) -> serde_json::Value {
        body(tool, call_result(sch, tool, args).await)
    }

    /// The body of a call on an existing file that must have succeeded.
    pub(super) async fn call_at(
        path: &std::path::Path,
        tool: &str,
        args: serde_json::Value,
    ) -> serde_json::Value {
        body(tool, call_result_at(path, tool, args).await)
    }

    fn body(tool: &str, result: CallToolResult) -> serde_json::Value {
        assert!(!result.is_error, "{tool} failed: {:?}", result.content);
        let crate::mcp::protocol::ToolContent::Text { text } = &result.content[0] else {
            panic!("expected text content");
        };
        serde_json::from_str(text).unwrap()
    }

    /// The `pins` of a `get_net_connections` answer as sorted `REF.pin`
    /// strings, the form `kicad-cli sch export netlist` lists a net's nodes in.
    pub(super) fn net_pins(body: &serde_json::Value) -> Vec<String> {
        let mut pins: Vec<String> = body["pins"]
            .as_array()
            .unwrap_or_else(|| panic!("pins array: {body}"))
            .iter()
            .map(|p| {
                format!(
                    "{}.{}",
                    p["reference"].as_str().unwrap(),
                    p["pin"].as_str().unwrap()
                )
            })
            .collect();
        pins.sort_unstable();
        pins
    }
}

#[cfg(test)]
mod power_symbol_net_tests {
    use super::tool_call_support::{call, call_result, net_pins};
    use super::*;

    /// R1 with pin 1 on a `SIG` label and pin 2 wired down to a `power:GND`
    /// symbol. The rail is what a label-only net graph loses.
    const SCH: &str = include_str!("../../tests/fixtures/power_rail.kicad_sch");

    /// The S-expression path: a pin reached only through a power symbol used to
    /// report no net at all.
    #[tokio::test]
    async fn a_pin_on_a_rail_reports_it() {
        let s = call(
            SCH,
            "get_pin_connections",
            json!({ "reference": "R1", "pin_number": "2" }),
        )
        .await;
        assert_eq!(s["net"], "GND");
    }

    #[tokio::test]
    async fn the_rail_lists_the_components_on_it() {
        let s = call(SCH, "get_net_components", json!({ "net": "GND" })).await;
        let refs: Vec<&str> = s["components"]
            .as_array()
            .unwrap()
            .iter()
            .map(|c| c["reference"].as_str().unwrap())
            .collect();
        assert!(refs.contains(&"R1"), "R1 is on GND, got {refs:?}");
    }

    /// KiCad's netlist puts only `R1.2` on GND: the `#PWR01` symbol names the
    /// net but is not a node on it, and `R1.1` is on `SIG`.
    #[tokio::test]
    async fn net_connections_lists_exactly_the_pins_kicad_nets() {
        let s = call(SCH, "get_net_connections", json!({ "net": "GND" })).await;
        assert_eq!(net_pins(&s), ["R1.2"], "{s}");
        let pin = &s["pins"][0];
        assert_eq!(pin["name"], "~", "{s}");
        assert_eq!(
            (pin["x"].as_f64(), pin["y"].as_f64()),
            (Some(100.0), Some(103.81))
        );
    }

    /// A symbol whose library entry is missing contributes no pins, so the
    /// answer would come back short with nothing to say so. It is refused.
    #[tokio::test]
    async fn net_connections_refuse_a_symbol_with_no_library_entry() {
        let stale = SCH.replace("(lib_id \"Device:R\")", "(lib_id \"Device:Missing\")");
        let result = call_result(&stale, "get_net_connections", json!({ "net": "GND" })).await;
        assert!(result.is_error, "a short pin list was answered as complete");
        let crate::mcp::protocol::ToolContent::Text { text } = &result.content[0] else {
            panic!("expected text content");
        };
        assert_eq!(
            text,
            &format!("Cannot list the pins on net 'GND': {UNRESOLVED_PIN_GEOMETRY}")
        );
    }

    #[tokio::test]
    async fn tracing_a_point_on_the_rail_names_it() {
        let s = call(SCH, "trace_from_point", json!({ "x": 100.0, "y": 103.81 })).await;
        assert_eq!(s["net"], "GND");
    }

    #[tokio::test]
    async fn the_rail_is_listed_among_the_nets() {
        let s = call(SCH, "list_schematic_nets", json!({})).await;
        let nets: Vec<&str> = s["nets"]
            .as_array()
            .unwrap()
            .iter()
            .map(|n| n.as_str().unwrap())
            .collect();
        assert_eq!(nets, vec!["GND", "SIG"]);
    }

    /// A rail carried onto a wire that also carries a named label is a short,
    /// and it was invisible while the graph knew only labels.
    #[tokio::test]
    async fn a_rail_shorted_to_a_named_net_is_reported() {
        let shorted = SCH.replace(
            "(label \"SIG\" (at 100 96.19 0)",
            "(label \"SIG\" (at 100 106 0)",
        );
        let s = call(&shorted, "find_shorted_nets", json!({})).await;
        assert_eq!(s["short_count"], 1, "SIG and GND share one wire: {s}");
    }
}

#[cfg(test)]
mod single_pin_net_tests {
    use super::tool_call_support::call;
    use super::*;

    /// A KiCad 10.0.5-authored parent sheet carrying one net of every shape the
    /// tool has to tell apart. Provenance and the KiCad netlist that fixes the
    /// expected pin count of each net are in `single_pin_nets.README.md`; the
    /// counts asserted below are that netlist's, not this crate's.
    ///
    /// - `TWO_PIN` — one label, R1.2 and R2.1. The ordinary way to draw a net,
    ///   and what the label count reported as a single-pin net.
    /// - `LONE` — two labels on two wire segments that never meet, one pin
    ///   between them. The defect the tool exists to find, invisible while a
    ///   second label made the count 2, and it can only be found by pooling the
    ///   two roots.
    /// - `SPLIT` — the same shape with a pin on each root. Pooling is what
    ///   keeps it off the report; KiCad's netlister agrees it is one 2-pin net.
    /// - `VCC` — a power symbol and one component pin.
    /// - `FLAGGED` — a `PWR_FLAG` (`#FLG01`) and one component pin.
    /// - `SHEET_NET` — R8.2 and a hierarchical sheet pin.
    /// - `MIXED` — a local and a global label naming one net that reaches one
    ///   pin.
    /// - `STUB` — a label on a wire that reaches no pin at all.
    /// - `GND` — the rail nine pins sit on, the control for a net that is
    ///   named by a power symbol and is not a defect.
    const SCH: &str = include_str!("../../tests/fixtures/single_pin_nets.kicad_sch");

    /// The child of that sheet: `SHEET_NET` continues here onto R10.2 through
    /// a hierarchical label, so each sheet on its own sees one pin of a net
    /// KiCad resolves to two.
    const CHILD: &str = include_str!("../../tests/fixtures/single_pin_nets_child.kicad_sch");

    async fn nets_of(sch: &str) -> Vec<serde_json::Value> {
        call(sch, "find_single_pin_nets", json!({})).await["nets"]
            .as_array()
            .unwrap()
            .clone()
    }

    async fn nets() -> Vec<serde_json::Value> {
        nets_of(SCH).await
    }

    fn net<'a>(nets: &'a [serde_json::Value], name: &str) -> Option<&'a serde_json::Value> {
        nets.iter().find(|net| net["net"] == name)
    }

    fn expect<'a>(nets: &'a [serde_json::Value], name: &str) -> &'a serde_json::Value {
        net(nets, name).unwrap_or_else(|| panic!("{name} missing: {nets:?}"))
    }

    /// The false positive that made the tool unusable: 11 of 11 nets reported
    /// on a real sheet, every one of them wired to two or more pins.
    #[tokio::test]
    async fn a_net_with_one_label_and_two_pins_is_not_reported() {
        let nets = nets().await;

        assert!(net(&nets, "TWO_PIN").is_none(), "{nets:?}");
    }

    /// And the miss on the other side: a second label used to make a real
    /// single-pin net count 2 and disappear.
    #[tokio::test]
    async fn a_net_with_two_labels_and_one_pin_is_reported() {
        let nets = nets().await;
        let lone = expect(&nets, "LONE");

        assert_eq!(lone["pin_count"], 1);
        assert_eq!(lone["label_count"], 2, "the label count is kept, not used");
        assert_eq!(lone["pins"][0]["reference"], "R3");
        assert_eq!(lone["pins"][0]["pin"], "2");
    }

    /// `LONE`'s two labels sit on wire segments that never touch. KiCAD joins
    /// them by name, so the pins under both roots are one net's pins — count
    /// them per root and the one pin under the far root is a second net that
    /// reaches nothing.
    #[tokio::test]
    async fn labels_of_one_name_on_disconnected_roots_pool() {
        let nets = nets().await;

        assert_eq!(expect(&nets, "LONE")["pin_count"], 1);
    }

    /// The same two-root shape with a pin on each root. Pooling is the only
    /// reason this is not reported, and KiCad's own netlister resolves it to a
    /// single net of R4.2 and R5.1.
    #[tokio::test]
    async fn one_pin_under_each_of_two_pooled_roots_is_two_pins() {
        let nets = nets().await;

        assert!(net(&nets, "SPLIT").is_none(), "{nets:?}");
    }

    /// A rail that reaches exactly one component pin is the defect. The power
    /// symbol's own pin names it and must not make the count 2.
    #[tokio::test]
    async fn a_rail_reaching_one_pin_is_reported_without_its_power_symbol() {
        let nets = nets().await;
        let vcc = expect(&nets, "VCC");

        assert_eq!(vcc["pin_count"], 1);
        assert_eq!(vcc["pins"][0]["reference"], "R6", "not #PWR001");
    }

    /// `PWR_FLAG` is the other symbol whose pin names a net without consuming
    /// one, and it is not a `LabelKind::PowerSymbol` — its pin is `power_out`,
    /// so the `(power)` test lets it through. `#FLG01` is what keeps it out.
    #[tokio::test]
    async fn a_pwr_flag_pin_does_not_count_as_a_connection() {
        let nets = nets().await;
        let flagged = expect(&nets, "FLAGGED");

        assert_eq!(flagged["pin_count"], 1);
        assert_eq!(flagged["pins"][0]["reference"], "R7", "not #FLG01");
    }

    /// The rail nine pins sit on. A power symbol names it, so the tool has to
    /// count past the naming symbol without reporting the net.
    #[tokio::test]
    async fn a_rail_reaching_many_pins_is_not_reported() {
        let nets = nets().await;

        assert!(net(&nets, "GND").is_none(), "{nets:?}");
    }

    /// A net leaving the sheet is connected to whatever is on the other side.
    #[tokio::test]
    async fn a_hierarchical_sheet_pin_counts_as_a_pin() {
        let nets = nets().await;

        assert!(net(&nets, "SHEET_NET").is_none(), "{nets:?}");
    }

    /// The stub a deleted component leaves behind reaches nothing at all. The
    /// label count reported it, `find_orphan_items` names only its wire end,
    /// and counting pins must not drop it on the floor. KiCad's netlist has no
    /// `STUB` net to compare against, which is the point.
    #[tokio::test]
    async fn a_net_that_reaches_no_pin_is_reported_as_zero() {
        let nets = nets().await;
        let stub = expect(&nets, "STUB");

        assert_eq!(stub["pin_count"], 0);
        assert_eq!(stub["label_count"], 1);
        assert_eq!(stub["pins"].as_array().unwrap().len(), 0);
    }

    /// Local labels are extracted first, so `type` alone reports `NetLabel` for
    /// a net that also carries a global label and hides the one fact a caller
    /// needs: this count is not the whole net.
    #[tokio::test]
    async fn a_net_carrying_a_global_label_reports_every_kind_that_named_it() {
        let nets = nets().await;
        let mixed = expect(&nets, "MIXED");

        assert_eq!(mixed["type"], "NetLabel", "the compatibility field");
        assert_eq!(
            mixed["label_types"],
            json!(["GlobalLabel", "NetLabel"]),
            "sorted, and the global label is not lost"
        );
        assert_eq!(mixed["cross_sheet_unverified"], true);
    }

    /// A net named only by a local label is fully answered by this sheet.
    #[tokio::test]
    async fn a_locally_named_net_is_not_flagged_cross_sheet() {
        let nets = nets().await;
        let stub = expect(&nets, "STUB");

        assert_eq!(stub["label_types"], json!(["NetLabel"]));
        assert_eq!(stub["cross_sheet_unverified"], false);
    }

    /// A power symbol is a global label in KiCAD, so a rail counted on one
    /// sheet is a lead, not a finding — even when, as here, the project-wide
    /// netlist agrees that `VCC` reaches exactly one pin.
    #[tokio::test]
    async fn a_power_symbol_rail_is_flagged_cross_sheet() {
        let nets = nets().await;
        let vcc = expect(&nets, "VCC");

        assert_eq!(vcc["label_types"], json!(["PowerSymbol"]));
        assert_eq!(vcc["cross_sheet_unverified"], true);
    }

    /// The hierarchy boundary from the child's side. `SHEET_NET` reaches R10.2
    /// and its hierarchical label here, and R8.2 and the sheet pin on the
    /// parent; KiCad resolves the pair to one 2-pin net. Neither sheet can see
    /// that alone, so the child reports one pin and says it is unverified.
    #[tokio::test]
    async fn a_child_sheet_reports_a_hierarchical_net_as_unverified() {
        let nets = nets_of(CHILD).await;
        let sheet_net = expect(&nets, "SHEET_NET");

        assert_eq!(sheet_net["pin_count"], 1);
        assert_eq!(sheet_net["pins"][0]["reference"], "R10");
        assert_eq!(sheet_net["label_types"], json!(["HierarchicalLabel"]));
        assert_eq!(sheet_net["cross_sheet_unverified"], true);
    }

    /// And the control on the same child: a local net of two pins is answered
    /// there in full.
    #[tokio::test]
    async fn a_child_sheet_local_net_of_two_pins_is_not_reported() {
        let nets = nets_of(CHILD).await;

        assert!(net(&nets, "CHILD_LOCAL").is_none(), "{nets:?}");
    }

    /// The nets came out of a `HashMap` in whatever order it held them, so two
    /// runs of the same binary over the same sheet could disagree on the order.
    #[tokio::test]
    async fn the_report_is_sorted_and_stable() {
        let names: Vec<String> = nets()
            .await
            .iter()
            .map(|net| net["net"].as_str().unwrap().to_string())
            .collect();

        assert_eq!(names, vec!["FLAGGED", "LONE", "MIXED", "STUB", "VCC"]);
    }
}

#[cfg(test)]
mod cross_sheet_single_pin_tests {
    use super::tool_call_support::call_at;
    use super::*;

    const FIXTURE: &str = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/fixtures/annotate_nested_hierarchy"
    );

    /// A copy of the nested fixture. `nested_pages` places `bank` twice and
    /// `bank` places `cell` twice, so `cell` is in the design four times.
    /// `+3V3` is on all three sheets and reaches no component pin.
    ///
    /// Expected pin counts are KiCad's, not this code's: each scenario below
    /// was applied to a copy with `add_power_symbol` and exported with
    /// `kicad-cli sch export netlist --format kicadxml nested_pages.kicad_sch`
    /// (KiCad 10.0.7):
    ///
    /// | tie                                 | `+3V3` nodes      |
    /// |-------------------------------------|-------------------|
    /// | none                                | none (no such net) |
    /// | `cell` R7 pin 1 to `+3V3`           | R7, R9, R14, R16  |
    /// | `bank` R3 pin 1 to `+3V3`           | R3, R10           |
    /// | `nested_pages` R1 pin 1 to `+3V3`   | R1                |
    ///
    /// Only `+3V3` is used: its symbol is cached in the fixture's sheets, so
    /// `add_power_symbol` needs no installed KiCad library.
    fn project() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        for entry in std::fs::read_dir(FIXTURE).unwrap().flatten() {
            std::fs::copy(entry.path(), dir.path().join(entry.file_name())).unwrap();
        }
        dir
    }

    fn net<'a>(report: &'a serde_json::Value, name: &str) -> Option<&'a serde_json::Value> {
        report["nets"]
            .as_array()
            .unwrap()
            .iter()
            .find(|net| net["net"] == name)
    }

    async fn single_pin_nets(sheet: &std::path::Path) -> serde_json::Value {
        call_at(sheet, "find_single_pin_nets", json!({})).await
    }

    async fn tie_to_3v3(sheet: &std::path::Path, reference: &str) {
        call_at(
            sheet,
            "add_power_symbol",
            json!({ "power_net": "+3V3", "reference": reference, "pin_number": "1" }),
        )
        .await;
    }

    /// KiCad: `+3V3` reaches no pin. Checked across the whole project it is a
    /// finding, no longer a lead.
    #[tokio::test]
    async fn a_net_checked_project_wide_is_verified() {
        let dir = project();
        let report = single_pin_nets(&dir.path().join("cell.kicad_sch")).await;
        let rail = net(&report, "+3V3").expect("+3V3 reaches no pin anywhere");
        assert_eq!(rail["pin_count"], 0, "{report}");
        assert_eq!(rail["cross_sheet_unverified"], false, "{report}");
        assert_eq!(report["sheets_checked"], 3, "{report}");
        assert_eq!(report["cross_sheet_resolved"], json!([]), "{report}");
    }

    /// KiCad: one pin on `cell` is four pins in the design, because `cell` is
    /// placed four times. Asked from `cell` itself and from the root, which
    /// sees the pin only through the other sheet, `+3V3` is resolved.
    #[tokio::test]
    async fn a_pin_on_a_sheet_placed_four_times_is_four_pins() {
        let dir = project();
        let cell = dir.path().join("cell.kicad_sch");
        tie_to_3v3(&cell, "R7").await;
        for sheet in [cell.clone(), dir.path().join("nested_pages.kicad_sch")] {
            let report = single_pin_nets(&sheet).await;
            assert!(net(&report, "+3V3").is_none(), "{report}");
            assert_eq!(report["cross_sheet_resolved"], json!(["+3V3"]), "{report}");
        }
    }

    /// KiCad: one pin on `bank`, placed twice, is two pins. The sheet the
    /// tool is given is weighted by its own placements too.
    #[tokio::test]
    async fn the_given_sheet_counts_once_per_placement() {
        let dir = project();
        let bank = dir.path().join("bank.kicad_sch");
        tie_to_3v3(&bank, "R3").await;
        let report = single_pin_nets(&bank).await;
        assert!(net(&report, "+3V3").is_none(), "{report}");
        assert_eq!(report["cross_sheet_resolved"], json!(["+3V3"]), "{report}");
    }

    /// KiCad: one pin on the root, placed once, is one pin. It stays a
    /// verified single-pin net.
    #[tokio::test]
    async fn one_pin_on_a_sheet_placed_once_is_a_verified_finding() {
        let dir = project();
        let root = dir.path().join("nested_pages.kicad_sch");
        tie_to_3v3(&root, "R1").await;
        let report = single_pin_nets(&root).await;
        let rail = net(&report, "+3V3").expect("one pin in the whole design");
        assert_eq!(rail["pin_count"], 1, "{report}");
        assert_eq!(rail["cross_sheet_unverified"], false, "{report}");
        assert_eq!(report["cross_sheet_resolved"], json!([]), "{report}");
    }

    /// Without a project file there is no hierarchy to read: the per-sheet
    /// answer and its flag stand, and nothing claims a project-wide check.
    #[tokio::test]
    async fn without_a_project_the_net_stays_unverified() {
        let dir = project();
        std::fs::remove_file(dir.path().join("nested_pages.kicad_pro")).unwrap();
        let report = single_pin_nets(&dir.path().join("cell.kicad_sch")).await;
        assert_eq!(
            net(&report, "+3V3").unwrap()["cross_sheet_unverified"],
            true
        );
        assert!(report.get("sheets_checked").is_none(), "{report}");
        assert!(report.get("cross_sheet_resolved").is_none(), "{report}");
    }

    /// A second project file beside the first makes ownership unproven
    /// (`resolve_schematic_ownership` refuses it), whatever order the
    /// directory lists them in: the per-sheet answer stands.
    #[tokio::test]
    async fn two_project_files_leave_the_per_sheet_answer() {
        let dir = project();
        std::fs::copy(
            dir.path().join("nested_pages.kicad_pro"),
            dir.path().join("another.kicad_pro"),
        )
        .unwrap();
        let report = single_pin_nets(&dir.path().join("cell.kicad_sch")).await;
        assert_eq!(
            net(&report, "+3V3").unwrap()["cross_sheet_unverified"],
            true,
            "{report}"
        );
        assert!(report.get("sheets_checked").is_none(), "{report}");
    }
}

#[cfg(test)]
mod multi_unit_tool_tests {
    use super::*;
    use crate::tools::ServerConfig;
    use std::io::Write;
    use std::sync::Arc;

    const ECC83: &str = include_str!("../../tests/fixtures/ecc83_multiunit.kicad_sch");

    async fn call_content(
        content: &str,
        tool: &str,
        mut args: serde_json::Value,
    ) -> serde_json::Value {
        let mut schematic = tempfile::NamedTempFile::with_suffix(".kicad_sch").unwrap();
        schematic.write_all(content.as_bytes()).unwrap();
        schematic.flush().unwrap();

        args["schematic"] = json!(schematic.path().to_str().unwrap());
        let definition = tools()
            .into_iter()
            .find(|tool_def| tool_def.name == tool)
            .unwrap();
        let context = ToolContext::new(
            ServerConfig::default(),
            Arc::new(crate::router::ToolRouter::new()),
        );
        let result = (definition.handler)(&args, Arc::new(context))
            .await
            .unwrap();
        assert!(!result.is_error, "{tool} failed");
        let crate::mcp::protocol::ToolContent::Text { text } = &result.content[0] else {
            panic!("expected text content");
        };
        serde_json::from_str(text).unwrap()
    }

    async fn call(tool: &str, args: serde_json::Value) -> serde_json::Value {
        call_content(ECC83, tool, args).await
    }

    #[tokio::test]
    async fn pin_lookup_uses_the_unit_that_owns_the_pin() {
        let result = call(
            "get_pin_connections",
            json!({ "reference": "U1", "pin_number": "4" }),
        )
        .await;

        let pin_x = result["pin_x"].as_f64().unwrap();
        let pin_y = result["pin_y"].as_f64().unwrap();
        assert!((pin_x - 60.96).abs() < 1e-9, "wrong unit x: {pin_x}");
        assert!((pin_y - 86.36).abs() < 1e-9, "wrong unit y: {pin_y}");
    }

    /// A reference on another sheet of the project is refused with that
    /// sheet's file, not a bare "not found" that reads as "no such part".
    /// KiCad-saved `annotate_nested_hierarchy`: `kicad-cli` netlists `R9` on
    /// Sheetfile `cell.kicad_sch`, as the instance of the symbol saved there
    /// as `R7` (see `reference_hint::tests`).
    #[tokio::test]
    async fn component_nets_name_the_sheet_a_missing_reference_is_on() {
        let fixture = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/fixtures/annotate_nested_hierarchy"
        );
        let dir = tempfile::tempdir().unwrap();
        for name in ["nested_pages.kicad_sch", "bank.kicad_sch", "cell.kicad_sch"] {
            std::fs::copy(
                std::path::Path::new(fixture).join(name),
                dir.path().join(name),
            )
            .unwrap();
        }
        let root = dir.path().join("nested_pages.kicad_sch");
        let cell = dir.path().join("cell.kicad_sch");
        let context = Arc::new(ToolContext::new(
            ServerConfig::default(),
            Arc::new(crate::router::ToolRouter::new()),
        ));
        let args = json!({ "schematic": root.to_str().unwrap(), "reference": "R9" });
        let error = handle_get_component_nets(&args, &context)
            .await
            .expect_err("R9 is not on the root sheet")
            .to_string();
        assert!(
            error.starts_with("Component 'R9' not found on this sheet"),
            "{error}"
        );
        assert!(error.contains(cell.to_str().unwrap()), "{error}");
        assert!(error.contains("address it as 'R7'"), "{error}");
    }

    #[tokio::test]
    async fn component_nets_use_each_pins_own_unit_placement() {
        let result = call("get_component_nets", json!({ "reference": "U1" })).await;
        let pins = result["pins"].as_array().unwrap();
        assert_eq!(pins.len(), 9, "ECC83 must expose all three units: {pins:?}");

        let pin_four = pins
            .iter()
            .find(|pin| pin["pin"] == "4")
            .expect("heater pin 4");
        let pin_x = pin_four["x"].as_f64().unwrap();
        let pin_y = pin_four["y"].as_f64().unwrap();
        assert!((pin_x - 60.96).abs() < 1e-9, "wrong unit x: {pin_x}");
        assert!((pin_y - 86.36).abs() < 1e-9, "wrong unit y: {pin_y}");
    }

    #[tokio::test]
    async fn net_components_ignore_pins_from_unplaced_units() {
        // Applying the heater unit's pin 5 geometry to unit 2's placement
        // invents a pin at (160.02, 119.38); no placed ECC83 unit owns it.
        let phantom_label =
            "\t(label \"PHANTOM_TEST\" (at 160.02 119.38 0) (uuid \"00000000-0000-4000-8000-000000000183\"))\n";
        let content = ECC83.replacen(
            "\t(sheet_instances",
            &format!("{phantom_label}\t(sheet_instances"),
            1,
        );
        assert_ne!(content, ECC83, "fixture insertion point changed");

        let result = call_content(
            &content,
            "get_net_components",
            json!({ "net": "PHANTOM_TEST" }),
        )
        .await;
        assert!(
            result["components"]
                .as_array()
                .unwrap()
                .iter()
                .all(|component| component["reference"] != "U1"),
            "unit 2 must not invent heater pin 5: {result}"
        );
    }

    #[tokio::test]
    async fn connected_items_include_nets_from_every_placed_unit() {
        let heater_label =
            "\t(label \"HEATER_TEST\" (at 66.04 86.36 0) (uuid \"00000000-0000-4000-8000-000000000184\"))\n";
        let content = ECC83.replacen(
            "\t(sheet_instances",
            &format!("{heater_label}\t(sheet_instances"),
            1,
        );
        assert_ne!(content, ECC83, "fixture insertion point changed");

        let result = call_content(
            &content,
            "get_connected_items",
            json!({ "reference": "U1" }),
        )
        .await;
        assert!(
            result["nets"]
                .as_array()
                .unwrap()
                .iter()
                .any(|net| net == "HEATER_TEST"),
            "heater unit net missing: {result}"
        );
    }
}

/// A pin's net that no label names, named as KiCad's netlist names it.
///
/// Provenance and KiCad's netlist of the fixture are in
/// `pin_net_names_kicad10.README.md`; every expectation below is that table.
#[cfg(test)]
mod pin_net_name_tests {
    use super::tool_call_support::{call, call_at};
    use super::*;

    const SCH: &str = include_str!("../../tests/fixtures/pin_net_names_kicad10.kicad_sch");
    const CHILD: &str = include_str!("../../tests/fixtures/pin_net_names_sub_kicad10.kicad_sch");

    /// The `net` `get_component_nets` gives `REF.pin`.
    async fn net(sch: &str, pin: &str) -> Option<String> {
        let (reference, number) = pin.split_once('.').unwrap();
        let body = call(sch, "get_component_nets", json!({ "reference": reference })).await;
        let found = body["pins"]
            .as_array()
            .unwrap()
            .iter()
            .find(|entry| entry["pin"] == number)
            .unwrap_or_else(|| panic!("no pin {pin}: {body}"));
        found["net"].as_str().map(str::to_string)
    }

    #[tokio::test]
    async fn a_net_no_label_names_takes_kicads_name_for_it() {
        for (name, pins) in [
            ("Net-(U1-VIN)", &["U1.1", "R1.1"][..]),
            ("Net-(J1-CC1)", &["J1.A5", "R2.1"]),
            ("Net-(J1-D+-PadA6)", &["J1.A6", "R3.1"]),
            (
                "Net-(J1-GND-PadA1)",
                &["J1.A1", "J1.A12", "J1.B1", "J1.B12"],
            ),
            ("Net-(J1-VBUS-PadA4)", &["J1.A4", "J1.A9", "J1.B4", "J1.B9"]),
            ("Net-(U2A-+)", &["U2.3", "R4.1"]),
            ("Net-(R10-Pad1)", &["R9.2", "R10.1"]),
            ("Net-(R7-Pad1)", &["R7.1"]),
            ("unconnected-(R5-Pad2)", &["R5.2", "R6.1"]),
            ("unconnected-(J2-GND-Pad2)", &["J2.[2,5,8,11,16]", "R15.1"]),
            ("Net-(U3-C{slash}B)", &["U3.8", "R19.2"]),
            ("Net-(R14-Pad1)", &["R14.1", "R20.1"]),
            ("Net-(R8-Pad1)", &["R8.1", "R16.1"]),
            ("Net-(#R1-Pad1)", &["#R1.1", "R18.1"]),
            ("Net-(R13-Pad2)", &["R13.2", "R?.1"]),
        ] {
            for pin in pins {
                assert_eq!(net(SCH, pin).await.as_deref(), Some(name), "{pin}");
            }
        }
    }

    /// `null` is a pin KiCad nets with no other, or one whose net the sheet
    /// below names (`/SUB/IN`).
    #[tokio::test]
    async fn a_pin_kicad_nets_alone_has_no_name() {
        for pin in ["J1.B6", "U1.4", "R11.1", "R12.2", "R15.2", "U4.1", "U4.3"] {
            assert_eq!(net(SCH, pin).await, None, "{pin}");
        }
    }

    /// `R?` sorts before `TP1`, so KiCad names their net after `R?`'s pin,
    /// with a hash of its UUID no tool here computes: `null`.
    #[tokio::test]
    async fn a_net_named_after_an_unannotated_pin_has_no_name() {
        for pin in ["R?.2", "TP1.1"] {
            assert_eq!(net(SCH, pin).await, None, "{pin}");
        }
    }

    /// A pin named as it is numbered is written by its number, as
    /// `two_name_nets.README.md`'s `Net-(TP8-Pad1)` is.
    #[tokio::test]
    async fn a_pin_named_its_number_is_written_by_the_number() {
        let sch = include_str!("../../tests/fixtures/two_name_nets.kicad_sch");
        for pin in ["TP8.1", "TP9.1"] {
            assert_eq!(
                net(sch, pin).await.as_deref(),
                Some("Net-(TP8-Pad1)"),
                "{pin}"
            );
        }
    }

    #[tokio::test]
    async fn the_one_pin_tools_give_the_same_name() {
        for tool in ["get_pin_connections", "get_pin_net_name"] {
            let named = call(SCH, tool, json!({ "reference": "R4", "pin_number": "1" })).await;
            assert_eq!(named["net"], "Net-(U2A-+)", "{tool}: {named}");
            let alone = call(SCH, tool, json!({ "reference": "U1", "pin_number": "4" })).await;
            assert!(alone["net"].is_null(), "{tool}: {alone}");
        }
    }

    /// The project's unit notation, here `.` and `1`, as KiCad's Preferences
    /// write it; no Copperpour tool sets it, so the project file is written
    /// here.
    #[tokio::test]
    async fn a_unit_is_written_in_the_projects_notation() {
        let dir = tempfile::tempdir().unwrap();
        let sch = dir.path().join("pin_net_names_kicad10.kicad_sch");
        std::fs::write(&sch, SCH).unwrap();
        std::fs::write(
            dir.path().join("pin_net_names_sub_kicad10.kicad_sch"),
            CHILD,
        )
        .unwrap();
        let project = json!({
            "meta": { "filename": "pin_net_names_kicad10.kicad_pro", "version": 1 },
            "schematic": { "subpart_id_separator": 46, "subpart_first_id": 49 }
        });
        std::fs::write(
            dir.path().join("pin_net_names_kicad10.kicad_pro"),
            project.to_string(),
        )
        .unwrap();

        let body = call_at(
            &sch,
            "get_pin_net_name",
            json!({ "reference": "R4", "pin_number": "1" }),
        )
        .await;
        assert_eq!(body["net"], "Net-(U2.1-+)", "{body}");
    }

    /// A unit's display name, which KiCad's symbol editor writes as
    /// `(unit_name …)` and no Copperpour tool does, so it is written here.
    #[tokio::test]
    async fn a_unit_with_a_display_name_is_written_by_it() {
        let named = SCH.replacen(
            "\t\t\t(symbol \"LM358_1_1\"\n",
            "\t\t\t(symbol \"LM358_1_1\"\n\t\t\t\t(unit_name \"OPA\")\n",
            1,
        );
        assert_ne!(named, SCH, "fixture insertion point changed");
        assert_eq!(net(&named, "R4.1").await.as_deref(), Some("Net-(U2OPA-+)"));
    }

    /// A file older than KiCad 10's format reads a pin named `~` as unnamed.
    /// KiCad 10.0.6's netlist of each fixture names these nets.
    #[tokio::test]
    async fn an_older_file_reads_a_tilde_pin_name_as_none() {
        let off_grid = include_str!("../../tests/fixtures/power_on_off_grid_pin.kicad_sch");
        for pin in ["L1.2", "RESR1.1"] {
            assert_eq!(
                net(off_grid, pin).await.as_deref(),
                Some("Net-(L1-Pad2)"),
                "{pin}"
            );
        }
        let ecc83 = include_str!("../../tests/fixtures/ecc83_multiunit.kicad_sch");
        for (pin, name) in [
            ("R1.1", "Net-(U1A-G)"),
            ("U1.7", "Net-(U1A-G)"),
            ("R2.1", "Net-(U1B-K)"),
            ("P1.2", "Net-(P1-PM)"),
        ] {
            assert_eq!(net(ecc83, pin).await.as_deref(), Some(name), "{pin}");
        }
    }

    /// A hidden `power_in` pin of an ordinary symbol is a global net of its
    /// name even alone: `hidden_power_pin/README.md`'s `HIDDEN` on U99.1.
    #[tokio::test]
    async fn a_hidden_power_pin_names_its_net() {
        let sch = include_str!("../../tests/fixtures/hidden_power_pin/single_pin_nets.kicad_sch");
        assert_eq!(net(sch, "U99.1").await.as_deref(), Some("HIDDEN"));
        assert_eq!(net(sch, "U99.2").await, None);
    }
}

/// The read-only side of net identity, on the real KiCad fixture.
///
/// Provenance, KiCad's own netlist and the ERC precedence statements are in
/// `two_name_nets.README.md`; every expectation below is that table.
#[cfg(test)]
mod two_name_net_tests {
    use super::tool_call_support::{call, call_at, call_result, net_pins};
    use super::*;

    const SCH: &str = include_str!("../../tests/fixtures/two_name_nets.kicad_sch");

    /// Every multi-name net on this sheet answers with the name KiCad prints,
    /// so the five `multiple_net_names` outcomes in the ERC report are all
    /// asserted here. Twenty runs: the old scan returned the first `point_nets`
    /// entry on the net's root, so the answer moved between runs of the same
    /// binary rather than within one.
    #[tokio::test]
    async fn a_multi_name_net_answers_with_kicads_name() {
        // pin, and the name KiCad's netlist gives the net it is on.
        for (reference, pin, expected) in [
            // The five nets carrying more than one name, one per precedence rule.
            ("U1", "8", "+3V3"),   // power symbol over the VCC and ALT labels
            ("U1", "4", "RETURN"), // global label over the GND symbols
            ("R1", "1", "SYS"),    // global over both the +5V symbol and PULLUP
            ("TP2", "1", "AAA"),   // two locals, the lexically first one
            ("TP4", "1", "MIX"),   // local label over the hierarchical one
            // And a single-name net whose two segments are joined by that name.
            ("R1", "2", "SDA"),
        ] {
            for attempt in 0..20 {
                let body = call(
                    SCH,
                    "get_pin_connections",
                    json!({ "reference": reference, "pin_number": pin }),
                )
                .await;
                assert_eq!(
                    body["net"], expected,
                    "{reference}.{pin} on attempt {attempt}: {body}"
                );
            }
        }
    }

    /// `get_connected_items` answers about the component's *nets*: everything
    /// on them, including the parts reachable only by name and the aliases the
    /// net is not called by.
    #[tokio::test]
    async fn connected_items_span_every_segment_and_alias_of_the_net() {
        let items = call(SCH, "get_connected_items", json!({ "reference": "U1" })).await;

        let mut nets: Vec<&str> = items["nets"]
            .as_array()
            .unwrap()
            .iter()
            .map(|net| net.as_str().unwrap())
            .collect();
        nets.sort_unstable();
        assert_eq!(nets, ["+3V3", "RETURN", "SCL", "SDA"], "{items}");

        let components: Vec<&str> = items["connected_components"]
            .as_array()
            .unwrap()
            .iter()
            .map(|component| component["reference"].as_str().unwrap())
            .collect();
        for reachable_by_name_only in ["C1", "C2", "TP1", "TP7", "R1", "R2", "C3"] {
            assert!(
                components.contains(&reachable_by_name_only),
                "{reachable_by_name_only} is on one of U1's nets: {items}"
            );
        }

        let labels: Vec<&str> = items["labels"]
            .as_array()
            .unwrap()
            .iter()
            .map(|label| label["net"].as_str().unwrap())
            .collect();
        // The names these nets are *not* called by are still on them: `VCC` and
        // `ALT` on the rail KiCad calls `+3V3`, `GND` on the one it calls
        // `RETURN`. (`PULLUP` names the pull-up rail, which U1 is not on — its
        // SDA/SCL pins reach the resistors, not the rail behind them.)
        for alias in ["VCC", "ALT", "GND"] {
            assert!(labels.contains(&alias), "alias {alias} dropped: {items}");
        }
    }

    /// `/SDA` is joined only by its two labels: `U1.5` and `R1.2` sit on wire
    /// segments that never meet. KiCad's netlist (`two_name_nets.README.md`)
    /// puts exactly those two pins on it; `U1.6` and `R2.2` are on `/SCL`
    /// beside it, and `R1.1` is on the pull-up rail.
    #[tokio::test]
    async fn net_connections_follow_a_net_joined_only_by_labels() {
        let s = call(SCH, "get_net_connections", json!({ "net": "SDA" })).await;
        assert_eq!(net_pins(&s), ["R1.2", "U1.5"], "{s}");
        assert_eq!(s["label_count"], 2, "{s}");
    }

    /// The rail KiCad calls `+3V3` carries the `VCC` and `ALT` labels too. Its
    /// pins are KiCad's five, without the two `+3V3` power symbols, and asking
    /// by any of its names lists every label on it, not only the ones spelling
    /// the name asked for.
    #[tokio::test]
    async fn net_connections_answer_for_the_whole_net_under_any_of_its_names() {
        for name in ["+3V3", "VCC", "ALT"] {
            let s = call(SCH, "get_net_connections", json!({ "net": name })).await;
            assert_eq!(
                net_pins(&s),
                ["C1.1", "C2.1", "TP1.1", "TP7.1", "U1.8"],
                "{name}: {s}"
            );
            let mut labels: Vec<String> = s["labels"]
                .as_array()
                .unwrap()
                .iter()
                .map(|l| {
                    format!(
                        "{} {}",
                        l["type"].as_str().unwrap(),
                        l["net"].as_str().unwrap()
                    )
                })
                .collect();
            labels.sort_unstable();
            assert_eq!(
                labels,
                [
                    "NetLabel ALT",
                    "NetLabel ALT",
                    "NetLabel VCC",
                    "PowerSymbol +3V3",
                    "PowerSymbol +3V3"
                ],
                "{name}: {s}"
            );
            assert_eq!(s["label_count"], 5, "{name}: {s}");
        }
    }

    /// Ground here is named by a `RETURN` global label over `GND` symbols.
    /// Asked for as `GND`, it is still KiCad's four pins and none of the
    /// `#PWR` symbols that name it.
    #[tokio::test]
    async fn net_connections_leave_out_the_power_symbols_naming_a_net() {
        let s = call(SCH, "get_net_connections", json!({ "net": "GND" })).await;
        assert_eq!(net_pins(&s), ["C1.2", "C2.2", "C3.2", "U1.4"], "{s}");
    }

    /// A net with no label at all is still a net. Collecting by name left it
    /// out of the point set entirely, so the wire and the part at its other end
    /// went unreported.
    #[tokio::test]
    async fn an_unnamed_net_reports_its_wire_and_the_part_at_the_far_end() {
        let items = call(SCH, "get_connected_items", json!({ "reference": "TP8" })).await;

        assert_eq!(items["connected_wires"], 1, "{items}");
        let components: Vec<&str> = items["connected_components"]
            .as_array()
            .unwrap()
            .iter()
            .map(|component| component["reference"].as_str().unwrap())
            .collect();
        assert_eq!(components, ["TP9"], "{items}");
    }

    const NAMED_NET_TOOLS: [&str; 3] = [
        "get_net_connections",
        "get_net_connectivity",
        "get_net_components",
    ];

    /// A name no label or power symbol on the sheet spells is refused, not
    /// answered as a net with nothing on it: a typo, KiCad's netlist name for
    /// the local `SDA` net, the same in another case, and the name KiCad's
    /// netlist gives the `TP8`–`TP9` net, which nothing on the sheet names.
    #[tokio::test]
    async fn a_name_nothing_on_the_sheet_spells_is_refused() {
        for tool in NAMED_NET_TOOLS {
            for (net, offered) in [
                ("NOPE", ""),
                ("/SDA", " (did you mean 'SDA'?)"),
                ("sda", " (did you mean 'SDA'?)"),
                ("Net-(TP8-Pad1)", ""),
            ] {
                let result = call_result(SCH, tool, json!({ "net": net })).await;
                assert!(result.is_error, "{tool} answered '{net}' as a net");
                let crate::mcp::protocol::ToolContent::Text { text } = &result.content[0] else {
                    panic!("expected text content");
                };
                let body: serde_json::Value = serde_json::from_str(text).unwrap();
                assert_eq!(body["error"]["kind"], "invalid_argument", "{tool}: {body}");
                assert_eq!(body["error"]["field"], "net", "{tool}: {body}");
                // The name offered, or nothing, comes straight after the name.
                let reason = body["error"]["reason"].as_str().unwrap();
                assert!(
                    reason.starts_with(&format!(
                        "no label or power symbol on this sheet names net '{net}'{offered}. \
                         list_schematic_nets lists the names on this sheet;"
                    )),
                    "{tool}: {body}"
                );
                assert_eq!(
                    body["message"],
                    format!("Argument 'net' is invalid: {reason}"),
                    "{tool}"
                );
            }
        }
    }

    /// Every name on the sheet still answers: the label names, the aliases a
    /// net is not called by among them, and the power symbols' names.
    #[tokio::test]
    async fn every_name_on_the_sheet_is_answered() {
        let listed = call(SCH, "list_schematic_nets", json!({})).await;
        assert_eq!(
            listed["nets"],
            json!([
                "+3V3", "+5V", "AAA", "ALT", "GND", "MIX", "MIX_H", "PULLUP", "RETURN", "SCL",
                "SDA", "SYS", "VCC", "ZZZ"
            ]),
            "{listed}"
        );
        for tool in NAMED_NET_TOOLS {
            for name in listed["nets"].as_array().unwrap() {
                call(SCH, tool, json!({ "net": name })).await;
            }
        }
    }

    /// The 15 labels as Eeschema serialized them, read from the fixture text
    /// (see the README), not from this crate's parser.
    const KICAD_LABELS: [(&str, &str, &str); 15] = [
        ("NetLabel", "AAA", "06aae51d-ad92-45a3-ab6b-a2934ec12f2a"),
        ("NetLabel", "ALT", "15de34f4-cedd-40a3-9159-f3eff2ac13bb"),
        ("NetLabel", "SDA", "35064194-a7cb-4fcc-b60a-539f8e03bbbc"),
        ("NetLabel", "SCL", "3893093c-b550-492c-80fd-10739f176fb5"),
        ("NetLabel", "ZZZ", "5990e890-7fcd-4caf-ac60-269b1b5f7d3e"),
        ("NetLabel", "ALT", "849c7341-fc4a-4c95-a59f-68f8994893f5"),
        ("NetLabel", "SDA", "8a222e21-42d9-4588-bafd-c4947977eed0"),
        ("NetLabel", "PULLUP", "9724b793-eefa-47df-a8b9-9c0dc04b968f"),
        ("NetLabel", "MIX", "a102cfc5-873d-4e3d-b1ba-a697aa9366ca"),
        ("NetLabel", "VCC", "ab408dff-d6ba-43fb-abfa-bfff6771323d"),
        ("NetLabel", "ZZZ", "b6d04cca-e63c-4dab-aa7b-48bc7b576c07"),
        ("NetLabel", "SCL", "f7a3ae8e-998e-4051-990b-eda340b2e7de"),
        ("GlobalLabel", "SYS", "821185fe-9fdd-4020-a37c-7b17c7a5d1ef"),
        (
            "GlobalLabel",
            "RETURN",
            "c8e4d402-48cc-4823-9cc2-f7275f5e2973",
        ),
        (
            "HierarchicalLabel",
            "MIX_H",
            "f9df2d51-8230-4a59-b15b-468f176904ac",
        ),
    ];

    fn label_rows(body: &serde_json::Value) -> Vec<(&str, &str, &str)> {
        let mut rows: Vec<_> = body["labels"]
            .as_array()
            .unwrap()
            .iter()
            .map(|l| {
                let field = |k: &str| l[k].as_str().unwrap_or_default();
                (field("type"), field("net"), field("uuid"))
            })
            .collect();
        rows.sort_unstable();
        rows
    }

    /// Each label carries the UUID KiCad gave it, for all three label kinds.
    #[tokio::test]
    async fn listed_labels_carry_kicads_uuids() {
        let body = call(SCH, "list_schematic_labels", json!({})).await;

        let mut expected = KICAD_LABELS.to_vec();
        expected.sort_unstable();
        assert_eq!(label_rows(&body), expected, "{body}");
    }

    /// The listed UUIDs are what `batch_delete` takes: one call removes every
    /// label and nothing else.
    #[tokio::test]
    async fn listed_label_uuids_feed_batch_delete() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("two_name_nets.kicad_sch");
        std::fs::write(&path, SCH).unwrap();

        let listed = call_at(&path, "list_schematic_labels", json!({})).await;
        let uuids: Vec<&str> = label_rows(&listed).into_iter().map(|(_, _, u)| u).collect();
        let wires_before = call_at(&path, "list_schematic_wires", json!({})).await["count"].clone();

        let deleted = call_at(&path, "batch_delete", json!({ "uuids": uuids })).await;
        let mut deleted_uuids: Vec<&str> = deleted["deleted_item_uuids"]
            .as_array()
            .unwrap()
            .iter()
            .map(|u| u.as_str().unwrap())
            .collect();
        deleted_uuids.sort_unstable();
        let mut requested = uuids.clone();
        requested.sort_unstable();
        assert_eq!(deleted_uuids, requested, "{deleted}");
        assert_eq!(deleted["deleted_components"], json!([]), "{deleted}");
        assert_eq!(deleted["removed_no_connects_count"], 0, "{deleted}");
        assert_eq!(deleted["junctions_pruned_count"], 0, "{deleted}");
        assert_eq!(deleted["errors"], json!([]), "{deleted}");

        let after = call_at(&path, "list_schematic_labels", json!({})).await;
        assert_eq!(after["count"], 0, "{after}");
        let wires_after = call_at(&path, "list_schematic_wires", json!({})).await["count"].clone();
        assert_eq!(wires_after, wires_before);
    }

    /// Every label entry the four net queries return for the sheet at `path`,
    /// with the tool and the net name asked for: `get_net_connections` and
    /// `get_net_connectivity` for every name on the sheet,
    /// `get_connected_items` for U1, R1, TP2 and TP4, whose nets carry every
    /// label, and `trace_from_point` at every label. `ALT` shares its point with a `+3V3`
    /// symbol's pin, so the trace meets a power symbol too.
    async fn net_query_labels(
        path: &std::path::Path,
    ) -> Vec<(&'static str, String, serde_json::Value)> {
        let mut found = Vec::new();
        let nets = call_at(path, "list_schematic_nets", json!({})).await;
        for net in nets["nets"].as_array().unwrap() {
            let net = net.as_str().unwrap();
            for tool in ["get_net_connections", "get_net_connectivity"] {
                let body = call_at(path, tool, json!({ "net": net })).await;
                for label in body["labels"].as_array().unwrap() {
                    found.push((tool, net.to_string(), label.clone()));
                }
            }
        }
        for reference in ["U1", "R1", "TP2", "TP4"] {
            let body = call_at(
                path,
                "get_connected_items",
                json!({ "reference": reference }),
            )
            .await;
            for label in body["labels"].as_array().unwrap() {
                found.push(("get_connected_items", String::new(), label.clone()));
            }
        }
        let labels = call_at(path, "list_schematic_labels", json!({})).await;
        for at in labels["labels"].as_array().unwrap() {
            let body = call_at(
                path,
                "trace_from_point",
                json!({ "x": at["x"], "y": at["y"] }),
            )
            .await;
            for label in body["labels_here"].as_array().unwrap() {
                found.push(("trace_from_point", String::new(), label.clone()));
            }
        }
        found
    }

    /// The net queries give each label the UUID KiCad wrote for it, for all
    /// three kinds, and a power symbol's entry none: its pseudo-label carries
    /// the symbol's UUID.
    #[tokio::test]
    async fn net_queries_report_each_labels_own_uuid() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("two_name_nets.kicad_sch");
        std::fs::write(&path, SCH).unwrap();

        let expected: BTreeSet<(String, String, String)> = KICAD_LABELS
            .iter()
            .map(|&(kind, net, uuid)| (kind.into(), net.into(), uuid.into()))
            .collect();
        let mut rows: std::collections::BTreeMap<&str, BTreeSet<(String, String, String)>> =
            Default::default();
        let mut power: HashMap<&str, usize> = HashMap::new();
        for (tool, asked, label) in net_query_labels(&path).await {
            let kind = label["type"].as_str().unwrap();
            if kind == "PowerSymbol" {
                assert!(label.get("uuid").is_none(), "{tool}: {label}");
                *power.entry(tool).or_default() += 1;
                continue;
            }
            // `get_net_connectivity` lists only labels spelling the name asked
            // for, and leaves the name out.
            let net = label["net"].as_str().unwrap_or(&asked);
            let uuid = label["uuid"]
                .as_str()
                .unwrap_or_else(|| panic!("{tool}: {label}"));
            rows.entry(tool)
                .or_default()
                .insert((kind.into(), net.into(), uuid.into()));
        }
        for tool in [
            "get_net_connections",
            "get_net_connectivity",
            "get_connected_items",
            "trace_from_point",
        ] {
            assert_eq!(rows.get(tool), Some(&expected), "{tool}");
            assert!(power.get(tool).is_some_and(|&n| n > 0), "{tool}: {power:?}");
        }
    }

    /// Every UUID the net queries give a label is what `batch_delete` takes:
    /// one call removes the sheet's 15 labels, and the power symbols, wires
    /// and components stay.
    #[tokio::test]
    async fn net_query_label_uuids_delete_only_labels() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("two_name_nets.kicad_sch");
        std::fs::write(&path, SCH).unwrap();

        let mut uuids: Vec<String> = net_query_labels(&path)
            .await
            .into_iter()
            .filter_map(|(_, _, label)| label["uuid"].as_str().map(String::from))
            .collect();
        uuids.sort_unstable();
        uuids.dedup();
        let wires_before = call_at(&path, "list_schematic_wires", json!({})).await["count"].clone();

        let deleted = call_at(&path, "batch_delete", json!({ "uuids": uuids })).await;
        let mut deleted_uuids: Vec<&str> = deleted["deleted_item_uuids"]
            .as_array()
            .unwrap()
            .iter()
            .map(|u| u.as_str().unwrap())
            .collect();
        deleted_uuids.sort_unstable();
        let mut kicad_uuids: Vec<&str> = KICAD_LABELS.iter().map(|(_, _, u)| *u).collect();
        kicad_uuids.sort_unstable();
        assert_eq!(deleted_uuids, kicad_uuids, "{deleted}");
        assert_eq!(deleted["deleted_components"], json!([]), "{deleted}");
        assert_eq!(deleted["removed_no_connects_count"], 0, "{deleted}");
        assert_eq!(deleted["junctions_pruned_count"], 0, "{deleted}");
        assert_eq!(deleted["errors"], json!([]), "{deleted}");

        let after = call_at(&path, "list_schematic_labels", json!({})).await;
        assert_eq!(after["count"], 0, "{after}");
        let nets_after = call_at(&path, "list_schematic_nets", json!({})).await;
        assert_eq!(
            nets_after["nets"],
            json!(["+3V3", "+5V", "GND"]),
            "{nets_after}"
        );
        let wires_after = call_at(&path, "list_schematic_wires", json!({})).await["count"].clone();
        assert_eq!(wires_after, wires_before);
    }

    /// `get_net_connectivity`'s wires carry the UUID `list_schematic_wires`
    /// lists for the same segment, as `get_connected_items`' wires do.
    #[tokio::test]
    async fn net_connectivity_reports_each_wires_uuid() {
        let listed = call(SCH, "list_schematic_wires", json!({})).await;
        let by_uuid: HashMap<&str, &serde_json::Value> = listed["wires"]
            .as_array()
            .unwrap()
            .iter()
            .map(|wire| (wire["uuid"].as_str().unwrap(), wire))
            .collect();
        let nets = call(SCH, "list_schematic_nets", json!({})).await;
        let mut seen: BTreeSet<String> = BTreeSet::new();
        for net in nets["nets"].as_array().unwrap() {
            let body = call(SCH, "get_net_connectivity", json!({ "net": net })).await;
            for wire in body["wires"].as_array().unwrap() {
                let uuid = wire["uuid"]
                    .as_str()
                    .unwrap_or_else(|| panic!("{net}: {wire}"));
                let same = by_uuid
                    .get(uuid)
                    .unwrap_or_else(|| panic!("{net}: {wire} is not listed"));
                for end in ["x1", "y1", "x2", "y2"] {
                    assert_eq!(wire[end].as_f64(), same[end].as_f64(), "{net}: {wire}");
                }
                seen.insert(uuid.to_string());
            }
        }
        // Every wire but the one on the net no label names, TP8 to TP9.
        assert_eq!(seen.len(), by_uuid.len() - 1, "{listed}");
    }
}

/// What `trace_from_point` reports as being at a point, on the real KiCad
/// fixture (#539). Wires and labels were already answered; a pin and a junction
/// dot were not, and there was no empty key to show they had been skipped.
#[cfg(test)]
mod trace_from_point_tests {
    use super::tool_call_support::{call, call_result};
    use super::*;

    const SCH: &str = include_str!("../../tests/fixtures/trace_point_kicad10.kicad_sch");

    async fn at(x: f64, y: f64) -> serde_json::Value {
        call(SCH, "trace_from_point", json!({ "x": x, "y": y })).await
    }

    async fn at_within(x: f64, y: f64, tolerance: serde_json::Value) -> serde_json::Value {
        call(
            SCH,
            "trace_from_point",
            json!({ "x": x, "y": y, "tolerance": tolerance }),
        )
        .await
    }

    /// The references and pin numbers reported at a point, sorted, so the
    /// assertion does not depend on the order symbols sit in the file.
    fn pins(trace: &serde_json::Value) -> Vec<String> {
        let mut found: Vec<String> = trace["pins_here"]
            .as_array()
            .expect("pins_here is always present")
            .iter()
            .map(|pin| {
                format!(
                    "{}.{}",
                    pin["reference"].as_str().unwrap(),
                    pin["pin"].as_str().unwrap()
                )
            })
            .collect();
        found.sort();
        found
    }

    /// How many entries a `*_here` key carries. Every one of them is always
    /// present, so an absent key is a failure rather than a zero.
    fn count(trace: &serde_json::Value, key: &str) -> usize {
        trace[key]
            .as_array()
            .unwrap_or_else(|| panic!("{key} is always present: {trace}"))
            .len()
    }

    /// The filed case: `R1` pin 1, two wire ends, a junction dot and net `NETA`
    /// all at `(100.33, 96.52)`. The net and the wires were already right; the
    /// pin and the dot were the two the response dropped.
    #[tokio::test]
    async fn the_pin_and_the_dot_at_a_point_are_both_reported() {
        let trace = at(100.33, 96.52).await;

        assert_eq!(trace["net"], "NETA", "{trace}");
        assert_eq!(count(&trace, "wires_here"), 2, "{trace}");
        assert_eq!(pins(&trace), ["R1.1"], "{trace}");
        assert_eq!(count(&trace, "junctions_here"), 1, "{trace}");
    }

    /// A reported pin carries enough to act on without a second call.
    #[tokio::test]
    async fn a_reported_pin_names_itself_and_its_position() {
        let trace = at(100.33, 96.52).await;
        let pin = &trace["pins_here"][0];

        assert_eq!(pin["reference"], "R1", "{trace}");
        assert_eq!(pin["pin"], "1", "{trace}");
        assert_eq!(pin["electrical_type"], "passive", "{trace}");
        assert_eq!(pin["x"], 100.33, "{trace}");
        assert_eq!(pin["y"], 96.52, "{trace}");
    }

    /// `R1` pin 2, wired but with no dot on it. A pin must not imply a junction.
    #[tokio::test]
    async fn a_pin_without_a_dot_reports_no_junction() {
        let trace = at(100.33, 104.14).await;

        assert_eq!(pins(&trace), ["R1.2"], "{trace}");
        assert_eq!(count(&trace, "junctions_here"), 0, "{trace}");
        assert_eq!(count(&trace, "wires_here"), 1, "{trace}");
    }

    /// A branch landing mid-span: the dot `add_wire` inserts, with no pin under
    /// it. A junction must not imply a pin either.
    #[tokio::test]
    async fn a_dot_without_a_pin_reports_no_pin() {
        let trace = at(130.81, 121.92).await;

        assert_eq!(count(&trace, "junctions_here"), 1, "{trace}");
        assert!(pins(&trace).is_empty(), "{trace}");
        assert_eq!(count(&trace, "wires_here"), 2, "{trace}");
    }

    /// `R2` pin 2 and `R3` pin 1 sit on one point with no wire between them —
    /// KiCad's own netlist resolves them as the single net `Net-(R2-Pad2)`.
    /// Reporting one of them would be the same defect in a smaller form.
    #[tokio::test]
    async fn both_of_two_stacked_pins_are_reported() {
        let trace = at(160.02, 104.14).await;

        assert_eq!(pins(&trace), ["R2.2", "R3.1"], "{trace}");
    }

    /// Both units of `U1` are placed, 30mm apart. Reading the pins off the
    /// library symbol rather than the placed unit would put unit 1's pins on
    /// unit 2's point (#35).
    #[tokio::test]
    async fn a_point_on_one_unit_reports_only_that_unit() {
        let unit_two = at(198.12, 130.81).await;
        assert_eq!(pins(&unit_two), ["U1.7"], "{unit_two}");

        let unit_one = at(198.12, 100.33).await;
        assert_eq!(pins(&unit_one), ["U1.1"], "{unit_one}");
    }

    /// The omission was invisible because no key was there to be empty. On bare
    /// paper all four lists are present and empty.
    #[tokio::test]
    async fn a_bare_point_answers_with_four_empty_lists() {
        let trace = at(210.0, 50.0).await;

        assert_eq!(trace["net"], serde_json::Value::Null, "{trace}");
        for key in ["wires_here", "labels_here", "pins_here", "junctions_here"] {
            assert_eq!(count(&trace, key), 0, "{key} should be empty: {trace}");
        }
    }

    /// A tolerance no coincidence test can pass used to be accepted, and the
    /// response then asserted `net: "NETA"` at a point it reported as bare:
    /// the net comes from the shared graph, which resolves at its own fixed
    /// tolerance and never saw the argument. Empty evidence and a named net in
    /// one successful answer is the contradiction; refusing the argument is the
    /// fix. `0` is refused too — a point has no zero-width neighbourhood.
    #[tokio::test]
    async fn an_unusable_tolerance_is_refused_not_answered_empty() {
        for tolerance in [json!(0.0), json!(-1.0), json!("wide")] {
            let result = call_result(
                SCH,
                "trace_from_point",
                json!({ "x": 100.33, "y": 96.52, "tolerance": tolerance }),
            )
            .await;

            assert!(result.is_error, "accepted tolerance {tolerance}");
            let crate::mcp::protocol::ToolContent::Text { text } = &result.content[0] else {
                panic!("expected text content");
            };
            let body: serde_json::Value = serde_json::from_str(text).unwrap();
            assert_eq!(body["error"]["kind"], "invalid_argument", "{body}");
            assert_eq!(body["error"]["field"], "tolerance", "{body}");
            assert!(body.get("net").is_none(), "a refusal names no net: {body}");
        }
    }

    /// The refusal is of unusable values, not of the argument: a positive
    /// tolerance still answers, and the filed point answers the same.
    #[tokio::test]
    async fn a_positive_tolerance_is_still_accepted() {
        let trace = at_within(100.33, 96.52, json!(0.05)).await;

        assert_eq!(trace["net"], "NETA", "{trace}");
        assert_eq!(pins(&trace), ["R1.1"], "{trace}");
        assert_eq!(count(&trace, "junctions_here"), 1, "{trace}");
    }

    /// Pins and dots are read from `ConnectivityIndex` at the tolerance the
    /// caller asked for, so the same probe 0.02 mm off `R1` pin 1 finds it at
    /// `0.05` and does not at `0.01`. A scan that hardcoded a tolerance, or an
    /// index built at one while the probe used another, fails one of these.
    #[tokio::test]
    async fn the_requested_tolerance_governs_the_pins_and_dots_reported() {
        let wide = at_within(100.35, 96.52, json!(0.05)).await;
        assert_eq!(pins(&wide), ["R1.1"], "{wide}");
        assert_eq!(count(&wide, "junctions_here"), 1, "{wide}");

        let tight = at_within(100.35, 96.52, json!(0.01)).await;
        assert!(pins(&tight).is_empty(), "{tight}");
        assert_eq!(count(&tight, "junctions_here"), 0, "{tight}");
    }
}
