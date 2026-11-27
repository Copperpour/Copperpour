//! Read-only advice on the interface of one explicitly selected block.

use super::sch_connectivity::{
    add_symbol_power_drivers, exact_drawn_net_graph, NetGraph, NetRoot, SymbolPower,
};
use super::sch_label_rename::{is_bus_name, stored_name, unescaped};
use super::sch_layout_score::read_saved_sheet;
use super::{get_path, invalid_arg, is_power_symbol_reference, ToolDef};
use crate::{
    mcp::{error::ToolErrorKind, protocol::CallToolResult},
    tool,
};
use copperpour_sexp::schematic::{
    extract_labels, extract_wires, find_lib_symbol, lib_pin_nodes_for_body_style,
    parse_subsymbol_unit, LabelKind,
};
use copperpour_sexp::SexpNode;
use serde::Serialize;
use serde_json::{json, Value};
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

// Advisory drawing distance only. This says nothing about PCB placement.
const NEARBY_MM: f64 = 25.4;

pub(super) fn tool_def() -> ToolDef {
    tool!(
        "plan_schematic_split",
        "Read-only. Measure the interface of a block of symbols on one sheet, as if it moved \
         to a child sheet. Report nets it would hide, boundary pins and drawn objects to \
          review, suggested sheet pins, and supporting parts that may be left behind. \
         This is advice from one saved sheet, not a transfer plan or a project-wide netlist. \
         Writes nothing.",
        json!({
            "type": "object", "additionalProperties": false,
            "properties": {
                "schematic": { "type": "string", "description": "Path to the saved .kicad_sch sheet that holds the block" },
                "symbols": {
                    "type": "array", "minItems": 1, "uniqueItems": true,
                    "items": {
                        "type": "object", "additionalProperties": false,
                        "properties": {
                            "uuid": {"type": "string", "minLength": 1},
                            "unit": {"type": "integer", "minimum": 1, "maximum": u32::MAX}
                        },
                        "required": ["uuid", "unit"]
                    },
                    "description": "Placed symbol UUIDs and units to consider moving. Exclude power symbols."
                }
            },
            "required": ["schematic", "symbols"]
        }),
        |args, _ctx| async move { handle_plan_schematic_split(args) }
    )
}

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
struct SymbolId<'a> {
    uuid: &'a str,
    unit: u32,
}

#[derive(Clone, Serialize, PartialEq, Eq, PartialOrd, Ord)]
struct PinRef {
    reference: String,
    symbol_uuid: String,
    unit: u32,
    number: String,
    ordinal: usize,
    #[serde(skip)]
    name: String,
    #[serde(skip)]
    electrical_type: String,
}

impl PinRef {
    fn label(&self) -> String {
        format!("{}.{}", self.reference, self.number)
    }
}

#[derive(Default)]
struct NetSides {
    inside: Vec<PinRef>,
    outside: Vec<PinRef>,
    /// A hierarchical label or sheet pin: the net already leaves this sheet.
    leaves_sheet: bool,
}

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "snake_case")]
enum Scope {
    Local,
    Hierarchical,
    Global,
    LocalPower,
    GlobalPower,
}

fn rail(scopes: &BTreeSet<(Scope, String)>) -> bool {
    scopes
        .iter()
        .any(|(scope, _)| matches!(scope, Scope::LocalPower | Scope::GlobalPower))
}

struct Placed {
    pin: PinRef,
    inside: bool,
    net: NetRoot,
    island: NetRoot,
    pin_at: (f64, f64),
}

fn supporting_parts(
    pins: &[Placed],
    drawn_wires: &BTreeSet<NetRoot>,
    exits: &BTreeSet<NetRoot>,
    nets: &NetGraph,
    scopes: &BTreeMap<NetRoot, BTreeSet<(Scope, String)>>,
) -> (Vec<Value>, Vec<String>) {
    let mut outside = BTreeMap::<(&str, &str), Vec<&Placed>>::new();
    let mut islands = BTreeMap::<NetRoot, Vec<&Placed>>::new();
    let mut power_inputs = BTreeMap::<NetRoot, Vec<&Placed>>::new();
    for p in pins {
        islands.entry(p.island).or_default().push(p);
        if !p.inside {
            outside
                .entry((&p.pin.reference, &p.pin.symbol_uuid))
                .or_default()
                .push(p);
            if p.pin.electrical_type == "power_in" {
                power_inputs.entry(p.net).or_default().push(p);
            }
        }
    }
    let mut suggestions = Vec::new();
    let mut stranded = Vec::new();
    for ((reference, symbol_uuid), terminals) in outside {
        if terminals.len() != 2 || terminals.iter().any(|p| p.pin.electrical_type != "passive") {
            continue;
        }
        let mut shared = Vec::new();
        let mut retained_nearby = BTreeSet::new();
        let mut other_rail_consumers = BTreeSet::new();
        let mut retained_port = false;
        for p in terminals {
            let mut block_pins = BTreeSet::new();
            retained_port |= exits.contains(&p.island);
            for peer in &islands[&p.island] {
                if peer.inside {
                    block_pins.insert(peer.pin.label());
                } else if peer.pin.symbol_uuid != symbol_uuid
                    && (peer.pin_at.0 - p.pin_at.0).hypot(peer.pin_at.1 - p.pin_at.1) <= NEARBY_MM
                {
                    retained_nearby.insert(peer.pin.label());
                }
            }
            if !block_pins.is_empty() && drawn_wires.contains(&p.island) {
                shared.push(json!({
                    "pin": p.pin,
                    "net": nets.name_of_root(p.net),
                    "block_pins": block_pins,
                }));
            }
            // Alternatives, not membership. A shared rail alone cannot say
            // which load this part serves, even if that load is close by.
            if scopes.get(&p.net).is_some_and(rail) {
                for peer in power_inputs.get(&p.net).into_iter().flatten() {
                    if peer.island != p.island {
                        other_rail_consumers.insert(peer.pin.label());
                    }
                }
            }
        }
        if shared.is_empty() {
            continue;
        }
        shared.sort_by(|a, b| {
            a["pin"]["number"]
                .as_str()
                .cmp(&b["pin"]["number"].as_str())
        });
        let would_be_stranded = retained_nearby.is_empty() && !retained_port;
        if would_be_stranded {
            stranded.push(reference.to_owned());
        }
        suggestions.push(json!({
            "reference": reference,
            "symbol_uuid": symbol_uuid,
            "shared_drawn_wires": shared,
            "would_be_stranded": would_be_stranded,
            "retained_nearby_pins": retained_nearby,
            "retained_port": retained_port,
            "other_rail_consumers": other_rail_consumers,
            "options": [
                "include_with_block",
                "keep_and_place_near_the_part_it_serves"
            ]
        }));
    }
    (suggestions, stranded)
}

fn drives(t: &str) -> bool {
    matches!(
        t,
        "output" | "tri_state" | "open_collector" | "open_emitter" | "power_out" | "bidirectional"
    )
}

/// The sheet pin shape seen from the child: what the block does to the net.
fn shape(inside: &[PinRef], outside: &[PinRef]) -> &'static str {
    let any =
        |pins: &[PinRef], f: &dyn Fn(&str) -> bool| pins.iter().any(|p| f(&p.electrical_type));
    if any(inside, &|t| t == "bidirectional") {
        return "bidirectional";
    }
    match (any(inside, &drives), any(outside, &drives)) {
        (true, true) => "unresolved",
        (true, false) => "output",
        (false, true) => "input",
        (false, false) => "passive",
    }
}

fn suggested_pin_name(label: Option<&str>, sides: &NetSides) -> (String, &'static str) {
    if let Some(name) = label {
        return (name.to_owned(), "label");
    }
    // An IC's pin name says what the signal is; a connector's `Pin_3` does not.
    let named = |pins: &[PinRef], active: bool| {
        pins.iter()
            .filter(|p| !active || p.electrical_type != "passive")
            // Also excludes empty pin names and KiCad's "~" placeholder.
            .find(|p| p.name.chars().any(char::is_alphabetic))
            .map(|p| p.name.clone())
    };
    if let Some(name) = named(&sides.inside, true)
        .or_else(|| named(&sides.outside, true))
        .or_else(|| named(&sides.inside, false))
        .or_else(|| named(&sides.outside, false))
    {
        return (name, "pin_name");
    }
    // Only a net with selected pins reaches the boundary report.
    let first = &sides.inside[0];
    (format!("{}_{}", first.reference, first.number), "generated")
}

fn uuid_of(node: &SexpNode) -> Option<String> {
    node.find_str("uuid").map(str::to_owned)
}

fn at_of(node: &SexpNode) -> Option<(f64, f64)> {
    let at = node.find("at")?;
    let (x, y) = (at.get_f64(1)?, at.get_f64(2)?);
    (x.is_finite() && y.is_finite()).then_some((x, y))
}

fn blocked(reason: impl Into<String>) -> CallToolResult {
    let reason = reason.into();
    CallToolResult::error_kind(
        ToolErrorKind::PlanBlocked {
            operation: "plan_schematic_split".into(),
            reasons: vec![reason.clone()],
        },
        reason,
    )
}

fn stale(target: impl Into<String>, reason: impl Into<String>) -> CallToolResult {
    let reason = reason.into();
    CallToolResult::error_kind(
        ToolErrorKind::StaleTarget {
            target: target.into(),
            reason: reason.clone(),
        },
        reason,
    )
}

fn ambiguous(target: impl Into<String>, candidates: Vec<String>) -> CallToolResult {
    let target = target.into();
    let message = format!("Cannot identify one {target}: {}", candidates.join(", "));
    CallToolResult::error_kind(
        ToolErrorKind::AmbiguousTarget { target, candidates },
        message,
    )
}

fn instance_path(path: &Path, source: &str) -> Result<String, CallToolResult> {
    let schematic = copperpour_schematic_editor::Schematic::from_source(path, source)
        .map_err(|e| blocked(format!("Cannot read saved instance metadata: {e}")))?;
    let Some(uuid) = schematic.uuid.as_deref() else {
        return Err(stale(
            path.display().to_string(),
            "The sheet has no saved UUID.",
        ));
    };
    let instance = super::saved_sheet_instance_context(path, source, uuid)
        .map_err(super::SchematicTargetError::into_tool_result)?;
    if instance.instance_paths.len() != 1 {
        let mut candidates = instance.instance_paths;
        // The resolver sorts paths and preserves repeated placements. Keep the
        // refusal even when there is only one distinct path to display.
        candidates.dedup();
        let message = format!(
            "Cannot identify one saved sheet instance: multiple placements found at {}. \
             Check for reused sheets or duplicate placement UUIDs.",
            candidates.join(", ")
        );
        return Err(CallToolResult::error_kind(
            ToolErrorKind::AmbiguousTarget {
                target: "saved sheet instance".into(),
                candidates,
            },
            message,
        ));
    }
    super::validate_sheet_instance_state(path, &schematic, &instance)
        .map_err(super::SchematicTargetError::into_tool_result)?;
    Ok(instance.instance_path)
}

fn uuid_key(uuid: &str) -> std::borrow::Cow<'_, str> {
    match uuid::Uuid::parse_str(uuid) {
        Ok(uuid) => uuid.hyphenated().to_string().into(),
        Err(_) => uuid.into(),
    }
}

fn selected_symbols(
    requested: &[SymbolId<'_>],
    placed: &super::PlacedPinsByReference,
) -> Result<BTreeSet<String>, CallToolResult> {
    let mut by_uuid = BTreeMap::new();
    for (inst, _) in placed {
        let Some(uuid) = inst.uuid.as_deref() else {
            return Err(blocked(format!("{} has no placed UUID.", inst.reference)));
        };
        if let Some(other) = by_uuid.insert(uuid_key(uuid), inst) {
            return Err(ambiguous(
                format!("placed UUID '{uuid}'"),
                vec![other.reference.clone(), inst.reference.clone()],
            ));
        }
    }
    let mut selected = BTreeSet::new();
    let mut references = BTreeSet::new();
    for (i, id) in requested.iter().enumerate() {
        let Some(inst) = by_uuid.get(&uuid_key(id.uuid)) else {
            return Err(stale(
                id.uuid,
                format!("symbols[{i}].uuid is not on the saved sheet."),
            ));
        };
        if inst.unit != id.unit {
            return Err(stale(
                id.uuid,
                format!("symbols[{i}].unit is {}, not {}.", inst.unit, id.unit),
            ));
        }
        if is_power_symbol_reference(&inst.reference) {
            return Err(invalid_arg(
                &format!("symbols[{i}].uuid"),
                "select a part, not a power symbol",
            ));
        }
        selected.insert(inst.uuid.as_ref().expect("indexed placed UUID").clone());
        references.insert(&inst.reference);
    }
    let mut units = BTreeMap::new();
    for (inst, _) in placed {
        // Unannotated references are placeholders, except when we need to
        // reason about whether all units of a selected part were selected.
        if is_power_symbol_reference(&inst.reference)
            || (inst.reference.ends_with('?') && !references.contains(&inst.reference))
        {
            continue;
        }
        let uuid = inst.uuid.as_deref().expect("indexed placed UUID");
        if let Some(other) = units.insert((&inst.reference, inst.unit), uuid) {
            return Err(ambiguous(
                format!("placed unit {} of {}", inst.unit, inst.reference),
                vec![other.to_owned(), uuid.to_owned()],
            ));
        }
    }
    for (inst, _) in placed {
        if references.contains(&inst.reference) && !selected.contains(inst.uuid.as_deref().unwrap())
        {
            return Err(blocked(format!(
                "Select every placed unit of {} together.",
                inst.reference
            )));
        }
    }
    Ok(selected)
}

fn handle_plan_schematic_split(args: &Value) -> anyhow::Result<CallToolResult> {
    let path = get_path(args, "schematic")?;
    let result = (|| {
        let requested = read_selection(args)?;
        let (source, tree) = read_saved_sheet(&path)?;
        let instance_path = instance_path(&path, &source)?;
        let mut report = split_report(&tree, &requested)?;
        report["schematic"] = json!(path);
        report["source"] = json!("saved_file");
        report["sheet_instance_path"] = json!(instance_path);
        report["revision"] = json!(crate::hex::sha256_hex(source.as_bytes()));
        Ok(CallToolResult::json(&report))
    })();
    Ok(result.unwrap_or_else(|e| e))
}

fn read_selection(args: &Value) -> Result<Vec<SymbolId<'_>>, CallToolResult> {
    let entries = super::require_array(args, "symbols")?;
    if entries.is_empty() {
        return Err(invalid_arg("symbols", "select at least one placed symbol"));
    }
    entries
        .iter()
        .enumerate()
        .map(|(i, entry)| {
            let uuid = entry["uuid"]
                .as_str()
                .filter(|uuid| !uuid.is_empty())
                .ok_or_else(|| {
                    invalid_arg(&format!("symbols[{i}].uuid"), "expected a placed UUID")
                })?;
            let unit = super::opt_u32(entry, "unit")
                .ok()
                .flatten()
                .filter(|&unit| unit > 0)
                .ok_or_else(|| {
                    invalid_arg(&format!("symbols[{i}].unit"), "expected a positive unit")
                })?;
            Ok(SymbolId { uuid, unit })
        })
        .collect()
}

type PinAlternates<'a> = BTreeMap<&'a str, &'a str>;

fn library_has_pins(node: &SexpNode) -> bool {
    node.find("pin").is_some() || node.find_all("symbol").into_iter().any(library_has_pins)
}

fn validate_placed_pin_geometry(
    node: &SexpNode,
    lib: &SexpNode,
    inst: &copperpour_sexp::schematic::SymbolInstance,
    resolved: &super::UnitPins,
) -> Result<(), CallToolResult> {
    let saved: BTreeSet<_> = node
        .find_all("pin")
        .into_iter()
        .map(|pin| pin.get(1).and_then(SexpNode::as_str).unwrap_or_default())
        .collect();
    let resolved: BTreeSet<_> = resolved
        .iter()
        .map(|(pin, _)| pin.number.as_str())
        .collect();
    // Older saves can omit the placed pin records. In that case a nonempty
    // embedded inventory is still useful, but an unmatched body is not pinless.
    if (resolved.is_empty() && library_has_pins(lib))
        || (!saved.is_empty() && !resolved.is_subset(&saved))
        || resolved.contains("")
    {
        return Err(blocked(format!(
            "Cannot resolve the saved symbol pin geometry on {}.",
            inst.reference
        )));
    }
    // KiCad can save records for every unit. Extra records must still have
    // a definition in the saved body style; otherwise a real pin may be lost.
    let mut extra: BTreeSet<_> = saved.difference(&resolved).copied().collect();
    if !extra.is_empty() {
        let style = super::symbol_body_style(node);
        let units: BTreeSet<_> = lib
            .find_all("symbol")
            .into_iter()
            .filter_map(|child| parse_subsymbol_unit(child.get(1)?.as_str()?))
            .collect();
        for pin in units
            .into_iter()
            .flat_map(|unit| lib_pin_nodes_for_body_style(lib, unit, style))
        {
            if let Some(number) = pin.find_str("number") {
                extra.remove(number);
            }
        }
        if !extra.is_empty() {
            return Err(blocked(format!(
                "Cannot resolve the saved symbol pin geometry on {}.",
                inst.reference
            )));
        }
    }
    Ok(())
}

fn apply_pin_alternates<'a>(
    tree: &'a SexpNode,
    placed: &mut super::PlacedPinsByReference,
    libs: &[&SexpNode],
) -> Result<Vec<PinAlternates<'a>>, CallToolResult> {
    let mut names = Vec::with_capacity(placed.len());
    // drawn_symbol_pins inventories every symbol in file order or fails.
    for (node, (inst, pins)) in tree.find_all("symbol").into_iter().zip(placed) {
        let lib = find_lib_symbol(libs, inst).expect("resolved embedded library symbol");
        validate_placed_pin_geometry(node, lib, inst, pins)?;
        let mut active = BTreeMap::new();
        for pin in node.find_all("pin") {
            let Some(alternate) = pin.find("alternate") else {
                continue;
            };
            let error = || {
                blocked(format!(
                    "Cannot resolve a saved pin alternate on {}.",
                    inst.reference
                ))
            };
            let number = pin.get(1).and_then(SexpNode::as_str).ok_or_else(error)?;
            let name = alternate
                .get(1)
                .and_then(SexpNode::as_str)
                .ok_or_else(error)?;
            if active.insert(number, name).is_some() {
                return Err(error());
            }
        }
        if active.is_empty() {
            names.push(active);
            continue;
        }
        let mut resolved = BTreeSet::new();
        for (node, (pin, _)) in
            lib_pin_nodes_for_body_style(lib, inst.unit, super::symbol_body_style(node))
                .into_iter()
                .zip(pins)
        {
            let Some(&name) = active.get(pin.number.as_str()) else {
                continue;
            };
            let mut alternates = node
                .find_all("alternate")
                .into_iter()
                .filter(|alt| alt.get(1).and_then(SexpNode::as_str) == Some(name));
            let kind = alternates.next().and_then(|alt| alt.get(2)?.as_str());
            let Some(kind) = kind.filter(|_| alternates.next().is_none()) else {
                return Err(blocked(format!(
                    "Cannot resolve pin alternate '{name}' on {}.{}.",
                    inst.reference, pin.number
                )));
            };
            resolved.insert(pin.number.clone());
            // A hidden power input takes its net name from the library pin,
            // even when an alternate changes its displayed name and type.
            pin.electrical_type = kind.to_owned();
        }
        if resolved.len() != active.len() {
            return Err(blocked(format!(
                "Cannot resolve a saved pin alternate on {}.",
                inst.reference
            )));
        }
        names.push(active);
    }
    Ok(names)
}

fn split_report(tree: &SexpNode, requested: &[SymbolId<'_>]) -> Result<Value, CallToolResult> {
    for kind in ["bus", "bus_entry", "bus_alias"] {
        if tree.find(kind).is_some() {
            return Err(blocked("Bus membership is not supported by this report."));
        }
    }
    let mut placed = super::drawn_symbol_pins(tree)
        .ok_or_else(|| blocked("Cannot resolve the saved symbol pin geometry."))?;
    let block_uuids = selected_symbols(requested, &placed)?;
    let wires = extract_wires(tree);
    let labels = extract_labels(tree);
    if wires.len() != tree.find_all("wire").len()
        || wires
            .iter()
            .any(|w| ![w.x1, w.y1, w.x2, w.y2].into_iter().all(f64::is_finite))
    {
        return Err(blocked("Cannot resolve the endpoints of every wire."));
    }
    for kind in ["label", "global_label", "hierarchical_label"] {
        for node in tree.find_all(kind) {
            check_name(node, "net label")?;
        }
    }
    let mut drawn = exact_drawn_net_graph(tree, &wires, &labels);
    let mut scope_points = Vec::new();
    for label in &labels {
        let scope = match label.kind {
            LabelKind::NetLabel => Scope::Local,
            LabelKind::HierarchicalLabel => Scope::Hierarchical,
            LabelKind::GlobalLabel => Scope::Global,
            LabelKind::PowerSymbol => unreachable!("extract_labels excludes power symbols"),
        };
        scope_points.push(((label.x, label.y), scope, label.net.clone()));
    }
    let libs = tree
        .find("lib_symbols")
        .map(|n| n.find_all("symbol"))
        .unwrap_or_default();
    let alternate_names = apply_pin_alternates(tree, &mut placed, &libs)?;
    let mut power_symbols = Vec::new();
    for (inst, pins) in &placed {
        // The drawn pin inventory has already resolved this definition.
        let lib = find_lib_symbol(&libs, inst).expect("resolved embedded library symbol");
        let power = SymbolPower::from_library(lib);
        let at: Vec<_> = pins
            .iter()
            .map(|(pin, t)| (pin, drawn.pin_position(pin, *t)))
            .collect();
        for &(pin, point) in &at {
            if pin.electrical_type == "power_in" {
                let scoped_name = match power {
                    SymbolPower::Ordinary if pin.hidden => {
                        Some((Scope::GlobalPower, stored_name(&pin.name)))
                    }
                    SymbolPower::Global => Some((Scope::GlobalPower, inst.value.clone())),
                    SymbolPower::Local => Some((Scope::LocalPower, inst.value.clone())),
                    _ => None,
                };
                if let Some((scope, name)) = scoped_name {
                    if !resolved_scalar_name(&name) {
                        return Err(blocked(format!(
                            "Cannot resolve a saved power name on {}.{}.",
                            inst.reference, pin.number
                        )));
                    }
                    scope_points.push((point, scope, name));
                }
            }
            if is_power_symbol_reference(&inst.reference) {
                power_symbols.push((inst.uuid.as_deref().unwrap(), &inst.reference, point));
            }
        }
        add_symbol_power_drivers(&mut drawn, inst, power, at, drop);
    }
    let mut nets = drawn.clone().into_named_nets();
    let mut scopes = BTreeMap::<NetRoot, BTreeSet<(Scope, String)>>::new();
    let mut named_islands = BTreeMap::<NetRoot, BTreeSet<NetRoot>>::new();
    for ((x, y), scope, name) in scope_points {
        let root = nets.root_at(x, y);
        scopes.entry(root).or_default().insert((scope, name));
        named_islands
            .entry(root)
            .or_default()
            .insert(drawn.root_at(x, y));
    }
    let drawn_wires: BTreeSet<_> = wires
        .iter()
        .map(|wire| drawn.root_at(wire.x1, wire.y1))
        .collect();
    let mut placed_pins = Vec::new();
    for ((inst, pins), alternates) in placed.iter().zip(&alternate_names) {
        if is_power_symbol_reference(&inst.reference) {
            continue;
        }
        let uuid = inst.uuid.as_ref().unwrap();
        let inside = block_uuids.contains(uuid.as_str());
        for (ordinal, (pin, transform)) in pins.iter().enumerate() {
            let at = drawn.pin_position(pin, *transform);
            placed_pins.push(Placed {
                pin: PinRef {
                    reference: inst.reference.clone(),
                    symbol_uuid: uuid.clone(),
                    unit: inst.unit,
                    number: pin.number.clone(),
                    ordinal,
                    name: alternates
                        .get(pin.number.as_str())
                        .copied()
                        .unwrap_or(&pin.name)
                        .to_owned(),
                    electrical_type: pin.electrical_type.clone(),
                },
                inside,
                net: nets.root_at(at.0, at.1),
                island: drawn.root_at(at.0, at.1),
                pin_at: at,
            });
        }
    }
    let mut sides: BTreeMap<NetRoot, NetSides> = BTreeMap::new();
    let mut island_sides: BTreeMap<NetRoot, (bool, bool)> = BTreeMap::new();
    for placed in &placed_pins {
        let net = sides.entry(placed.net).or_default();
        let island = island_sides.entry(placed.island).or_default();
        if placed.inside {
            net.inside.push(placed.pin.clone());
            island.0 = true;
        } else {
            net.outside.push(placed.pin.clone());
            island.1 = true;
        }
    }
    let mut exits: Vec<_> = labels
        .iter()
        .filter(|l| l.kind == LabelKind::HierarchicalLabel)
        .map(|l| (l.x, l.y))
        .collect();
    for sheet in tree.find_all("sheet") {
        for pin in sheet.find_all("pin") {
            check_name(pin, "sheet pin")?;
            exits.push(at_of(pin).ok_or_else(|| blocked("Cannot resolve a sheet pin position."))?);
        }
    }
    let mut drawn_exits = BTreeSet::new();
    let mut exits_by_net = BTreeMap::<NetRoot, BTreeSet<NetRoot>>::new();
    for at in exits {
        let root = nets.root_at(at.0, at.1);
        sides.entry(root).or_default().leaves_sheet = true;
        let island = drawn.root_at(at.0, at.1);
        drawn_exits.insert(island);
        exits_by_net.entry(root).or_default().insert(island);
        island_sides.entry(island).or_default().1 = true;
    }
    let island_kind = |island: &NetRoot| match island_sides.get(island) {
        Some((true, false)) => "block",
        Some((true, true)) => "boundary",
        _ => "outside",
    };

    let mut internal = Vec::new();
    let mut boundary = Vec::new();
    let (mut signal_ports, mut rails, mut globals) = (0, 0, 0);
    let mut proposed_names = BTreeSet::new();
    let mut boundary_roots = BTreeSet::new();
    let no_scopes = BTreeSet::new();
    let mut child_aliases = BTreeMap::<String, BTreeSet<NetRoot>>::new();
    for (root, aliases) in &scopes {
        if sides.get(root).is_some_and(|net| !net.inside.is_empty()) {
            for (_, alias) in aliases {
                child_aliases
                    .entry(port_collision_key(alias))
                    .or_default()
                    .insert(*root);
            }
        }
    }
    for (root, net) in &mut sides {
        if net.inside.is_empty() {
            continue;
        }
        net.inside.sort_unstable();
        net.outside.sort_unstable();
        let name = nets.name_of_root(*root);
        let scoped = scopes.get(root).unwrap_or(&no_scopes);
        let scope_kinds: BTreeSet<_> = scoped.iter().map(|(scope, _)| *scope).collect();
        if scope_kinds.len() > 1 {
            return Err(blocked(format!(
                "Net '{}' has mixed label scopes; resolve its aliases before planning a split.",
                name.as_deref().unwrap_or("unnamed")
            )));
        }
        let aliases: Vec<_> = scoped
            .iter()
            .map(|(scope, name)| json!({"name": name, "scope": scope}))
            .collect();
        let is_rail = rail(scoped);
        let is_global = scope_kinds.contains(&Scope::Global);
        let crosses_project = is_global || scope_kinds.contains(&Scope::GlobalPower);
        if net.outside.is_empty() && !net.leaves_sheet && !crosses_project {
            if net.inside.len() >= 2 {
                internal.push(json!({"name": name, "aliases": aliases, "pins": net.inside}));
            }
            continue;
        }
        boundary_roots.insert(*root);
        let kind = if is_rail {
            "rail"
        } else if is_global {
            "global"
        } else {
            "signal"
        };
        if scope_kinds.contains(&Scope::LocalPower) {
            return Err(blocked(format!(
                "Sheet-local power net '{}' crosses the block boundary. It needs an explicit interface that keeps its scope.",
                name.as_deref().unwrap_or("unnamed")
            )));
        }
        let mut inside_islands = BTreeSet::new();
        let mut outside_islands = BTreeSet::new();
        for p in placed_pins.iter().filter(|p| p.net == *root) {
            if p.inside {
                inside_islands.insert(p.island);
            } else {
                outside_islands.insert(p.island);
            }
        }
        for &island in named_islands.get(root).into_iter().flatten() {
            if !inside_islands.contains(&island) {
                outside_islands.insert(island);
            }
        }
        outside_islands.extend(exits_by_net.get(root).into_iter().flatten().copied());
        let by_wire = inside_islands
            .iter()
            .any(|island| island_kind(island) == "boundary" && drawn_wires.contains(island));
        let by_name = inside_islands
            .iter()
            .any(|a| outside_islands.iter().any(|b| a != b));
        let joined_by = match (by_wire, by_name) {
            (true, true) => "both",
            (true, false) => "wire",
            (false, true) => "name",
            (false, false) if inside_islands.iter().any(|i| island_kind(i) == "boundary") => {
                "contact"
            }
            (false, false) => "off_sheet",
        };
        let mut entry = json!({
            "name": name,
            "aliases": aliases,
            "kind": kind,
            "joined_by": joined_by,
            "pins_inside": net.inside,
            "pins_outside": net.outside,
            "leaves_sheet": net.leaves_sheet,
        });
        if crosses_project {
            entry["scope_decision"] = json!(["keep_existing_scope", "stop"]);
        }
        match kind {
            "rail" => rails += 1,
            "global" => globals += 1,
            _ => {
                signal_ports += 1;
                let local_label = name.as_deref().filter(|_| scoped.len() == 1);
                let (pin_name, source) = suggested_pin_name(local_label, net);
                check_scalar_name(&pin_name, "suggested sheet pin")?;
                let collision_key = port_collision_key(&pin_name);
                if child_aliases
                    .get(&collision_key)
                    .is_some_and(|roots| roots.iter().any(|alias_root| alias_root != root))
                {
                    return Err(blocked(format!(
                        "Suggested sheet pin '{pin_name}' matches a different net in the block."
                    )));
                }
                if !proposed_names.insert(collision_key) {
                    return Err(blocked(format!(
                        "More than one signal would use sheet pin '{pin_name}'."
                    )));
                }
                entry["sheet_pin"] = json!({
                    "name": pin_name,
                    "name_source": source,
                    "shape": shape(&net.inside, &net.outside),
                    "requires_confirmation": true,
                });
                if by_name {
                    entry["scope_decision"] =
                        json!(["use_sheet_pin_without_widening_scope", "stop"]);
                }
            }
        }
        boundary.push(entry);
    }

    // Graph roots are not stable between runs; the report's order must be.
    let order = |net: &Value| {
        (
            net["name"].as_str().map(str::to_owned),
            net["pins"].to_string(),
            net["pins_inside"].to_string(),
        )
    };
    internal.sort_by_key(order);
    boundary.sort_by_key(order);

    // Drawn objects to review, not a list of edits or a transfer selection.
    let mut selection = BTreeMap::<&str, Vec<String>>::new();
    selection.insert("symbols", block_uuids.iter().cloned().collect());
    let mut moved_power = Vec::new();
    let mut boundary_power_symbols = Vec::new();
    for (uuid, reference, (x, y)) in power_symbols {
        match island_kind(&drawn.root_at(x, y)) {
            "block" => {
                moved_power.push(reference);
                selection
                    .entry("power_symbols")
                    .or_default()
                    .push(uuid.to_owned());
            }
            "boundary" => boundary_power_symbols.push(uuid.to_owned()),
            _ => {}
        }
    }
    let mut boundary_wires = Vec::new();
    for wire in &wires {
        let uuid = wire
            .uuid
            .clone()
            .ok_or_else(|| blocked("A saved wire has no UUID."))?;
        match island_kind(&drawn.root_at(wire.x1, wire.y1)) {
            "block" => selection.entry("wires").or_default().push(uuid),
            "boundary" => boundary_wires.push(uuid),
            _ => {}
        }
    }
    let mut boundary_labels = Vec::new();
    for kind in ["label", "global_label", "hierarchical_label"] {
        for node in tree.find_all(kind) {
            let uuid = uuid_of(node).ok_or_else(|| blocked("A saved label has no UUID."))?;
            let (x, y) = at_of(node).ok_or_else(|| blocked("Cannot resolve a label position."))?;
            if boundary_roots.contains(&nets.root_at(x, y)) {
                boundary_labels.push(uuid);
            } else if island_kind(&drawn.root_at(x, y)) == "block" {
                selection.entry("labels").or_default().push(uuid);
            }
        }
    }
    for (kind, key) in [("junction", "junctions"), ("no_connect", "no_connects")] {
        for node in tree.find_all(kind) {
            let uuid =
                uuid_of(node).ok_or_else(|| blocked(format!("A saved {kind} has no UUID.")))?;
            let at =
                at_of(node).ok_or_else(|| blocked(format!("Cannot resolve a {kind} position.")))?;
            if island_kind(&drawn.root_at(at.0, at.1)) == "block" {
                selection.entry(key).or_default().push(uuid);
            }
        }
    }

    for list in selection.values_mut() {
        list.sort_unstable();
        list.dedup();
    }
    boundary_wires.sort_unstable();
    boundary_labels.sort_unstable();
    boundary_power_symbols.sort_unstable();
    moved_power.sort_unstable();
    moved_power.dedup();

    let parts = placed
        .iter()
        .filter(|(inst, _)| block_uuids.contains(inst.uuid.as_deref().unwrap()))
        .map(|(inst, _)| &inst.reference)
        .collect::<BTreeSet<_>>()
        .len();
    let mut reasons = Vec::new();
    if internal.is_empty() {
        reasons.push("the block hides no net: every net it uses also leaves it".to_owned());
    } else if signal_ports > internal.len() {
        reasons.push(format!(
            "the block would expose {signal_ports} signals to hide {} nets",
            internal.len()
        ));
    }
    let recommendation = if reasons.is_empty() {
        reasons.push(format!(
            "The block would hide {} nets behind {signal_ports} signal ports.",
            internal.len()
        ));
        "split"
    } else {
        "keep"
    };
    let (supporting_parts, stranded_parts) =
        supporting_parts(&placed_pins, &drawn_wires, &drawn_exits, &nets, &scopes);

    Ok(json!({
        "recommendation": recommendation,
        "requires_membership_review": !supporting_parts.is_empty(),
        "supporting_parts": supporting_parts,
        "stranded_parts": stranded_parts,
        "nearby_mm": NEARBY_MM,
        "reasons": reasons,
        "counts": {
            "parts": parts,
            "internal_nets": internal.len(),
            "signal_ports": signal_ports,
            "rails": rails,
            "global_nets": globals,
        },
        "internal_nets": internal,
        "boundary_nets": boundary,
        "block_objects_for_review": selection,
        "block_power_references": moved_power,
        "boundary_wires": boundary_wires,
        "boundary_labels": boundary_labels,
        "boundary_power_symbols": boundary_power_symbols,
    }))
}

fn check_name(node: &SexpNode, kind: &str) -> Result<(), CallToolResult> {
    let name = node
        .get(1)
        .and_then(SexpNode::as_str)
        .ok_or_else(|| blocked(format!("Cannot resolve a saved {kind} name.")))?;
    check_scalar_name(name, kind)
}

fn check_scalar_name(name: &str, kind: &str) -> Result<(), CallToolResult> {
    if !resolved_scalar_name(name) {
        return Err(blocked(format!("Cannot resolve a saved {kind} name.")));
    }
    if is_bus_name(name) {
        return Err(blocked(format!(
            "Bus membership is not supported: {kind} '{name}'."
        )));
    }
    Ok(())
}

fn resolved_scalar_name(name: &str) -> bool {
    let name = unescaped(name);
    !name.is_empty() && !name.contains("${")
}

fn port_collision_key(name: &str) -> String {
    stored_name(&unescaped(name))
}

#[cfg(test)]
#[test]
fn direct_split_call_rejects_malformed_symbol_arguments() {
    for (symbols, field) in [
        (json!({}), "symbols"),
        (json!([]), "symbols"),
        (json!([{"uuid":"","unit":1}]), "symbols[0].uuid"),
        (json!([{"uuid":"x","unit":0}]), "symbols[0].unit"),
        (json!([{"uuid":"x","unit":0.5}]), "symbols[0].unit"),
    ] {
        let result = handle_plan_schematic_split(&json!({
            "schematic": "unused.kicad_sch",
            "symbols": symbols,
        }))
        .unwrap();
        assert!(result.is_error);
        let crate::mcp::protocol::ToolContent::Text { text } = &result.content[0] else {
            panic!("expected a text error");
        };
        let body: Value = serde_json::from_str(text).unwrap();
        assert_eq!(body["error"]["kind"], "invalid_argument", "{body:#}");
        assert_eq!(body["error"]["field"], field, "{body:#}");
    }
}

#[cfg(test)]
fn test_pin(reference: &str, number: &str, inside: bool, island: NetRoot, x: f64) -> Placed {
    Placed {
        pin: PinRef {
            reference: reference.into(),
            symbol_uuid: reference.into(),
            unit: 1,
            number: number.into(),
            ordinal: 0,
            name: String::new(),
            electrical_type: "passive".into(),
        },
        inside,
        net: island,
        island,
        pin_at: (x, 0.0),
    }
}

#[cfg(test)]
#[test]
fn split_plan_keeps_nearby_pins_and_retained_ports_separate() {
    use super::sch_connectivity::net_graph_for;

    let tree = copperpour_sexp::parse_sexp("(kicad_sch)").unwrap();
    let nets = net_graph_for(&tree, &[], &[]);
    let pin = test_pin;
    let common = (1, 0);
    let other = (2, 0);
    let base = [
        pin("R1", "1", true, common, 0.0),
        pin("C1", "1", false, common, 5.0),
        pin("C1", "2", false, other, 5.0),
    ];
    for (delta, exit, stranded) in [
        (30.0, false, true),
        (10.0, false, false),
        (30.0, true, false),
    ] {
        let mut placed = base
            .iter()
            .map(|p| {
                pin(
                    &p.pin.reference,
                    &p.pin.number,
                    p.inside,
                    p.island,
                    p.pin_at.0,
                )
            })
            .collect::<Vec<_>>();
        let mut retained = pin("U1", "1", false, other, 5.0 + delta);
        retained.pin.electrical_type = "input".into();
        placed.push(retained);
        let exits = if exit {
            BTreeSet::from([other])
        } else {
            BTreeSet::new()
        };
        let (suggestions, parts) = supporting_parts(
            &placed,
            &BTreeSet::from([common, other]),
            &exits,
            &nets,
            &BTreeMap::new(),
        );
        assert_eq!(suggestions.len(), 1);
        assert_eq!(suggestions[0]["retained_port"], exit);
        assert_eq!(suggestions[0]["would_be_stranded"], stranded);
        assert_eq!(parts, if stranded { vec!["C1"] } else { vec![] });
        assert_eq!(
            suggestions[0]["retained_nearby_pins"],
            if delta < NEARBY_MM {
                json!(["U1.1"])
            } else {
                json!([])
            }
        );
    }
}

#[cfg(test)]
#[test]
fn split_plan_only_reports_name_joined_power_inputs_on_rails() {
    use super::sch_connectivity::net_graph_for;

    let tree = copperpour_sexp::parse_sexp("(kicad_sch)").unwrap();
    let nets = net_graph_for(&tree, &[], &[]);
    let shared = (1, 0);
    let separate = (2, 0);
    let mut consumer = test_pin("U1", "1", false, separate, 10.0);
    consumer.net = shared;
    consumer.pin.electrical_type = "power_in".into();
    let placed = [
        test_pin("R1", "1", true, shared, 0.0),
        test_pin("C1", "1", false, shared, 5.0),
        test_pin("C1", "2", false, (3, 0), 5.0),
        consumer,
    ];
    for (scope, expected) in [
        (Scope::Local, json!([])),
        (Scope::GlobalPower, json!(["U1.1"])),
    ] {
        let (suggestions, _) = supporting_parts(
            &placed,
            &BTreeSet::from([shared]),
            &BTreeSet::new(),
            &nets,
            &BTreeMap::from([(shared, BTreeSet::from([(scope, "SWITCHED".into())]))]),
        );
        assert_eq!(suggestions.len(), 1);
        assert_eq!(suggestions[0]["reference"], "C1");
        assert_eq!(suggestions[0]["other_rail_consumers"], expected);
    }
}
