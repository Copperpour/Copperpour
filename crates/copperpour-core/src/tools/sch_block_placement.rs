//! Read-only placement around a fixed schematic anchor.

mod candidate;
mod inventory;
mod poses;
#[cfg(test)]
mod tests;
mod topology;

use super::sch_analysis::{overlap_findings, OVERLAP_TOLERANCE};
use super::sch_layout_score::{compare_layout, read_saved_sheet, LayoutComparison};
use super::sch_pin_contacts::pin_contact_findings;
use super::{get_path, invalid_arg, ToolContext, ToolDef};
use crate::mcp::protocol::CallToolResult;
use crate::tool;
use candidate::Changes;
use copperpour_sexp::schematic::{extract_labels, extract_wires, symbol_geometry_for_instance};
use copperpour_sexp::text_bounds::free_note_bounds;
use copperpour_sexp::SexpNode;
use inventory::{Inventory, Part, Pin, Role, SymbolId};
use poses::{direction, local_poses, span, step, Pose, CONNECTOR_GAP, MAX_CONNECTOR_RUN, MIN_WIRE};
use serde::Deserialize;
use serde_json::{json, Value};
use std::collections::{BTreeMap, BTreeSet};
use topology::{endpoints, length, mandatory_stubs};

const MAX_MOVABLE: usize = 24;
const MAX_TRIALS: usize = 64;

pub(super) fn definition() -> ToolDef {
    let id = json!({
        "type": "object", "required": ["uuid", "unit"], "additionalProperties": false,
        "properties": { "uuid": { "type": "string", "minLength": 1 }, "unit": { "type": "integer", "minimum": 1 } }
    });
    let uuid_list = json!({ "type": "array", "uniqueItems": true, "items": { "type": "string", "minLength": 1 } });
    tool!(
        "preview_block_placement",
        "Preview clearer placement and wiring around a fixed IC on one saved schematic sheet. \
         Select movable members by UUID and unit. Assign bypass capacitors explicitly to anchor \
         supply pins. Returns separate membership evidence, a revision, a checked plan, scores \
         and blockers. Does not edit files or run KiCad.",
        json!({
            "type": "object",
            "required": ["schematic", "anchor", "members"],
            "properties": {
                "schematic": { "type": "string", "description": "Path to the saved .kicad_sch sheet" },
                "anchor": id,
                "members": { "type": "array", "maxItems": MAX_MOVABLE, "uniqueItems": true, "items": id },
                "fixed_uuids": uuid_list,
                "note_uuids": uuid_list,
                "assignments": {
                    "type": "array", "maxItems": MAX_MOVABLE,
                    "items": { "type": "object", "required": ["member", "anchor_pin"], "additionalProperties": false,
                        "properties": { "member": id, "anchor_pin": { "type": "string", "minLength": 1 } } }
                },
                "connectors": {
                    "type": "array", "maxItems": MAX_MOVABLE,
                    "items": { "type": "object", "required": ["connector", "connector_pin", "port", "port_pin"], "additionalProperties": false,
                        "properties": { "connector": id, "connector_pin": { "type": "string", "minLength": 1 },
                            "port": id, "port_pin": { "type": "string", "minLength": 1 } } }
                }
            }
        }),
        |args, ctx| async move { handle_preview_block_placement(args, ctx).await }
    )
}

#[derive(Deserialize)]
struct Assignment<'a> {
    #[serde(borrow)]
    member: SymbolId<'a>,
    anchor_pin: &'a str,
}

#[derive(Deserialize)]
struct Connector<'a> {
    #[serde(borrow)]
    connector: SymbolId<'a>,
    connector_pin: &'a str,
    #[serde(borrow)]
    port: SymbolId<'a>,
    port_pin: &'a str,
}

struct Request<'a> {
    anchor: SymbolId<'a>,
    members: Vec<SymbolId<'a>>,
    assigned: BTreeMap<SymbolId<'a>, &'a str>,
    fixed: BTreeSet<&'a str>,
    notes: Vec<&'a str>,
    connectors: Vec<Connector<'a>>,
}

impl<'a> Request<'a> {
    fn parse(args: &'a Value) -> Result<Self, CallToolResult> {
        fn field<'a, T: Deserialize<'a>>(args: &'a Value, key: &str) -> Result<T, CallToolResult> {
            T::deserialize(&args[key]).map_err(|e| invalid_arg(key, &e.to_string()))
        }
        fn optional<'a, T: Default + Deserialize<'a>>(
            args: &'a Value,
            key: &str,
        ) -> Result<T, CallToolResult> {
            if args.get(key).is_none() {
                Ok(T::default())
            } else {
                field(args, key)
            }
        }
        let anchor = field(args, "anchor")?;
        let members: Vec<SymbolId<'_>> = field(args, "members")?;
        let member_ids: BTreeSet<_> = members.iter().copied().collect();
        if member_ids.len() != members.len() || member_ids.contains(&anchor) {
            return Err(invalid_arg(
                "members",
                "each member must be unique and cannot be the anchor",
            ));
        }
        let mut assigned = BTreeMap::new();
        for Assignment { member, anchor_pin } in
            optional::<Vec<Assignment<'a>>>(args, "assignments")?
        {
            if !member_ids.contains(&member) || assigned.insert(member, anchor_pin).is_some() {
                return Err(invalid_arg(
                    "assignments",
                    "each assignment must name a distinct selected member",
                ));
            }
        }
        Ok(Self {
            anchor,
            members,
            assigned,
            fixed: optional(args, "fixed_uuids")?,
            notes: optional(args, "note_uuids")?,
            connectors: optional(args, "connectors")?,
        })
    }
}

async fn handle_preview_block_placement(
    args: &Value,
    _ctx: &ToolContext,
) -> anyhow::Result<CallToolResult> {
    let path = get_path(args, "schematic")?;
    let request = match Request::parse(args) {
        Ok(request) => request,
        Err(e) => return Ok(e),
    };
    let (source, tree) = match read_saved_sheet(&path) {
        Ok(sheet) => sheet,
        Err(error) => return Ok(error),
    };
    let (suggestions, status, blockers, score, plan) = preview(&tree, &request);
    Ok(CallToolResult::json(&json!({
        "source": "saved_file", "schematic": path, "revision": crate::hex::sha256_hex(source.as_bytes()),
        "suggestions": suggestions, "status": status, "blockers": blockers,
        "comparison": score, "plan": plan,
    })))
}

type Preview = (Value, &'static str, Vec<String>, Value, Value);

fn preview(tree: &SexpNode, request: &Request<'_>) -> Preview {
    let refused =
        |suggestions, blockers| (suggestions, "blocked", blockers, Value::Null, Value::Null);
    let inventory = match Inventory::new(tree) {
        Ok(inventory) => inventory,
        Err(e) => return refused(json!([]), vec![e]),
    };
    let anchor = match inventory.symbol(request.anchor) {
        Ok(anchor) => anchor,
        Err(e) => return refused(json!([]), vec![e]),
    };
    let suggestions = json!(inventory.membership(anchor));
    let movable = match movable_parts(tree, anchor, &inventory, request) {
        Ok(movable) => movable,
        Err(e) => return refused(suggestions, vec![e]),
    };
    if tree.find("text_box").is_some() {
        return refused(
            suggestions,
            vec!["cannot measure clearance of text boxes".into()],
        );
    }
    for note in tree.find_all("text") {
        if let Err(reason) = free_note_bounds(note) {
            let uuid = note.find_str("uuid").unwrap_or_default();
            return refused(suggestions, vec![format!("note {uuid}: {reason}")]);
        }
    }
    let labels = extract_labels(tree);
    let topology = topology::Topology::new(tree, &inventory, &labels);
    if movable.is_empty() && request.connectors.is_empty() && request.notes.is_empty() {
        return (suggestions, "unchanged", vec![], Value::Null, Value::Null);
    }

    let mut failures = Vec::new();
    let mut rejected_candidate = None;
    let mut checked = None;
    for trial in 0..MAX_TRIALS {
        let mut changes = match place(anchor, &inventory, &topology, &movable, request, trial) {
            Ok(changes) => changes,
            Err(e) => {
                failures = vec![e];
                continue;
            }
        };
        for &uuid in &request.notes {
            if request.fixed.contains(uuid) {
                continue;
            }
            let Some((x, y, _)) = tree
                .find_all("text")
                .into_iter()
                .find(|node| node.find_str("uuid") == Some(uuid))
                .and_then(copperpour_sexp::schematic::parse_at)
            else {
                continue;
            };
            if trial != 0 {
                let vectors = [(1.0, 0.0), (-1.0, 0.0), (0.0, 1.0), (0.0, -1.0)];
                let distance = ((trial - 1) / 4 + 1) as f64 * MIN_WIRE;
                changes
                    .notes
                    .insert(uuid, step((x, y), vectors[(trial - 1) % 4], distance));
            }
        }
        let mut candidate = match candidate::build(tree, &inventory, &topology, &changes) {
            Ok(candidate) => candidate,
            Err(e) => {
                failures = vec![e];
                continue;
            }
        };
        let field_owners: BTreeSet<_> = changes
            .poses
            .keys()
            .copied()
            .chain(std::iter::once(anchor.id.uuid))
            .filter(|id| !request.fixed.contains(id))
            .collect();
        candidate::clear_visible_fields(&mut candidate, &field_owners, anchor);
        if let Err(e) = check_connector_runs(&candidate, request) {
            failures = vec![e];
            continue;
        }
        if let Err(e) = check_signal_lanes(&candidate, anchor, &movable) {
            failures = vec![e];
            continue;
        }
        if let Err(e) = check_drawn_islands(&candidate, &topology, &changes) {
            failures = vec![e];
            continue;
        }
        if let Err(e) = check_fixed_and_notes(tree, &candidate, request) {
            failures = vec![e];
            continue;
        }
        let removed: Vec<_> = changes.removed.into_iter().map(str::to_owned).collect();
        let comparison = match accept_candidate(tree, &candidate, &removed) {
            Ok(comparison) => comparison,
            Err(e) => {
                rejected_candidate.get_or_insert(e);
                continue;
            }
        };
        let score = comparison.json();
        if candidate == *tree {
            return (suggestions, "unchanged", vec![], score, Value::Null);
        }
        // The candidate already passed the preservation gate. Separately,
        // the preview must not call a wire-adding grouping a cleanup.
        let adds_wire = score["delta"]["wire_length_mm"]
            .as_f64()
            .is_some_and(|delta| delta > 0.0);
        if comparison.verdict == Some("improved") && !adds_wire {
            return (
                suggestions,
                "improved",
                vec![],
                score,
                candidate::diff(tree, &candidate),
            );
        }
        if !changes.groups.is_empty() && adds_wire {
            let plan = candidate::diff(tree, &candidate);
            if checked
                .as_ref()
                .is_none_or(|(status, _, _)| *status != "grouping_adds_wire")
            {
                checked = Some(("grouping_adds_wire", score, plan));
            }
        } else {
            let status = if adds_wire {
                "not_improved"
            } else {
                comparison.verdict.unwrap_or("unchanged")
            };
            checked.get_or_insert((status, score, Value::Null));
        }
    }
    if let Some((verdict, score, plan)) = checked {
        return (suggestions, verdict, vec![], score, plan);
    }
    refused(suggestions, rejected_candidate.unwrap_or(failures))
}

fn check_drawn_islands(
    candidate: &SexpNode,
    before: &topology::Topology<'_, '_>,
    changes: &Changes<'_>,
) -> Result<(), String> {
    use topology::Terminal;

    let after = Inventory::new(candidate)?;
    let labels = extract_labels(candidate);
    let mut drawn = super::sch_connectivity::redraw_net_graph(candidate, &after.wires, &labels);
    let surviving_wires: BTreeSet<_> = after
        .wires
        .iter()
        .filter_map(|w| w.uuid.as_deref())
        .collect();
    let rail_owners: BTreeMap<_, _> = changes
        .groups
        .iter()
        .flat_map(|(&root, members)| members.iter().copied().map(move |member| (member, root)))
        .collect();
    let mut owners = BTreeMap::new();
    let mut observed_roots = BTreeMap::new();
    for (&root, island) in &before.islands {
        let expected = rail_owners.get(&root).copied().unwrap_or(root);
        let mut record = |observed| {
            if owners
                .insert(observed, expected)
                .is_some_and(|other| other != expected)
            {
                return Err("candidate wiring would join separate drawn islands".to_owned());
            }
            if observed_roots
                .insert(expected, observed)
                .is_some_and(|other| other != observed)
            {
                return Err(if changes.groups.contains_key(&expected) {
                    "a grouped supply has no drawn wire to the retained symbol"
                } else {
                    "candidate wiring would split a drawn island"
                }
                .to_owned());
            }
            Ok(())
        };
        for end in &island.terminals {
            let observed = match end {
                Terminal::Pin(part, _) if changes.removed.contains(part.id.uuid) => continue,
                Terminal::Pin(part, pin) => {
                    after
                        .symbol(part.id)?
                        .pin(&pin.lib.number)
                        .ok_or("a drawn terminal is missing")?
                        .drawn
                }
                Terminal::Label(label) => drawn.root_at(label.x, label.y),
            };
            record(observed)?;
        }
        // Bare wires are also drawing objects, even if they carry no pin or label.
        for w in &island.wires {
            if w.uuid
                .as_deref()
                .is_some_and(|id| surviving_wires.contains(id))
            {
                record(drawn.root_at(w.x1, w.y1))?;
            }
        }
    }
    Ok(())
}

fn movable_parts<'s, 'a>(
    tree: &SexpNode,
    anchor: &'s Part<'a>,
    inventory: &'s Inventory<'a>,
    request: &Request<'_>,
) -> Result<Vec<(&'s Part<'a>, Role<'s>)>, String> {
    for &id in &request.fixed {
        if !tree
            .children()
            .into_iter()
            .flatten()
            .any(|n| n.find_str("uuid") == Some(id))
        {
            return Err(format!("fixed item {id} is not on the sheet"));
        }
    }
    for &id in &request.notes {
        if !tree
            .find_all("text")
            .iter()
            .any(|n| n.find_str("uuid") == Some(id))
        {
            return Err(format!(
                "note {id} is not an editable free-text note on the sheet"
            ));
        }
    }
    let mut result = Vec::new();
    for &id in &request.members {
        let part = inventory.symbol(id)?;
        if request.fixed.contains(part.id.uuid) {
            continue;
        }
        let role = inventory
            .role(anchor, part, request.assigned.get(&id).copied())
            .map_err(|e| format!("{}: {e}", part.inst.reference))?;
        result.push((part, role));
    }
    result.sort_by_key(|(part, role)| {
        let rank = match role {
            Role::Series { .. } => 0,
            Role::Feedback { .. } => 1,
            Role::Shunt { .. } => 2,
            Role::Bypass { .. } => 3,
        };
        (rank, part.id)
    });
    Ok(result)
}

fn place<'a>(
    anchor: &Part<'a>,
    inventory: &Inventory<'a>,
    topology: &topology::Topology<'_, 'a>,
    members: &[(&Part<'a>, Role<'_>)],
    request: &Request<'_>,
    trial: usize,
) -> Result<Changes<'a>, String> {
    let mut changes = Changes::default();
    for (index, &(part, role)) in members.iter().enumerate() {
        let mut tap = None;
        let old = Pose::of(part);
        if let Role::Bypass { supply, serves } = role {
            let p = part.pin(supply).ok_or("missing bypass supply pin")?;
            let a = anchor.pin(serves).ok_or("missing anchor supply pin")?;
            if bypass_is_placed(topology, part, p, a) {
                continue;
            }
            if a.drawn != p.drawn {
                group_rail(topology, request, &mut changes, a, p)?;
                tap = changes.rail_taps.get(&a.drawn).copied();
            }
        }
        let mut options = local_poses(anchor, part, role, tap);
        if !matches!(role, Role::Bypass { .. })
            && options.iter().take(5).any(|pose| *pose == old)
            && match role {
                Role::Series { near, serves } => series_lane(
                    topology,
                    part.pin(near).ok_or("missing series pin")?,
                    anchor.pin(serves).ok_or("missing served signal pin")?,
                ),
                Role::Feedback { .. } => trial == 0,
                _ => true,
            }
        {
            continue;
        }
        let attempt = trial / (index + 1);
        if let Role::Feedback { input, .. } = role {
            let near = part.pin(input.0).ok_or("missing feedback pin")?;
            let center = anchor.inst.y;
            let below = trial < MAX_TRIALS / 2;
            options.retain(|pose| (pose.tip(part, near).1 >= center) == below);
        }
        let (pose, paired) = options
            .drain(..)
            .filter_map(|pose| {
                let mut candidates = vec![(part, pose)];
                for pin in &part.pins {
                    if matches!(role, Role::Bypass { supply, .. } if supply == pin.lib.number) {
                        continue;
                    }
                    if let Some((power, power_pin)) = topology.private_supply(pin) {
                        let up = if inventory.is_ground(pin.net) {
                            1.0
                        } else {
                            -1.0
                        };
                        let at = step(pose.tip(part, pin), (0.0, up), MIN_WIRE);
                        candidates.push((
                            power,
                            Pose::with_tip(power, power_pin, at, power.inst.rotation),
                        ));
                    }
                }
                clear_bodies(inventory, members, request, &changes, &candidates)
                    .then_some((pose, candidates))
            })
            .nth(attempt % 8)
            .ok_or_else(|| format!("no clear nearby pose for {}", part.inst.reference))?;
        changes.poses.insert(part.id.uuid, pose);
        for (part, pose) in paired {
            changes.poses.insert(part.id.uuid, pose);
        }
    }
    // Group the rail-facing leg of a pull-up with the explicitly assigned
    // bypass on the same local supply, not with every same-named island.
    for &(part, role) in members {
        let Role::Shunt { near, .. } = role else {
            continue;
        };
        let Some(rail_pin) = part.pins.iter().find(|p| p.lib.number != near) else {
            continue;
        };
        if !changes.poses.contains_key(part.id.uuid) || inventory.is_ground(rail_pin.net) {
            continue;
        }
        let targets: Vec<_> = anchor
            .pins
            .iter()
            .filter(|p| p.net == rail_pin.net && changes.groups.contains_key(&p.drawn))
            .collect();
        if let [target] = targets.as_slice() {
            if let Some((supply, _)) = topology.private_supply(rail_pin) {
                changes.poses.remove(supply.id.uuid);
                group_rail(topology, request, &mut changes, target, rail_pin)?;
            }
        }
    }
    for pairing in &request.connectors {
        let part = inventory.symbol(pairing.connector)?;
        if !part.is_connector()
            || (pairing.port != request.anchor && !request.members.contains(&pairing.port))
        {
            return Err("connector pairing needs an edge connector and a block port".into());
        }
        let pin = part
            .pin(pairing.connector_pin)
            .ok_or("unknown connector pin")?;
        let port = inventory.symbol(pairing.port)?;
        let served = port.pin(pairing.port_pin).ok_or("unknown block port pin")?;
        if pin.net != served.net {
            return Err("paired connector and block port are on different nets".into());
        }
        let current = Pose::of(part);
        let port_pose = changes
            .poses
            .get(port.id.uuid)
            .copied()
            .unwrap_or_else(|| Pose::of(port));
        let target = port_pose.tip(port, served);
        let side = if current.x < anchor.inst.x { -1.0 } else { 1.0 };
        let port_dir = copperpour_sexp::schematic::pin_outward_direction(
            &served.lib,
            port_pose.transform(port),
        );
        if direction(pin.outward) != (-side, 0.0) || direction(port_dir) != (side, 0.0) {
            return Err(format!(
                "{} does not face its paired block port",
                part.inst.reference
            ));
        }
        let join_rail = pin.drawn != served.drawn
            && !request.fixed.contains(part.id.uuid)
            && inventory.is_rail(pin.net)
            && changes.groups.contains_key(&served.drawn)
            && topology.private_supply(pin).is_some();
        if pin.drawn != served.drawn && !join_rail {
            return Err(format!(
                "{} and its paired port have no drawn run",
                part.inst.reference
            ));
        }
        if request.fixed.contains(part.id.uuid) {
            if span(target, pin.tip) > MAX_CONNECTOR_RUN + 1e-6 {
                return Err(format!(
                    "fixed connector {} needs a run longer than {MAX_CONNECTOR_RUN} mm",
                    part.inst.reference
                ));
            }
            continue;
        }
        let edge = |part: &Part<'_>, pose: Pose, direction: f64| {
            let mut inst = part.inst.clone();
            (inst.x, inst.y, inst.rotation) = (pose.x, pose.y, pose.rotation);
            let bounds = symbol_geometry_for_instance(part.lib, &inst)?.envelope()?;
            Some(if direction < 0.0 {
                -bounds.min_x
            } else {
                bounds.max_x
            })
        };
        let edge_of = |symbol: &Part<'_>| {
            let pose = changes
                .poses
                .get(symbol.id.uuid)
                .copied()
                .unwrap_or_else(|| Pose::of(symbol));
            edge(symbol, pose, side)
        };
        let outer = request.members.iter().try_fold(
            edge_of(anchor).ok_or("cannot measure the anchor envelope")?,
            |outer, &id| -> Result<f64, String> {
                let symbol = inventory.symbol(id)?;
                Ok(outer.max(edge_of(symbol).ok_or("cannot measure a block member envelope")?))
            },
        )?;
        let inner = -edge(part, current, -side).ok_or("cannot measure connector envelope")?;
        let projected_tip = outer + CONNECTOR_GAP + (pin.tip.0 * side - inner);
        let grid = poses::GRID;
        let at = (
            copperpour_sexp::geometry::round6(side * (projected_tip / grid).round() * grid),
            target.1,
        );
        if (at.0 - anchor.inst.x) * side <= 0.0 {
            return Err(format!(
                "{} cannot stay on its side of the block",
                part.inst.reference
            ));
        }
        let next = Pose::with_tip(part, pin, at, current.rotation);
        if !next.on_grid(part) {
            return Err("edge connector cannot be placed on the pin grid".into());
        }
        if next == current && !join_rail {
            if !topology.islands[&pin.drawn].reaches_within(pin.tip, served.tip, MAX_CONNECTOR_RUN)
            {
                changes.redraw.insert(pin.drawn);
            }
            continue;
        }
        if let Some((power, power_pin)) = part
            .pins
            .iter()
            .filter(|other| other.lib.number != pin.lib.number)
            .find_map(|other| topology.private_supply(other).map(|p| (p.0, p.1)))
        {
            let step_by = (next.x - current.x, next.y - current.y);
            changes.poses.insert(
                power.id.uuid,
                Pose::with_tip(
                    power,
                    power_pin,
                    step(power_pin.tip, step_by, 1.0),
                    power.inst.rotation,
                ),
            );
        }
        changes.poses.insert(part.id.uuid, next);
        if join_rail {
            group_rail(topology, request, &mut changes, served, pin)?;
        }
    }
    route_grouped_rails(topology, request, &mut changes)?;
    Ok(changes)
}

fn series_lane(topology: &topology::Topology<'_, '_>, pin: &Pin, served: &Pin) -> bool {
    let (dx, dy) = direction(served.outward);
    let (x, y) = (pin.tip.0 - served.tip.0, pin.tip.1 - served.tip.1);
    x * dy == y * dx
        && x * dx + y * dy > 0.0
        && pin.drawn == served.drawn
        && topology.islands[&pin.drawn].reaches_within(
            pin.tip,
            served.tip,
            span(pin.tip, served.tip),
        )
}

fn bypass_is_placed(
    topology: &topology::Topology<'_, '_>,
    part: &Part<'_>,
    supply: &Pin,
    served: &Pin,
) -> bool {
    let Some(ground) = part
        .pins
        .iter()
        .find(|pin| pin.lib.number != supply.lib.number)
    else {
        return false;
    };
    let separation = span(supply.tip, served.tip);
    let ground_island = &topology.islands[&ground.drawn];
    supply.drawn == served.drawn
        && separation <= 9.0 * MIN_WIRE + 6.0 * poses::GRID + 1e-6
        && topology.islands[&supply.drawn].reaches_within(
            supply.tip,
            served.tip,
            separation + 2.0 * MIN_WIRE,
        )
        && supply.tip.0 == ground.tip.0
        && supply.tip.1 < ground.tip.1
        && ground_island.wires.iter().any(|wire| {
            let [a, b] = endpoints(wire);
            let other = if topology::coincides(a, ground.tip) {
                b
            } else if topology::coincides(b, ground.tip) {
                a
            } else {
                return false;
            };
            other.0 == ground.tip.0 && other.1 - ground.tip.1 >= MIN_WIRE - 1e-6
        })
        && ground_island.terminals.iter().any(|terminal| {
            matches!(terminal, topology::Terminal::Pin(part, marker)
                if part.lib.find("power").is_some()
                    && marker.lib.electrical_type == "power_in"
                    && marker.tip.1 > ground.tip.1)
        })
}

fn check_connector_runs(tree: &SexpNode, request: &Request<'_>) -> Result<(), String> {
    if request.connectors.is_empty() {
        return Ok(());
    }
    let inventory = Inventory::new(tree)?;
    let anchor = inventory.symbol(request.anchor)?;
    let labels = extract_labels(tree);
    let topology = topology::Topology::new(tree, &inventory, &labels);
    for pairing in &request.connectors {
        let connector = inventory.symbol(pairing.connector)?;
        let pin = connector
            .pin(pairing.connector_pin)
            .ok_or("missing connector pin")?;
        let port = inventory
            .symbol(pairing.port)?
            .pin(pairing.port_pin)
            .ok_or("missing port pin")?;
        let fail = || {
            format!(
                "connector {} cannot reach its port in a drawn run of at most {MAX_CONNECTOR_RUN} mm",
                connector.inst.reference,
            )
        };
        if (connector.inst.x - anchor.inst.x) * (port.tip.0 - pin.tip.0) >= 0.0 {
            return Err(format!(
                "connector {} does not face its block port",
                connector.inst.reference
            ));
        }
        if pin.drawn != port.drawn
            || !topology.islands[&pin.drawn].reaches_within(pin.tip, port.tip, MAX_CONNECTOR_RUN)
        {
            return Err(fail());
        }
    }
    Ok(())
}

fn check_signal_lanes(
    tree: &SexpNode,
    anchor: &Part<'_>,
    members: &[(&Part<'_>, Role<'_>)],
) -> Result<(), String> {
    let inventory = Inventory::new(tree)?;
    let labels = extract_labels(tree);
    let topology = topology::Topology::new(tree, &inventory, &labels);
    let observed_anchor = inventory.symbol(anchor.id)?;
    let bounds = |part: &Part<'_>| {
        symbol_geometry_for_instance(part.lib, &part.inst).and_then(|g| g.envelope())
    };
    for &(original, role) in members {
        let part = inventory.symbol(original.id)?;
        match role {
            Role::Series { near, serves } => {
                let pin = part.pin(near).ok_or("missing series pin")?;
                let served = observed_anchor
                    .pin(serves)
                    .ok_or("missing served signal pin")?;
                if !series_lane(&topology, pin, served) {
                    return Err(format!(
                        "{} cannot be wired in a straight series lane",
                        part.inst.reference
                    ));
                }
            }
            Role::Bypass { supply, serves } => {
                let pin = part.pin(supply).ok_or("missing bypass supply pin")?;
                let served = observed_anchor
                    .pin(serves)
                    .ok_or("missing anchor supply pin")?;
                if !bypass_is_placed(&topology, part, pin, served) {
                    return Err(format!(
                        "{} needs a nearby drawn supply path and a ground lead below it",
                        part.inst.reference
                    ));
                }
            }
            Role::Feedback { input, output } => {
                let anchor_bounds = bounds(anchor).ok_or("missing anchor envelope for feedback")?;
                let feedback = bounds(part).ok_or("missing resistor envelope for feedback")?;
                let a = part.pin(input.0).ok_or("missing feedback input leg")?;
                let b = part.pin(output.0).ok_or("missing feedback output leg")?;
                for (leg, anchor_pin) in [(a, input.1), (b, output.1)] {
                    let served = observed_anchor
                        .pin(anchor_pin)
                        .ok_or("missing feedback anchor pin")?;
                    if leg.drawn != served.drawn {
                        return Err(format!(
                            "{} has no drawn feedback leg to anchor pin {anchor_pin}",
                            part.inst.reference
                        ));
                    }
                }
                if a.tip.1 != b.tip.1
                    || (feedback.min_y < anchor_bounds.max_y
                        && feedback.max_y > anchor_bounds.min_y)
                {
                    return Err(format!(
                        "{} has no feedback lane above or below the anchor",
                        part.inst.reference
                    ));
                }
            }
            Role::Shunt { near, serves, .. } => {
                let pin = part.pin(near).ok_or("missing shunt pin")?;
                let served = observed_anchor
                    .pin(serves)
                    .ok_or("missing served shunt pin")?;
                if pin.drawn != served.drawn {
                    return Err(format!(
                        "{} has no drawn shunt leg to anchor pin {serves}",
                        part.inst.reference
                    ));
                }
            }
        }
    }
    Ok(())
}

fn route_grouped_rails<'a>(
    topology: &topology::Topology<'_, 'a>,
    request: &Request<'_>,
    changes: &mut Changes<'a>,
) -> Result<(), String> {
    use topology::Terminal;

    for (&root, branches) in &changes.groups {
        if changes.rail_taps.contains_key(&root) {
            continue;
        }
        let island = &topology.islands[&root];
        let (owner, target) = island
            .terminals
            .iter()
            .find_map(|t| match t {
                Terminal::Pin(owner, pin) if owner.lib.find("power").is_none() => {
                    Some((*owner, *pin))
                }
                _ => None,
            })
            .ok_or("cannot identify the retained supply node")?;
        let (symbol, supply_pin) = topology
            .private_supply(target)
            .ok_or("cannot locate the retained power symbol")?;
        let start = changes
            .poses
            .get(owner.id.uuid)
            .copied()
            .unwrap_or_else(|| Pose::of(owner))
            .tip(owner, target);
        let outward = direction(target.outward);
        let normal = (1.0, 0.0);
        let across = (0.0, if outward.1 == 0.0 { -1.0 } else { outward.1 });
        let mut trunk = BTreeMap::new();
        let mut stubs = Vec::new();
        let along = |(x, y): (f64, f64)| x * normal.0 + y * normal.1;
        let offset = |(x, y): (f64, f64)| x * across.0 + y * across.1;
        let terminals = branches
            .iter()
            .map(|root| {
                topology.islands[root]
                    .terminals
                    .iter()
                    .find_map(|t| match t {
                        Terminal::Pin(p, pin) if p.lib.find("power").is_none() => Some(
                            changes
                                .poses
                                .get(p.id.uuid)
                                .copied()
                                .unwrap_or_else(|| Pose::of(p))
                                .tip(p, pin),
                        ),
                        _ => None,
                    })
                    .ok_or("rail group has no branch terminal")
            })
            .collect::<Result<Vec<_>, _>>()?;
        let trunk_offset = if outward.1 != 0.0 {
            terminals
                .iter()
                .map(|&p| offset(p))
                .fold(offset(start), f64::max)
                + MIN_WIRE
        } else {
            offset(start)
        };
        let trunk_at = |p| step(p, across, trunk_offset - offset(p));
        let tee = if outward.0 == 0.0 {
            trunk_at(start)
        } else {
            step(start, outward, MIN_WIRE)
        };
        let point_key = |p| (along(p) * 1e6).round() as i64;
        trunk.insert(point_key(tee), tee);
        stubs.push([start, tee]);
        for p in terminals {
            let on_trunk = trunk_at(p);
            trunk.insert(point_key(on_trunk), on_trunk);
            if !topology::coincides(p, on_trunk) {
                stubs.push([p, on_trunk]);
            }
        }
        let power_out = direction(supply_pin.outward);
        let supply_at = step(tee, power_out, -MIN_WIRE);
        let pose = Pose::with_tip(symbol, supply_pin, supply_at, symbol.inst.rotation);
        if request.fixed.contains(symbol.id.uuid) && pose != Pose::of(symbol) {
            return Err(format!(
                "fixed supply {} prevents the local rail branch",
                symbol.inst.reference
            ));
        }
        changes.poses.insert(symbol.id.uuid, pose);
        stubs.push([tee, supply_at]);
        let points: Vec<_> = trunk.into_values().collect();
        stubs.extend(points.windows(2).map(|p| [p[0], p[1]]));
        changes.rail_routes.insert(root, stubs);
    }
    Ok(())
}

fn group_rail<'a>(
    topology: &topology::Topology<'_, 'a>,
    request: &Request<'_>,
    changes: &mut Changes<'a>,
    anchor: &Pin,
    member: &Pin,
) -> Result<(), String> {
    let retained = if let Some((symbol, _)) = topology.private_supply(anchor) {
        symbol
    } else {
        let island = &topology.islands[&anchor.drawn];
        let mut supplies = island
            .terminals
            .iter()
            .filter_map(|terminal| match terminal {
                topology::Terminal::Pin(part, pin)
                    if part.lib.find("power").is_some()
                        && pin.lib.electrical_type == "power_in" =>
                {
                    Some(*part)
                }
                _ => None,
            });
        let (Some(symbol), None) = (supplies.next(), supplies.next()) else {
            return Err("the anchor rail does not have one unambiguous supply symbol".into());
        };
        let tap = island
            .junctions
            .iter()
            .min_by(|&&a, &&b| span(a, anchor.tip).total_cmp(&span(b, anchor.tip)))
            .ok_or("the anchor rail has no existing junction for a bypass branch")?;
        changes.rail_taps.insert(anchor.drawn, *tap);
        symbol
    };
    let (removed, _) = topology
        .private_supply(member)
        .ok_or("the member supply does not have a private lead")?;
    if anchor.net != member.net
        || retained.inst.value != removed.inst.value
        || retained.lib.find("power") != removed.lib.find("power")
        || request.fixed.contains(removed.id.uuid)
    {
        return Err(
            "the member supply symbol is not a removable duplicate of the anchor supply".into(),
        );
    }
    changes.removed.insert(removed.id.uuid);
    changes
        .groups
        .entry(anchor.drawn)
        .or_default()
        .insert(member.drawn);
    Ok(())
}

fn clear_bodies(
    inventory: &Inventory<'_>,
    members: &[(&Part<'_>, Role<'_>)],
    request: &Request<'_>,
    changes: &Changes<'_>,
    proposed: &[(&Part<'_>, Pose)],
) -> bool {
    let bounds = |part: &Part<'_>, pose: Pose| {
        let mut inst = part.inst.clone();
        (inst.x, inst.y, inst.rotation) = (pose.x, pose.y, pose.rotation);
        symbol_geometry_for_instance(part.lib, &inst).and_then(|geom| geom.body)
    };
    proposed.iter().enumerate().all(|(i, &(part, pose))| {
        if request.fixed.contains(part.id.uuid) && pose != Pose::of(part) {
            return false;
        }
        let Some(body) = bounds(part, pose) else {
            return true;
        };
        !proposed[..i].iter().any(|&(other, other_pose)| {
            bounds(other, other_pose).is_some_and(|b| b.interiors_intersect(body))
        }) && !inventory
            .parts
            .values()
            .filter(|p| {
                !proposed.iter().any(|&(part, _)| part.id == p.id)
                    && !changes.removed.contains(p.id.uuid)
            })
            .filter_map(|p| {
                changes
                    .poses
                    .get(p.id.uuid)
                    .copied()
                    .or_else(|| {
                        (!members.iter().any(|&(part, _)| part.id == p.id)).then(|| Pose::of(p))
                    })
                    .and_then(|pose| bounds(p, pose))
            })
            .any(|b| b.interiors_intersect(body))
    })
}

fn check_fixed_and_notes(
    before: &SexpNode,
    after: &SexpNode,
    request: &Request<'_>,
) -> Result<(), String> {
    for &uuid in &request.fixed {
        let get = |tree: &SexpNode| {
            tree.children()
                .into_iter()
                .flatten()
                .find(|n| n.find_str("uuid") == Some(uuid))
                .cloned()
        };
        if get(before) != get(after) {
            return Err(format!("candidate would change fixed item {uuid}"));
        }
    }
    let wires = extract_wires(after);
    let inventory = Inventory::new(after)?;
    let symbol_envelopes: Vec<_> = inventory
        .parts
        .values()
        .map(|p| {
            symbol_geometry_for_instance(p.lib, &p.inst)
                .and_then(|g| g.envelope())
                .ok_or_else(|| format!("cannot measure geometry of {}", p.inst.reference))
        })
        .collect::<Result<_, _>>()?;
    let text_extents = copperpour_sexp::text_bounds::text_extents(after);
    let mut other_notes = Vec::new();
    for note in after.find_all("text") {
        let uuid = note.find_str("uuid").unwrap_or_default();
        if let Some(bounds) = free_note_bounds(note).map_err(|e| format!("note {uuid}: {e}"))? {
            if wires
                .iter()
                .any(|w| bounds.segment_enters((w.x1, w.y1), (w.x2, w.y2)))
            {
                return Err(format!("candidate wires would cross note {uuid}"));
            }
            if symbol_envelopes
                .iter()
                .any(|b| bounds.interiors_intersect(*b))
                || text_extents
                    .iter()
                    .any(|extent| bounds.interiors_intersect(extent.reported_run()))
                || other_notes.iter().any(|b| bounds.interiors_intersect(*b))
            {
                return Err(format!("candidate geometry intersects note {uuid}"));
            }
            other_notes.push(bounds);
        }
    }
    Ok(())
}

fn accept_candidate(
    before: &SexpNode,
    after: &SexpNode,
    removed: &[String],
) -> Result<LayoutComparison, Vec<String>> {
    let before_inventory = inventory::Inventory::new(before).map_err(|e| vec![e])?;
    let after_inventory = inventory::Inventory::new(after).map_err(|e| vec![e])?;
    let comparison = compare_layout(
        after,
        Some(before),
        removed,
        [
            &mandatory_stubs(before, &before_inventory),
            &mandatory_stubs(after, &after_inventory),
        ],
    );
    if comparison.verdict == Some("blocked") {
        return Err(comparison.reasons);
    }

    let wires = extract_wires(after);
    let (overlaps, _, unresolved) = overlap_findings(after, &wires, OVERLAP_TOLERANCE);
    if !unresolved.is_empty() {
        return Err(vec![format!(
            "cannot measure clearance of {}",
            unresolved.join(", ")
        )]);
    }
    let (contacts, _) =
        pin_contact_findings(after, &wires, poses::MIN_WIRE).map_err(|e| vec![e.into()])?;
    let before_wires: BTreeSet<_> = before_inventory
        .wires
        .iter()
        .filter_map(|w| w.uuid.as_deref())
        .collect();
    let new_wires: Vec<_> = wires
        .iter()
        .filter(|w| {
            w.uuid
                .as_deref()
                .is_some_and(|id| !before_wires.contains(id))
        })
        .collect();
    let mut blockers: Vec<_> = overlaps
        .into_iter()
        .chain(contacts)
        .map(|mut finding| {
            if let Some(wire) = finding.get_mut("a").filter(|a| a["kind"] == "wire") {
                if let Some(wire) = wire.as_object_mut() {
                    wire.remove("uuid");
                }
            }
            finding.to_string()
        })
        .collect();
    for directive in ["netclass_flag", "directive_label"]
        .into_iter()
        .flat_map(|tag| after.find_all(tag))
    {
        let Some((x, y, _)) = copperpour_sexp::schematic::parse_at(directive) else {
            return Err(vec!["cannot locate a net-class directive".into()]);
        };
        if new_wires
            .iter()
            .any(|wire| topology::on_wire((x, y), endpoints(wire)))
        {
            blockers.push(format!(
                "a new wire would touch a net-class directive at ({x}, {y})"
            ));
        }
    }
    for wire in &wires {
        if length(endpoints(wire)) + 1e-6 < poses::MIN_WIRE {
            blockers.push(format!(
                "wire from ({}, {}) to ({}, {}) is shorter than {:.2} mm",
                wire.x1,
                wire.y1,
                wire.x2,
                wire.y2,
                poses::MIN_WIRE
            ));
        }
    }
    if blockers.is_empty() {
        Ok(comparison)
    } else {
        Err(blockers)
    }
}
