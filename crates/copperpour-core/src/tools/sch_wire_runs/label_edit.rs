//! Exact item changes for the standalone-sheet wire/label exchange.

use super::{mm, point, WireRun, WireRuns};
use crate::{
    mcp::{error::ToolErrorKind, protocol::CallToolResult},
    tool,
    tools::{
        get_path, invalid_arg, non_regular_write_refusal, resolve_schematic_ownership,
        revision_conflict_result, same_schematic_document,
        sch_connectivity::{
            kicad_schematic_mm, net_graph_for, placed_pin_alternates, redraw_net_graph,
            saved_label_touches_wire, schematic_pin_endpoint, schematic_point,
            schematic_point_on_wire, select_pin_alternate, NamingPin, NetRoot, PinNetNames,
            UnitNotation,
        },
        ToolDef,
    },
};
use copperpour_schematic_editor::{
    schematic::{label::Label as NewLabel, wire::Wire},
    sexp::{
        writer::{detect_indent, write_schematic_item},
        SexpNode as ItemNode,
    },
};
use copperpour_sexp::{
    command::{prepare_command, ItemAnchor, ItemChange, ItemId, SchematicCommand},
    geometry::point_on_segment,
    schematic::{
        extract_bus_entries, extract_buses, extract_junctions, extract_wires, parse_at,
        pin_endpoint, read_schematic, Label, LabelKind,
    },
    writer::write_atomic_if_unchanged,
    SexpError, SexpNode,
};
use serde::Deserialize;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    path::Path,
};

type Point = (f64, f64);
const STUB_MM: f64 = 2.54;
const OPERATION: &str = "exchange schematic wires and labels";

pub(in crate::tools) fn tool_def() -> ToolDef {
    let ids = json!({"type":"array", "minItems":1, "uniqueItems":true, "items":{"type":"string","minLength":1}});
    let pair = |items| json!({"type":"array", "minItems":2, "maxItems":2, "items":items});
    let point = pair(json!({"type":"number"}));
    tool!(
        "exchange_schematic_wires_and_labels",
        "Preview one wire/label exchange on a standalone saved sheet. Applying requires the \
         preview revision and acknowledgement of any exact netlist rename. No batch or hierarchy edits.",
        json!({
            "type":"object",
            "properties":{
                "schematic":{"type":"string"},
                "edit":{
                    "type":"object",
                    "properties":{
                        "direction":{"enum":["wire_to_labels","labels_to_wire"]},
                        "wire_uuids":ids,
                        "local_name":{"type":"string"},
                        "anchors":pair(point),
                        "label_uuids":pair(json!({"type":"string","minLength":1}))
                    },
                    "required":["direction"]
                },
                "revision":{"type":"string","minLength":1,"description":"Omit to preview; pass the preview revision to apply."},
                "acknowledge_rename":{
                    "type":"object",
                    "properties":{"from":{"type":"string"},"to":{"type":"string"}},
                    "required":["from","to"]
                }
            },
            "required":["schematic","edit"]
        }),
        |args, _ctx| async move { handle_exchange_schematic_wires_and_labels(args) }
    )
}

fn handle_exchange_schematic_wires_and_labels(args: &Value) -> anyhow::Result<CallToolResult> {
    let path = get_path(args, "schematic")?;
    if std::fs::symlink_metadata(&path).is_ok_and(|entry| !entry.is_file()) {
        return Ok(non_regular_write_refusal(
            &SexpError::NonRegularWriteTarget { path },
            OPERATION,
        )
        .expect("non-regular target refusal"));
    }
    let path = match path.canonicalize() {
        Ok(path) => path,
        Err(error) => {
            let error = SexpError::Io(error);
            return super::schematic_read_refusal(&path, &error).ok_or_else(|| error.into());
        }
    };
    let edit_value = &args["edit"];
    let edit: Edit = match serde_json::from_value(edit_value.clone()) {
        Ok(edit) => edit,
        Err(error) => return Ok(invalid_arg("edit", &error.to_string())),
    };
    let supplied = match args.get("revision") {
        None => None,
        Some(Value::String(value)) if !value.is_empty() => Some(value.as_str()),
        _ => {
            return Ok(invalid_arg(
                "revision",
                "must be a nonempty preview revision",
            ))
        }
    };
    let (source, before) = match read_schematic(&path) {
        Ok(sheet) => sheet,
        Err(error) => {
            return super::schematic_read_refusal(&path, &error).ok_or_else(|| error.into());
        }
    };
    if before.head() != Some("kicad_sch") {
        return Ok(super::malformed_schematic(
            &path,
            "expected a KiCad schematic sheet".to_owned(),
        ));
    }
    let ownership = match resolve_schematic_ownership(&path) {
        Ok(ownership) => ownership,
        Err(error) => return Ok(error.into_tool_result()),
    };
    if before.find("sheet").is_some()
        || ownership.as_ref().is_some_and(|owner| {
            !same_schematic_document(&owner.root_schematic, &path)
                || owner.single_top_level_sheet().is_err()
                || owner.instance_paths.len() != 1
        })
    {
        return Ok(invalid_arg("schematic", "select a standalone KiCad sheet"));
    }

    let project = if let Some(owner) = &ownership {
        let path = &owner.project_file;
        match std::fs::read_to_string(path) {
            Ok(contents) => Some((path, contents)),
            Err(error) if error.kind() == std::io::ErrorKind::InvalidData => {
                return Ok(CallToolResult::error_kind(
                    ToolErrorKind::MalformedFile {
                        path: path.display().to_string(),
                        reason: error.to_string(),
                    },
                    format!("Cannot read project {}: {error}", path.display()),
                ));
            }
            Err(error) => {
                return Ok(CallToolResult::error_kind(
                    ToolErrorKind::file_not_found(path),
                    format!("Cannot read project {}: {error}", path.display()),
                ));
            }
        }
    } else {
        None
    };
    let notation = project
        .as_ref()
        .and_then(|(_, source)| UnitNotation::from_project_source(source))
        .unwrap_or_default();
    let mut digest = Sha256::new();
    let encoded_edit = edit_value.to_string();
    for part in [
        path.as_os_str().as_encoded_bytes(),
        source.as_bytes(),
        encoded_edit.as_bytes(),
    ] {
        digest.update((part.len() as u64).to_le_bytes());
        digest.update(part);
    }
    if let Some((project_path, contents)) = &project {
        for part in [
            project_path.as_os_str().as_encoded_bytes(),
            contents.as_bytes(),
        ] {
            digest.update((part.len() as u64).to_le_bytes());
            digest.update(part);
        }
    }
    let inputs = crate::hex::encode_hex(&digest.finalize());
    if supplied
        .is_some_and(|value| value.split_once('.').map(|(input, _)| input) != Some(inputs.as_str()))
    {
        return Ok(revision_conflict_result(&path, OPERATION));
    }
    let measured = match super::measure_wire_runs(&before) {
        Ok(measured) => measured,
        Err(reason) => {
            return Ok(CallToolResult::error_kind(
                crate::mcp::error::ToolErrorKind::MalformedFile {
                    path: path.display().to_string(),
                    reason: reason.to_owned(),
                },
                format!("Cannot check {}: {reason}", path.display()),
            ));
        }
    };
    if variable_labels(&measured.labels) {
        return Ok(invalid_arg(
            "schematic",
            "cannot verify net names on a sheet with variable-expanded labels",
        ));
    }
    let (candidate, mut plan) =
        match prepare_candidate(&path, &source, &before, &measured, &edit, &inputs, notation) {
            Ok(plan) => plan,
            Err(PlanRefusal::Selection(error)) => {
                return Ok(invalid_arg("edit", &error.to_string()))
            }
            Err(PlanRefusal::Unsafe(error)) => {
                let reason = error.to_string();
                return Ok(CallToolResult::error_kind(
                    ToolErrorKind::PlanBlocked {
                        operation: OPERATION.to_owned(),
                        reasons: vec![reason.clone()],
                    },
                    reason,
                ));
            }
        };
    let revision = format!(
        "{inputs}.{}",
        crate::hex::encode_hex(&Sha256::digest(candidate.as_bytes()))
    );
    if supplied.is_none() {
        if args.get("acknowledge_rename").is_some() {
            return Ok(invalid_arg(
                "acknowledge_rename",
                "preview first and acknowledge only on apply",
            ));
        }
        plan["revision"] = json!(revision);
        plan["status"] = json!("preview");
    } else {
        if supplied != Some(revision.as_str()) {
            return Ok(revision_conflict_result(&path, OPERATION));
        }
        if args.get("acknowledge_rename") != (!plan["rename"].is_null()).then(|| &plan["rename"]) {
            return Ok(invalid_arg(
                "acknowledge_rename",
                "must match the preview's exact from and to names for a rename; otherwise omit it",
            ));
        }
        write_atomic_if_unchanged(&path, &source, &candidate)?;
        plan["status"] = json!("applied");
    }
    plan["schematic"] = json!(path);
    Ok(CallToolResult::json(&plan))
}

#[derive(Deserialize)]
#[serde(tag = "direction", rename_all = "snake_case", deny_unknown_fields)]
pub(super) enum Edit {
    WireToLabels {
        wire_uuids: Vec<String>,
        local_name: Option<String>,
        /// Optional final label anchors, in measured start/end order.
        anchors: Option<[Point; 2]>,
    },
    LabelsToWire {
        label_uuids: [String; 2],
    },
}

struct Changes {
    remove: Vec<String>,
    add: Vec<NewItem>,
    anchors: [Point; 2],
    label: String,
    kept_labels: Vec<String>,
    original_ends: [Point; 2],
}

#[derive(Clone)]
struct NewItem {
    id: ItemId,
    node: ItemNode,
}

fn wire(uuid: String, a: Point, b: Point) -> anyhow::Result<NewItem> {
    let mut item = Wire::new(a.0, a.1, b.0, b.1);
    item.uuid = uuid;
    Ok(NewItem {
        id: ItemId::new(&item.uuid)?,
        node: item.to_sexp(),
    })
}

fn label(uuid: String, name: &str, at: Point, leftward: bool) -> anyhow::Result<NewItem> {
    let mut item = NewLabel::new(name, at.0, at.1);
    item.uuid = uuid;
    item.set_rotation(if leftward { 180.0 } else { 0.0 });
    Ok(NewItem {
        id: ItemId::new(&item.uuid)?,
        node: item.to_sexp(),
    })
}

// KiCad's schematic writer sorts by item type and then UUID. Choose an
// insertion point without moving any retained item.
fn item_order(tag: &str) -> Option<u8> {
    match tag {
        "wire" | "bus" => Some(0),
        "image" => Some(1),
        "table" => Some(2),
        "label" => Some(3),
        "global_label" => Some(4),
        "hierarchical_label" => Some(5),
        "rule_area" => Some(6),
        "netclass_flag" => Some(7),
        "symbol" => Some(8),
        "group" => Some(9),
        "sheet" => Some(10),
        _ => None,
    }
}

fn ordered_insertions(
    source: &str,
    tree: &SexpNode,
    changes: &Changes,
) -> anyhow::Result<Vec<ItemChange>> {
    let indent = detect_indent(source);
    let mut ordered = Vec::with_capacity(changes.add.len());
    for item in &changes.add {
        let rank = item
            .node
            .tag()
            .and_then(item_order)
            .expect("exchange item type");
        let id = item.id.to_string();
        let mut anchor = ItemAnchor::BeforeFooter;
        for sibling in tree.children().unwrap_or_default() {
            let Some(sibling_rank) = sibling.head().and_then(item_order) else {
                continue;
            };
            let Some(uuid) = sibling.find_str("uuid") else {
                continue;
            };
            if !changes.remove.iter().any(|removed| removed == uuid)
                && (sibling_rank, uuid) > (rank, id.as_str())
            {
                anchor = ItemAnchor::Before(ItemId::new(uuid)?);
                break;
            }
        }
        ordered.push((
            rank,
            id,
            ItemChange {
                id: item.id.clone(),
                before: None,
                after: Some(write_schematic_item(&item.node, &indent)),
                anchor,
            },
        ));
    }
    // Insertions at one offset are applied back to front by prepare_command.
    ordered.sort_by(|a, b| (&b.0, &b.1).cmp(&(&a.0, &a.1)));
    Ok(ordered.into_iter().map(|(_, _, item)| item).collect())
}

fn item_uuid(revision: &str, index: usize) -> String {
    let hash = Sha256::digest(format!("{revision}:{index}"));
    let mut bytes = [0; 16];
    bytes.copy_from_slice(&hash[..16]);
    // UUID v8 leaves its payload up to the application.
    bytes[6] = (bytes[6] & 0x0f) | 0x80;
    bytes[8] = (bytes[8] & 0x3f) | 0x80;
    uuid::Uuid::from_bytes(bytes).to_string()
}

fn is_orthogonal(a: Point, b: Point) -> bool {
    a != b && (a.0 == b.0 || a.1 == b.1)
}

fn at(label: &Label) -> Point {
    (label.x, label.y)
}

fn native_coordinate((x, y): Point) -> bool {
    x == kicad_schematic_mm(x) && y == kicad_schematic_mm(y)
}

fn native_bus_entry_segments(tree: &SexpNode) -> impl Iterator<Item = (Point, Point)> + use<> {
    extract_bus_entries(tree).into_iter().filter_map(|entry| {
        let ((x, y), (dx, dy)) = (entry.at?, entry.size?);
        if ![x, y, dx, dy].into_iter().all(f64::is_finite) {
            return None;
        }
        // KiCad reads the two vectors separately before adding them.
        let a = (kicad_schematic_mm(x), kicad_schematic_mm(y));
        let b = (
            kicad_schematic_mm(a.0 + kicad_schematic_mm(dx)),
            kicad_schematic_mm(a.1 + kicad_schematic_mm(dy)),
        );
        Some((a, b))
    })
}

pub(super) fn native_bus_entry_wire_ends(
    tree: &SexpNode,
    buses: &[copperpour_sexp::schematic::Wire],
) -> Vec<Point> {
    native_bus_entry_segments(tree)
        .filter_map(|(a, b)| {
            let on_bus = |point| buses.iter().any(|bus| schematic_point_on_wire(point, bus));
            match (on_bus(a), on_bus(b)) {
                (true, false) => Some(b),
                (false, true) => Some(a),
                _ => None,
            }
        })
        .collect()
}

pub(super) fn default_stub_anchor(end: Point, toward: Point) -> Point {
    let (dx, dy) = (toward.0 - end.0, toward.1 - end.1);
    let length = dx.hypot(dy);
    mm(point(
        end.0 + dx * STUB_MM / length,
        end.1 + dy * STUB_MM / length,
    ))
}

pub(super) fn reusable_anchors<'a>(
    tree: &SexpNode,
    measured: &WireRuns,
    run: &WireRun,
    labels: &[&'a Label],
    explicit: Option<[Point; 2]>,
) -> [Option<&'a Label>; 2] {
    let removed: BTreeSet<_> = run.segments.iter().map(|edge| edge.wire).collect();
    let retained: Vec<_> = measured
        .wires
        .iter()
        .enumerate()
        .filter(|(i, _)| !removed.contains(i))
        .map(|(_, wire)| wire.clone())
        .collect();
    // Temporary distinct names retain geometric attachments but do not join
    // islands that are only connected by matching label text.
    let distinct: Vec<_> = labels
        .iter()
        .enumerate()
        .map(|(i, &label)| {
            let mut label = label.clone();
            label.net = i.to_string();
            label
        })
        .collect();
    let mut graph = redraw_net_graph(tree, &retained, &distinct);
    let ends = [mm(run.segments[0].a), mm(run.segments.last().unwrap().b)];
    std::array::from_fn(|i| {
        let end = ends[i];
        let root = graph.root_at(end.0, end.1);
        labels
            .iter()
            .copied()
            .filter(|label| graph.root_at(label.x, label.y) == root)
            .filter(|label| explicit.is_none_or(|points| points[i] == at(label)))
            .min_by(|a, b| {
                let distance = |label: &Label| (label.x - end.0).hypot(label.y - end.1);
                distance(a).total_cmp(&distance(b))
            })
    })
}

pub(super) fn ordinary_name(name: &str) -> bool {
    !name.trim().is_empty()
        && !name.starts_with("Net-(")
        && !name.starts_with("unconnected-(")
        && !name.contains(['[', ']', '{', '}', '/', '\\', '$'])
        && !name.chars().any(char::is_control)
}

pub(super) fn variable_labels(labels: &[Label]) -> bool {
    labels.iter().any(|label| label.net.contains("${"))
}

pub(super) fn cut_end_issue(
    end: Point,
    is_pin: bool,
    junctions: &[Point],
    wires: &[copperpour_sexp::schematic::Wire],
    removed: impl Fn(&copperpour_sexp::schematic::Wire) -> bool,
    labels: &[Label],
) -> Option<&'static str> {
    let at_end = |x, y| point(x, y) == point(end.0, end.1);
    if !is_pin && junctions.iter().any(|&(x, y)| at_end(x, y)) {
        return Some("the cut would leave a junction at the run end");
    }
    if is_pin
        || (labels.iter().any(|label| at_end(label.x, label.y))
            && wires.iter().any(|wire| {
                !removed(wire) && (at_end(wire.x1, wire.y1) || at_end(wire.x2, wire.y2))
            }))
    {
        return None;
    }
    Some("the cut would leave a dangling label or wire end")
}

fn wire_to_labels(
    tree: &SexpNode,
    measured: &WireRuns,
    uuids: &[String],
    local_name: Option<&str>,
    anchors: Option<[Point; 2]>,
    revision: &str,
) -> anyhow::Result<Vec<Changes>> {
    let unique: BTreeSet<_> = uuids.iter().map(String::as_str).collect();
    anyhow::ensure!(
        !unique.is_empty() && unique.len() == uuids.len(),
        "select distinct wire UUIDs"
    );
    let mut matched = measured.runs.iter().filter(|run| {
        super::label_advice::wire_uuids(run, &measured.wires).as_ref() == Some(&unique)
    });
    let run = matched
        .next()
        .ok_or_else(|| anyhow::anyhow!("select the exact UUIDs of one complete wire run"))?;
    anyhow::ensure!(
        matched.next().is_none(),
        "the selected UUIDs match multiple runs"
    );
    let first = &run.segments[0];
    let last = run.segments.last().expect("a run has segments");
    // A measured run can use only part of a stored wire after an interior tap
    // splits it. Removing the whole UUID in that case would remove the tap.
    anyhow::ensure!(
        run.uses_whole_wires(&measured.wires),
        "the selected run has an interior tap on a wire"
    );
    anyhow::ensure!(
        run.start == "attached" && run.end == "attached",
        "both ends must be attached and neither may be a branch"
    );

    let ends = [mm(first.a), mm(last.b)];
    let dirs = [mm(first.b), mm(last.a)];
    let mut graph = net_graph_for(tree, &measured.wires, &measured.labels);
    let root = graph.root_at(ends[0].0, ends[0].1);
    let hidden_power = super::hidden_power_roots(tree, &mut graph);
    anyhow::ensure!(
        !hidden_power.contains_key(&root),
        "implicit power nets are outside this action"
    );
    let existing: Vec<_> = measured
        .labels
        .iter()
        .filter(|label| graph.root_at(label.x, label.y) == root)
        .collect();
    let name = if let Some(existing_label) = existing.first() {
        anyhow::ensure!(
            local_name.is_none(),
            "reuse the existing local name; omit local_name"
        );
        anyhow::ensure!(
            existing
                .iter()
                .all(|label| label.kind == LabelKind::NetLabel && label.net == existing_label.net),
            "the run does not have one ordinary local name"
        );
        existing_label.net.as_str()
    } else {
        let name = local_name
            .ok_or_else(|| anyhow::anyhow!("choose a fresh local_name for this unnamed net"))?;
        let stored_name = crate::tools::sch_label_rename::stored_name;
        anyhow::ensure!(
            !measured
                .labels
                .iter()
                .any(|label| stored_name(&label.net) == stored_name(name)),
            "the local name already names another net"
        );
        name
    };
    anyhow::ensure!(
        ordinary_name(name),
        "use an ordinary local name, not a generated name, variable, or bus expression"
    );
    anyhow::ensure!(
        !hidden_power.values().flatten().any(|power| power == name),
        "the local name is also used by an implicit power net"
    );
    let net_has_bus_entry = native_bus_entry_wire_ends(tree, &extract_buses(tree))
        .into_iter()
        .any(|(x, y)| graph.root_at(x, y) == root);
    anyhow::ensure!(
        !net_has_bus_entry,
        "bus-connected nets are outside this action"
    );

    let mut kept_labels = Vec::new();
    let mut choices = [Vec::new(), Vec::new()];
    if let Some(anchors) = anchors {
        anyhow::ensure!(
            anchors.into_iter().all(native_coordinate),
            "explicit anchors must be exactly representable in KiCad schematic coordinates"
        );
    }
    for (i, nearest) in reusable_anchors(tree, measured, run, &existing, anchors)
        .into_iter()
        .enumerate()
    {
        let end = ends[i];
        if let Some(existing_label) = nearest {
            choices[i].push((at(existing_label), Vec::new()));
            kept_labels.push(
                existing_label
                    .uuid
                    .clone()
                    .ok_or_else(|| anyhow::anyhow!("the existing local label has no UUID"))?,
            );
            continue;
        }
        let candidates = if let Some(points) = anchors {
            let stub = points[i];
            let stub_length = (stub.0 - end.0).hypot(stub.1 - end.1);
            anyhow::ensure!(
                is_orthogonal(end, stub) && stub_length <= STUB_MM + 1e-9,
                "the explicit anchor must end a nonzero orthogonal stub no longer than 2.54 mm"
            );
            vec![stub]
        } else {
            let mut candidates = vec![default_stub_anchor(end, dirs[i])];
            for (dx, dy) in [
                (STUB_MM, 0.0),
                (-STUB_MM, 0.0),
                (0.0, STUB_MM),
                (0.0, -STUB_MM),
            ] {
                let alternative = mm(point(end.0 + dx, end.1 + dy));
                if !candidates.contains(&alternative) {
                    candidates.push(alternative);
                }
            }
            candidates
        };
        for stub in candidates {
            choices[i].push((
                stub,
                vec![
                    wire(item_uuid(revision, 2 * i), end, stub)?,
                    label(item_uuid(revision, 2 * i + 1), name, stub, stub.0 < end.0)?,
                ],
            ));
        }
    }
    Ok(choices[0]
        .iter()
        .flat_map(|(a, first)| {
            choices[1].iter().map(|(b, last)| Changes {
                remove: uuids.to_vec(),
                add: first.iter().chain(last).cloned().collect(),
                anchors: [*a, *b],
                label: name.to_owned(),
                kept_labels: kept_labels.clone(),
                original_ends: ends,
            })
        })
        .collect())
}

fn labels_to_wire(
    tree: &SexpNode,
    measured: &WireRuns,
    uuids: &[String; 2],
    revision: &str,
) -> anyhow::Result<Vec<Changes>> {
    anyhow::ensure!(uuids[0] != uuids[1], "select two different label UUIDs");
    let find = |uuid: &str| {
        measured
            .labels
            .iter()
            .find(|label| label.uuid.as_deref() == Some(uuid) && label.kind == LabelKind::NetLabel)
            .ok_or_else(|| anyhow::anyhow!("'{uuid}' is not an ordinary local label"))
    };
    let (a, b) = (find(&uuids[0])?, find(&uuids[1])?);
    anyhow::ensure!(
        ordinary_name(&a.net),
        "use an ordinary local name, not a generated name, variable, or bus expression"
    );
    let mut graph = net_graph_for(tree, &measured.wires, &measured.labels);
    let root = graph.root_at(a.x, a.y);
    anyhow::ensure!(
        root == graph.root_at(b.x, b.y),
        "the selected labels are not on the same net"
    );
    let hidden_power = super::hidden_power_roots(tree, &mut graph);
    anyhow::ensure!(
        !hidden_power.contains_key(&root)
            && !hidden_power.values().flatten().any(|power| power == &a.net),
        "implicit power nets are outside this action"
    );
    anyhow::ensure!(
        measured
            .labels
            .iter()
            .filter(|label| graph.root_at(label.x, label.y) == root)
            .all(|label| label.kind == LabelKind::NetLabel && label.net == a.net),
        "the selected net does not have one ordinary local name"
    );
    let (start, end) = (at(a), at(b));
    let net_has_bus_entry = native_bus_entry_wire_ends(tree, &extract_buses(tree))
        .into_iter()
        .any(|(x, y)| graph.root_at(x, y) == root);
    anyhow::ensure!(
        !net_has_bus_entry,
        "bus-connected nets are outside this action"
    );
    anyhow::ensure!(
        is_orthogonal(start, end),
        "the selected anchors do not make one direct orthogonal wire"
    );
    let pins = drawn_pin_positions(tree);
    let junctions = extract_junctions(tree);
    for anchor in [start, end] {
        let pin = pins.contains(&point(anchor.0, anchor.1));
        let touching: Vec<_> = measured
            .wires
            .iter()
            .filter(|wire| on_wire(anchor, wire))
            .collect();
        anyhow::ensure!(
            !junctions.contains(&anchor)
                && match touching.as_slice() {
                    [] => pin,
                    [wire] =>
                        !pin && (anchor == (wire.x1, wire.y1) || anchor == (wire.x2, wire.y2)),
                    _ => false,
                },
            "each selected label must sit on a pin or wire end, not a branch or through-run"
        );
    }
    let other_retained_label = measured.labels.iter().find(|label| {
        label.net == a.net
            && label.kind == LabelKind::NetLabel
            && label
                .uuid
                .as_ref()
                .is_some_and(|uuid| !uuids.contains(uuid))
            && graph.root_at(label.x, label.y) == root
    });
    let bridge = wire(item_uuid(revision, 0), start, end)?;
    let candidate = |remove, keep| Changes {
        remove,
        add: vec![bridge.clone()],
        anchors: [start, end],
        label: a.net.clone(),
        kept_labels: vec![keep],
        original_ends: [start, end],
    };
    let mut choices = Vec::new();
    if let Some(other) = other_retained_label.and_then(|label| label.uuid.clone()) {
        choices.push(candidate(uuids.to_vec(), other));
    }
    choices.push(candidate(vec![uuids[1].clone()], uuids[0].clone()));
    Ok(choices)
}

type PinKey = (String, u32, usize);
type Partition = BTreeMap<BTreeSet<PinKey>, Option<String>>;

fn pin_partition(
    notation: UnitNotation,
    tree: &SexpNode,
    measured: &WireRuns,
) -> anyhow::Result<Partition> {
    // Never prove an edit using geometry that KiCad would read differently.
    // This is exchange-only; the read-only run detector still reports the file as drawn.
    anyhow::ensure!(
        measured
            .wires
            .iter()
            .flat_map(|wire| [(wire.x1, wire.y1), (wire.x2, wire.y2)])
            .chain(measured.labels.iter().map(at))
            .chain(extract_junctions(tree))
            .all(native_coordinate),
        "cannot verify pin connectivity after KiCad rounds saved schematic coordinates"
    );
    let mut graph = net_graph_for(tree, &measured.wires, &measured.labels);
    let mut groups = BTreeMap::<NetRoot, BTreeSet<PinKey>>::new();
    let mut seen = BTreeSet::new();
    for entry in super::drawn_symbol_pin_inventory(tree) {
        let (symbol, pins) =
            entry.ok_or_else(|| anyhow::anyhow!("a placed symbol's pins could not be resolved"))?;
        let uuid = symbol
            .uuid
            .ok_or_else(|| anyhow::anyhow!("a placed symbol has no UUID"))?;
        for (ordinal, (pin, transform)) in pins.into_iter().enumerate() {
            let (x, y) = pin_endpoint(&pin, transform);
            let (native_x, native_y) = schematic_pin_endpoint(&pin, transform);
            anyhow::ensure!(
                native_coordinate(mm(point(x, y)))
                    && schematic_point(x, y) == schematic_point(native_x, native_y),
                "cannot verify pin connectivity after KiCad rounds a placed pin"
            );
            let key = (uuid.clone(), symbol.unit, ordinal);
            anyhow::ensure!(
                seen.insert(key.clone()),
                "a placed pin's identity is ambiguous"
            );
            groups.entry(graph.root_at(x, y)).or_default().insert(key);
        }
    }
    let pin_names =
        PinNetNames::build_with_pins(tree, &mut graph, notation, false, |_, symbol, lib, unit| {
            use copperpour_sexp::schematic::{lib_pin_nodes_for_body_style, parse_lib_pin};
            let alternates = placed_pin_alternates(symbol);
            lib_pin_nodes_for_body_style(lib, unit, crate::tools::symbol_body_style(symbol))
                .into_iter()
                .filter_map(|node| {
                    let mut pin = parse_lib_pin(node)?;
                    let alternate = select_pin_alternate(&mut pin, node, &alternates);
                    Some(NamingPin {
                        global_power: pin.hidden && pin.electrical_type == "power_in",
                        pin,
                        alternate_name: alternate,
                    })
                })
                .collect()
        });
    Ok(groups
        .into_iter()
        .map(|(root, pins)| {
            let name = graph
                .name_of_root(root)
                .map(|name| {
                    if measured.labels.iter().any(|label| {
                        label.net == name
                            && label.kind == LabelKind::NetLabel
                            && graph.root_at(label.x, label.y) == root
                    }) && !measured.labels.iter().any(|label| {
                        matches!(label.kind, LabelKind::GlobalLabel | LabelKind::PowerSymbol)
                            && graph.root_at(label.x, label.y) == root
                    }) {
                        format!("/{}", crate::tools::sch_label_rename::stored_name(&name))
                    } else {
                        name
                    }
                })
                .or_else(|| pin_names.pin_name_of_root(root));
            (pins, name)
        })
        .collect())
}

enum PlanRefusal {
    Selection(anyhow::Error),
    Unsafe(anyhow::Error),
}

fn prepare_candidate(
    path: &Path,
    source: &str,
    before: &SexpNode,
    measured: &WireRuns,
    edit: &Edit,
    revision: &str,
    notation: UnitNotation,
) -> Result<(String, Value), PlanRefusal> {
    let alternatives = match edit {
        Edit::WireToLabels {
            wire_uuids,
            local_name,
            anchors,
        } => wire_to_labels(
            before,
            measured,
            wire_uuids,
            local_name.as_deref(),
            *anchors,
            revision,
        ),
        Edit::LabelsToWire { label_uuids } => {
            labels_to_wire(before, measured, label_uuids, revision)
        }
    }
    .map_err(PlanRefusal::Selection)?;
    let mut first_refusal = None;
    for changes in alternatives {
        match verify_candidate(path, source, before, measured, edit, notation, changes) {
            Ok(plan) => return Ok(plan),
            Err(refusal) => {
                first_refusal.get_or_insert(refusal);
            }
        }
    }
    Err(PlanRefusal::Unsafe(
        first_refusal.expect("every edit has at least one candidate"),
    ))
}

fn verify_candidate(
    path: &Path,
    source: &str,
    before: &SexpNode,
    measured: &WireRuns,
    edit: &Edit,
    notation: UnitNotation,
    changes: Changes,
) -> anyhow::Result<(String, Value)> {
    let remove_ids = changes
        .remove
        .iter()
        .map(ItemId::new)
        .collect::<Result<Vec<_>, _>>()?;
    let mut command = SchematicCommand::delete_items(source, remove_ids, OPERATION)?
        .requiring_unchanged_document();
    let added: Vec<_> = changes.add.iter().map(|item| item.id.to_string()).collect();
    command
        .changes
        .extend(ordered_insertions(source, before, &changes)?);
    let (mut candidate, _) = prepare_command(path, source, &command)?;
    let mut removed = changes.remove;
    let forward = matches!(edit, Edit::WireToLabels { .. });
    let pins = if forward {
        drawn_pin_positions(before)
    } else {
        BTreeSet::new()
    };
    let junctions = extract_junctions(before);
    if forward {
        let pin_dots: Vec<_> = changes
            .original_ends
            .into_iter()
            .filter(|end| pins.contains(&point(end.0, end.1)) && junctions.contains(end))
            .collect();
        if !pin_dots.is_empty() {
            candidate = crate::tools::sch_wiring::reconcile_junctions_at(candidate, &pin_dots).0;
        }
    }
    let after = copperpour_sexp::parse_sexp(&candidate)?;
    let after_measured = super::measure_wire_runs(&after).map_err(anyhow::Error::msg)?;
    let before_groups = pin_partition(notation, before, measured)?;
    let after_groups = pin_partition(notation, &after, &after_measured)?;
    anyhow::ensure!(
        before_groups.keys().eq(after_groups.keys()),
        "the change would split or merge a pin net"
    );
    let mut rename = Value::Null;
    for (was, now) in before_groups
        .values()
        .zip(after_groups.values())
        .filter(|(was, now)| was != now)
    {
        let (Some(was), Some(now)) = (was, now) else {
            anyhow::bail!("the exact netlist rename could not be established");
        };
        // Selection refuses a name that already names another net. This
        // independent check verifies the saved candidate's only rename.
        anyhow::ensure!(
            rename.is_null()
                && matches!(
                    edit,
                    Edit::WireToLabels {
                        local_name: Some(_),
                        ..
                    }
                )
                && now
                    == &format!(
                        "/{}",
                        crate::tools::sch_label_rename::stored_name(&changes.label)
                    ),
            "the change would rename a net other than the selected unnamed net"
        );
        rename = json!({"from": was, "to": now});
    }
    let ids: Vec<_> = added.iter().map(String::as_str).collect();
    check_new_wire_contacts(
        before,
        measured,
        &after,
        &removed,
        &ids,
        changes.original_ends[0],
    )?;
    if forward {
        // Power pins need a junction or wire end, not label-style attachment.
        anyhow::ensure!(
            !measured.labels.iter().any(|label| {
                label.kind != LabelKind::PowerSymbol
                    && !changes.original_ends.contains(&at(label))
                    && measured.wires.iter().any(|wire| {
                        wire.uuid.as_ref().is_some_and(|id| removed.contains(id))
                            && saved_label_touches_wire(label, wire)
                    })
            }),
            "the cut would remove a wire with a label tap"
        );
        for end in changes.original_ends {
            if let Some(reason) = cut_end_issue(
                end,
                pins.contains(&point(end.0, end.1)),
                &junctions,
                &measured.wires,
                |wire| wire.uuid.as_ref().is_some_and(|id| removed.contains(id)),
                &measured.labels,
            ) {
                anyhow::bail!(reason);
            }
        }
    }
    let after_dots = extract_junctions(&after);
    for node in before.find_all("junction") {
        if parse_at(node).is_some_and(|(x, y, _)| {
            changes.original_ends.contains(&(x, y)) && !after_dots.contains(&(x, y))
        }) {
            let id = node
                .find_str("uuid")
                .ok_or_else(|| anyhow::anyhow!("a removed junction has no UUID"))?;
            removed.push(id.to_owned());
        }
    }
    let clearance = crate::tools::sch_analysis::added_clearance_findings(before, &after, &ids);
    anyhow::ensure!(
        clearance.bounds_unresolved.is_empty(),
        "cannot establish clearance: unresolved bounds for {}",
        clearance.bounds_unresolved.join(", ")
    );
    anyhow::ensure!(
        clearance.findings.is_empty(),
        "the new wires or labels do not clear: {}",
        json!(clearance.findings)
    );
    let counts = |runs: &WireRuns| {
        json!({
            "wires": runs.wires.len(),
            "local_labels": runs.labels.iter().filter(|label| label.kind == LabelKind::NetLabel).count(),
        })
    };
    let kept_wires: Vec<_> = measured
        .wires
        .iter()
        .filter_map(|wire| wire.uuid.as_deref())
        .filter(|id| !removed.iter().any(|removed| removed == id))
        .collect();
    Ok((
        candidate,
        json!({
            "anchors": changes.anchors,
            "label": { "text": changes.label, "kind": "local" },
            "removed_uuids": removed,
            "kept_wire_uuids": kept_wires,
            "kept_label_uuids": changes.kept_labels,
            "added_uuids": added,
            "rename": rename,
            "before": counts(measured),
            "after": counts(&after_measured),
            "layout_score": {
                "before": crate::tools::sch_layout_score::measure(before).json(),
                "after": crate::tools::sch_layout_score::measure(&after).json(),
            },
        }),
    ))
}

fn on_wire(p: Point, w: &copperpour_sexp::schematic::Wire) -> bool {
    point_on_segment(p.0, p.1, w.x1, w.y1, w.x2, w.y2, 0.5 / super::NM_PER_MM)
}

fn drawn_pin_positions(tree: &SexpNode) -> BTreeSet<super::Point> {
    super::drawn_symbol_pin_inventory(tree)
        .into_iter()
        .flatten()
        .flat_map(|(_, pins)| pins)
        .map(|(pin, transform)| {
            let (x, y) = pin_endpoint(&pin, transform);
            point(x, y)
        })
        .collect()
}

fn check_new_wire_contacts(
    before: &SexpNode,
    measured: &WireRuns,
    after: &SexpNode,
    removed: &[String],
    added: &[&str],
    endpoint: Point,
) -> anyhow::Result<()> {
    let mut graph = net_graph_for(before, &measured.wires, &measured.labels);
    let root = graph.root_at(endpoint.0, endpoint.1);
    let kept: Vec<_> = measured
        .wires
        .iter()
        .filter(|wire| {
            !wire
                .uuid
                .as_ref()
                .is_some_and(|uuid| removed.contains(uuid))
        })
        .cloned()
        .collect();
    let roots: Vec<_> = kept
        .iter()
        .map(|wire| graph.root_at(wire.x1, wire.y1))
        .collect();
    let kept_labels: Vec<_> = measured
        .labels
        .iter()
        .filter(|label| !label.uuid.as_ref().is_some_and(|id| removed.contains(id)))
        .collect();
    let foreign_labels: Vec<_> = kept_labels
        .iter()
        .filter(|label| graph.root_at(label.x, label.y) != root)
        .collect();
    let buses = extract_buses(before);
    let bus_entries: Vec<_> = native_bus_entry_segments(before).collect();
    let taps = extract_junctions(before)
        .into_iter()
        .chain(
            kept.iter()
                .flat_map(|wire| [(wire.x1, wire.y1), (wire.x2, wire.y2)]),
        )
        .chain(
            super::drawn_symbol_pin_inventory(before)
                .into_iter()
                .flatten()
                .flat_map(|(_, pins)| pins)
                .map(|(pin, transform)| pin_endpoint(&pin, transform)),
        )
        .collect::<Vec<_>>();
    let new: Vec<_> = extract_wires(after)
        .into_iter()
        .filter(|wire| {
            wire.uuid
                .as_deref()
                .is_some_and(|uuid| added.contains(&uuid))
        })
        .collect();
    for (i, wire) in new.iter().enumerate() {
        let start = (wire.x1, wire.y1);
        let end = (wire.x2, wire.y2);
        // Conservatively reserve the bounding rectangle of a diagonal bus or entry.
        let ranges_overlap =
            |a: f64, b: f64, c: f64, d: f64| a.min(b) <= c.max(d) && c.min(d) <= a.max(b);
        let overlaps_bus =
            |a, b, c, d| ranges_overlap(a, b, kicad_schematic_mm(c), kicad_schematic_mm(d));
        anyhow::ensure!(
            !buses
                .iter()
                .any(|bus| overlaps_bus(wire.x1, wire.x2, bus.x1, bus.x2)
                    && overlaps_bus(wire.y1, wire.y2, bus.y1, bus.y2)),
            "the new wire would cross or touch a bus"
        );
        anyhow::ensure!(
            !bus_entries.iter().any(|&(a, b)| {
                ranges_overlap(wire.x1, wire.x2, a.0, b.0)
                    && ranges_overlap(wire.y1, wire.y2, a.1, b.1)
            }),
            "the new wire would touch a bus entry"
        );
        anyhow::ensure!(
            new[i + 1..].iter().all(|other| {
                !on_wire(start, other)
                    && !on_wire(end, other)
                    && !on_wire((other.x1, other.y1), wire)
                    && !on_wire((other.x2, other.y2), wire)
            }) && super::label_advice::wire_crossings(
                &[start, end],
                None,
                &new[i + 1..],
                &vec![root; new.len() - i - 1],
            )
            .is_empty(),
            "the two new stubs would cross or touch"
        );
        anyhow::ensure!(
            super::label_advice::wire_crossings(&[start, end], None, &kept, &roots).is_empty(),
            "the new wire would cross another wire"
        );
        anyhow::ensure!(
            !taps
                .iter()
                .any(|&p| point(p.0, p.1) != point(start.0, start.1)
                    && point(p.0, p.1) != point(end.0, end.1)
                    && on_wire(p, wire))
                && !kept_labels.iter().any(|label| {
                    at(label) != start && at(label) != end && saved_label_touches_wire(label, wire)
                }),
            "the new wire would have an interior tap"
        );
        anyhow::ensure!(
            !foreign_labels
                .iter()
                .any(|label| saved_label_touches_wire(label, wire)),
            "the new wire would touch a foreign label"
        );
        for (other, &other_root) in kept.iter().zip(&roots) {
            if root != other_root {
                anyhow::ensure!(
                    !on_wire(start, other)
                        && !on_wire(end, other)
                        && !on_wire((other.x1, other.y1), wire)
                        && !on_wire((other.x2, other.y2), wire),
                    "the new wire would touch a foreign net"
                );
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mcp::handler::McpHandler;
    use std::{fs, path::PathBuf};

    const CROSSING: &[u8] = include_bytes!(
        "../../../tests/fixtures/labels_vs_wires_kicad10/crossing/crossing.kicad_sch"
    );
    const CROSSING_PROJECT: &str =
        include_str!("../../../tests/fixtures/labels_vs_wires_kicad10/crossing/crossing.kicad_pro");
    const ADJACENT_PROJECT: &str =
        include_str!("../../../tests/fixtures/labels_vs_wires_kicad10/adjacent/adjacent.kicad_pro");
    const ADJACENT: &[u8] = include_bytes!(
        "../../../tests/fixtures/labels_vs_wires_kicad10/adjacent/adjacent.kicad_sch"
    );
    const NAMES: &[u8] =
        include_bytes!("../../../tests/fixtures/labels_vs_wires_kicad10/names/names.kicad_sch");

    type ErcFindings = Vec<(String, String, Vec<(String, Option<String>)>)>;

    struct Sheet {
        _dir: tempfile::TempDir,
        path: PathBuf,
        handler: McpHandler,
    }

    impl Sheet {
        async fn new(fixture: &[u8]) -> Self {
            Self::named("exchange.kicad_sch", fixture).await
        }

        async fn named(name: &str, fixture: &[u8]) -> Self {
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join(name);
            fs::write(&path, fixture).unwrap();
            let handler = McpHandler::new(crate::tools::ServerConfig {
                eager_toolsets: true,
                ..Default::default()
            })
            .await
            .unwrap();
            Self {
                _dir: dir,
                path,
                handler,
            }
        }

        async fn call(&self, mut arguments: Value) -> (Value, Value) {
            self.tool("exchange_schematic_wires_and_labels", &mut arguments)
                .await
        }

        async fn tool(&self, name: &str, arguments: &mut Value) -> (Value, Value) {
            arguments["schematic"] = json!(self.path);
            super::super::tests::served(&self.handler, name, arguments.clone()).await
        }

        async fn readonly(&self, args: Value) -> (Value, Value) {
            let before = fs::read(&self.path).unwrap();
            let response = self.call(args).await;
            assert_eq!(fs::read(&self.path).unwrap(), before);
            response
        }

        async fn apply(&self, edit: Value) -> Value {
            let (result, preview) = self.readonly(json!({"edit":edit})).await;
            assert_ne!(result["isError"], true, "{preview:#}");
            let mut args = json!({"edit":edit, "revision":preview["revision"]});
            if !preview["rename"].is_null() {
                args["acknowledge_rename"] = preview["rename"].clone();
            }
            let (result, applied) = self.call(args).await;
            assert_ne!(result["isError"], true, "{applied:#}");
            assert_eq!(applied["status"], "applied");
            applied
        }

        async fn add_label(&self, net: &str, x: f64, y: f64) {
            let (reply, body) = self
                .tool(
                    "add_schematic_net_label",
                    &mut json!({"net":net, "x":x, "y":y}),
                )
                .await;
            assert_ne!(reply["isError"], true, "{body:#}");
        }

        async fn native_nets(&self, cli: &str) -> BTreeMap<String, BTreeSet<(String, String)>> {
            let output = self._dir.path().join("exchange.net");
            crate::tools::cli::export_netlist(cli, &self.path, &output, "kicadsexpr")
                .await
                .unwrap();
            let tree = copperpour_sexp::parse_sexp(&fs::read_to_string(output).unwrap()).unwrap();
            let nets: BTreeMap<String, BTreeSet<(String, String)>> = tree
                .find("nets")
                .unwrap()
                .find_all("net")
                .into_iter()
                .map(|net| {
                    (
                        net.find_str("name").unwrap().to_owned(),
                        net.find_all("node")
                            .into_iter()
                            .map(|node| {
                                (
                                    node.find_str("ref").unwrap().to_owned(),
                                    node.find_str("pin").unwrap().to_owned(),
                                )
                            })
                            .collect(),
                    )
                })
                .collect();
            eprintln!("KiCad netlist: {}", serde_json::to_string(&nets).unwrap());
            nets
        }

        async fn native_erc(&self, cli: &str) -> ErcFindings {
            let mut findings: ErcFindings = crate::tools::cli::run_erc(cli, &self.path)
                .await
                .unwrap()
                .violations
                .into_iter()
                .map(|finding| {
                    let mut items: Vec<_> = finding
                        .items
                        .into_iter()
                        .map(|item| (item.description, item.uuid))
                        .collect();
                    items.sort_unstable();
                    (finding.rule, finding.severity, items)
                })
                .collect();
            findings.sort_unstable();
            eprintln!("KiCad ERC: {}", serde_json::to_string(&findings).unwrap());
            findings
        }

        fn native_resave(&self, cli: &str) -> Vec<u8> {
            let scratch = tempfile::tempdir().unwrap();
            let copy = scratch.path().join(self.path.file_name().unwrap());
            fs::copy(&self.path, &copy).unwrap();
            let project = self.path.with_extension("kicad_pro");
            if project.is_file() {
                fs::copy(project, copy.with_extension("kicad_pro")).unwrap();
            }
            let output = std::process::Command::new(cli)
                .args(["sch", "upgrade", "--force"])
                .current_dir(scratch.path())
                .arg(copy.file_name().unwrap())
                .output()
                .unwrap();
            assert!(
                output.status.success(),
                "{}",
                String::from_utf8_lossy(&output.stderr)
            );
            fs::read(copy).unwrap()
        }
    }

    fn crossing_edit() -> Value {
        json!({
            "direction": "wire_to_labels",
            "wire_uuids": ["0bb4199c-0170-4fa5-beb8-87ea04205c3a"]
        })
    }

    fn reverse_edit() -> Value {
        json!({
            "direction":"labels_to_wire",
            "label_uuids":["30544d14-6e0a-47a2-aa69-bec165fbf2af", "1c5fd64f-c1d6-4285-bb22-fd1d9ed8de01"]
        })
    }

    fn names_edit() -> Value {
        json!({
            "direction":"wire_to_labels",
            "wire_uuids":["566e47a9-13f9-4bfe-8770-9f907db66b32"],
            "local_name":"SIG_NEW"
        })
    }

    #[tokio::test]
    async fn exchange_writes_kicad_style_blocks_before_symbols() {
        let sheet = Sheet::named("crossing.kicad_sch", CROSSING).await;
        let applied = sheet.apply(crossing_edit()).await;
        let saved = fs::read_to_string(&sheet.path).unwrap();
        let first_symbol = saved.find("\n\t(symbol\n").unwrap();
        let mut kinds = Vec::new();
        for uuid in applied["added_uuids"].as_array().unwrap() {
            let needle = format!("\n\t\t(uuid \"{}\")\n\t)\n", uuid.as_str().unwrap());
            let (prefix, _) = saved
                .split_once(&needle)
                .expect("KiCad UUID and closing indent");
            let (head, fields) = prefix
                .rsplit_once("\n\t(")
                .expect("sheet-child indentation");
            assert!(prefix.len() < first_symbol, "new item must precede symbols");
            let (kind, children) = fields.split_once('\n').unwrap();
            assert!(children.lines().all(|line| line.starts_with("\t\t")));
            kinds.push(kind);
            if kind == "wire" {
                assert!(
                    head.rfind("\n\t(label ").is_none(),
                    "wire must precede labels"
                );
            }
        }
        kinds.sort_unstable();
        assert_eq!(kinds, ["label \"SIG_A\"", "wire"]);
    }

    #[tokio::test]
    #[ignore = "requires KiCad 10 via COPPERPOUR_TEST_KICAD_CLI"]
    async fn native_exchange_preserves_pin_groups_and_file_format() {
        let cli = std::env::var("COPPERPOUR_TEST_KICAD_CLI").expect("set the KiCad CLI path");
        for (name, bytes, project, edit) in [
            ("crossing", CROSSING, CROSSING_PROJECT, crossing_edit()),
            ("adjacent", ADJACENT, ADJACENT_PROJECT, reverse_edit()),
        ] {
            let sheet = Sheet::named(&format!("{name}.kicad_sch"), bytes).await;
            fs::write(sheet.path.with_extension("kicad_pro"), project).unwrap();
            assert!(
                sheet.native_resave(&cli) == bytes,
                "{name} input changed on resave"
            );
            let nets = sheet.native_nets(&cli).await;
            let erc = sheet.native_erc(&cli).await;
            sheet.apply(edit).await;
            assert_eq!(sheet.native_nets(&cli).await, nets);
            assert_eq!(sheet.native_erc(&cli).await, erc);
            let saved = fs::read(&sheet.path).unwrap();
            let resaved = sheet.native_resave(&cli);
            let first = saved.iter().zip(&resaved).position(|(a, b)| a != b);
            assert!(saved == resaved, "{name} resave changed bytes at {first:?}");
        }
        let sheet = Sheet::named("names.kicad_sch", NAMES).await;
        let mut nets = sheet.native_nets(&cli).await;
        let pins = nets.remove("Net-(R1-Pad2)").unwrap();
        assert!(nets.insert("/SIG_NEW".into(), pins).is_none());
        sheet.apply(names_edit()).await;
        assert_eq!(sheet.native_nets(&cli).await, nets);
        let (sheet, edit) = alternate_case().await;
        let mut nets = sheet.native_nets(&cli).await;
        let pins = nets.remove("Net-(J2-SWCLK-Pad4)").unwrap();
        assert!(nets.insert("/TEST_ALT".into(), pins).is_none());
        assert_eq!(
            sheet.apply(edit).await["rename"],
            json!({"from":"Net-(J2-SWCLK-Pad4)", "to":"/TEST_ALT"})
        );
        assert_eq!(sheet.native_nets(&cli).await, nets);
        let sheet = clearance_sheet().await;
        fs::write(sheet.path.with_extension("kicad_pro"), ADJACENT_PROJECT).unwrap();
        fs::write(&sheet.path, sheet.native_resave(&cli)).unwrap();
        assert_unresolved_clearance_refusal(&sheet).await;
        for (fixture, edit, entry, bus) in bus_net_cases() {
            let sheet = Sheet::new(fixture).await;
            add_bus_wire_entry(&sheet, entry, bus).await;
            fs::write(&sheet.path, sheet.native_resave(&cli)).unwrap();
            assert_bus_net_refusal(&sheet, edit).await;
        }
    }

    fn wire_length(tree: &SexpNode) -> f64 {
        extract_wires(tree)
            .iter()
            .map(|wire| (wire.x1 - wire.x2).hypot(wire.y1 - wire.y2))
            .sum()
    }

    #[track_caller]
    fn assert_new_wire_stroke(before: &SexpNode, after: &SexpNode, applied: &Value) {
        let prior: BTreeMap<_, _> = before
            .find_all("wire")
            .into_iter()
            .map(|wire| (wire.find_str("uuid").unwrap(), wire))
            .collect();
        let expected = copperpour_sexp::parse_sexp("(stroke (width 0) (type default))").unwrap();
        let mut new_count = 0;
        for wire in after.find_all("wire") {
            let uuid = wire.find_str("uuid").unwrap();
            if let Some(old) = prior.get(uuid) {
                assert_eq!(wire, *old, "kept wire {uuid} changed");
            } else {
                assert!(applied["added_uuids"]
                    .as_array()
                    .unwrap()
                    .contains(&json!(uuid)));
                assert_eq!(
                    wire.find("stroke"),
                    Some(&expected),
                    "new wire {uuid} stroke"
                );
                new_count += 1;
            }
        }
        assert_eq!(new_count, 1);
    }

    #[tokio::test]
    async fn reuse_the_local_name_and_apply_exactly_the_previewed_items() {
        let sheet = Sheet::named("crossing.kicad_sch", CROSSING).await;
        let (_, before) = read_schematic(&sheet.path).unwrap();
        let edit = crossing_edit();
        let (result, preview) = sheet.readonly(json!({"edit":edit})).await;
        assert_ne!(result["isError"], true, "{preview:#}");
        assert_eq!(preview["status"], "preview");
        assert!(preview["rename"].is_null());
        assert_eq!(preview["layout_score"]["before"]["wire_length_mm"], 200.66);
        assert_eq!(preview["layout_score"]["after"]["wire_length_mm"], 59.69);
        assert_eq!(preview["layout_score"]["before"]["wire_segments"], 8);
        assert_eq!(preview["layout_score"]["after"]["wire_segments"], 8);
        let apply = json!({"edit":edit, "revision":preview["revision"]});
        let (result, applied) = sheet.call(apply.clone()).await;
        assert_ne!(result["isError"], true, "{applied:#}");
        assert_eq!(applied["status"], "applied");
        assert_eq!(applied["added_uuids"], preview["added_uuids"]);
        let (_, tree) = read_schematic(&sheet.path).unwrap();
        assert_new_wire_stroke(&before, &tree, &applied);
        let new = preview["added_uuids"].as_array().unwrap();
        for id in new {
            assert!(tree
                .children()
                .unwrap()
                .iter()
                .any(|n| n.find_str("uuid") == id.as_str()));
        }
        assert_eq!(tree.find_all("label").len(), 2);
        assert!(!extract_wires(&tree)
            .iter()
            .any(|w| w.uuid.as_deref() == edit["wire_uuids"][0].as_str()));
        assert_eq!(before.find_all("symbol"), tree.find_all("symbol"));
        assert!((wire_length(&before) - 200.66).abs() < 1e-6);
        assert!((wire_length(&tree) - 59.69).abs() < 1e-6);
        let (result, refused) = sheet.readonly(apply).await;
        assert_eq!(result["isError"], true, "{refused:#}");
        assert_eq!(refused["error"]["kind"], "conflict");

        let saved = fs::read(&sheet.path).unwrap();
        let (result, scan) = super::super::tests::served(
            &sheet.handler,
            "check_schematic_wire_runs",
            json!({
                "schematic":sheet.path,
                "label_advice": {
                    "long_run_mm":100.0,
                    "region":{"min":[50.0,70.0],"max":[215.0,115.0]}
                }
            }),
        )
        .await;
        assert_ne!(result["isError"], true, "{scan:#}");
        assert!(scan["label_advice"].as_array().unwrap().iter().all(|run| {
            run["recommendation"] != "consider_labels" && run["foreign_crossings"] == json!([])
        }));
        assert_eq!(fs::read(&sheet.path).unwrap(), saved);
    }

    #[tokio::test]
    async fn exchange_refuses_unreadable_inputs_with_typed_errors() {
        for (bytes, kind) in [
            (None, "file_not_found"),
            (Some(&b"(kicad_sch \xff)"[..]), "malformed_file"),
            (Some(&b"(kicad_sch"[..]), "malformed_file"),
            (
                Some(&b"(kicad_pcb (version 20241229))"[..]),
                "malformed_file",
            ),
        ] {
            let sheet = Sheet::new(bytes.unwrap_or_default()).await;
            if bytes.is_none() {
                fs::remove_file(&sheet.path).unwrap();
            }
            let (result, body) = sheet.call(json!({"edit":crossing_edit()})).await;
            assert_eq!(result["isError"], true, "{body:#}");
            assert_eq!(body["error"]["kind"], kind, "{body:#}");
            let expected_path = sheet
                .path
                .canonicalize()
                .unwrap_or_else(|_| sheet.path.clone());
            assert_eq!(
                body["error"]["path"],
                expected_path.to_string_lossy().as_ref()
            );
            if let Some(bytes) = bytes {
                assert_eq!(fs::read(&sheet.path).unwrap(), bytes);
            } else {
                assert_eq!(fs::read_dir(sheet._dir.path()).unwrap().count(), 0);
            }
        }
    }

    #[tokio::test]
    async fn exchange_refuses_non_regular_targets_before_reading() {
        let sheet = Sheet::new(CROSSING).await;
        let target = sheet.path.with_extension("bak");
        fs::rename(&sheet.path, &target).unwrap();
        fs::create_dir(&sheet.path).unwrap();
        let refuse = || async {
            let (result, body) = sheet.call(json!({"edit":crossing_edit()})).await;
            assert_eq!(result["isError"], true, "{body:#}");
            assert_eq!(body["error"]["kind"], "invalid_write_target", "{body:#}");
        };
        refuse().await;
        assert_eq!(fs::read_dir(&sheet.path).unwrap().count(), 0);
        #[cfg(unix)]
        {
            fs::remove_dir(&sheet.path).unwrap();
            std::os::unix::fs::symlink(&target, &sheet.path).unwrap();
            refuse().await;
            assert_eq!(fs::read(&target).unwrap(), CROSSING);
        }
    }

    #[tokio::test]
    async fn exchange_refuses_invalid_wire_selection_and_name() {
        let sheet = Sheet::new(CROSSING).await;
        let edit = crossing_edit();
        for (fields, reason) in [
            (
                json!({"wire_uuids":["not-a-saved-wire"]}),
                "select the exact UUIDs of one complete wire run",
            ),
            (
                json!({"local_name":"SIG_A"}),
                "reuse the existing local name; omit local_name",
            ),
            (
                json!({"anchors":[[55.88,96.52],[199.4,96.53]]}),
                "the explicit anchor must end a nonzero orthogonal stub",
            ),
            (
                json!({"anchors":[[55.88,96.52],[199.39,93.98004]]}),
                "explicit anchors must be exactly representable in KiCad schematic coordinates",
            ),
        ] {
            let mut edit = edit.clone();
            edit.as_object_mut()
                .unwrap()
                .extend(fields.as_object().unwrap().clone());
            let (result, body) = sheet.readonly(json!({"edit":edit})).await;
            assert_eq!(result["isError"], true, "{body:#}");
            assert_eq!(body["error"]["kind"], "invalid_argument", "{body:#}");
            assert!(
                body["error"]["reason"]
                    .as_str()
                    .unwrap()
                    .starts_with(reason),
                "{body:#}"
            );
        }
    }

    #[tokio::test]
    async fn reverse_refuses_duplicate_and_unknown_label_ids() {
        let sheet = Sheet::new(ADJACENT).await;
        let edit = reverse_edit();
        let first = &edit["label_uuids"][0];
        for (ids, reason) in [
            (json!([first, first]), "select two different label UUIDs"),
            (
                json!([first, "not-a-local-label"]),
                "'not-a-local-label' is not an ordinary local label",
            ),
        ] {
            let (result, body) = sheet
                .readonly(json!({"edit":{"direction":"labels_to_wire","label_uuids":ids}}))
                .await;
            assert_eq!(result["isError"], true, "{body:#}");
            assert_eq!(body["error"]["kind"], "invalid_argument", "{body:#}");
            assert_eq!(body["error"]["reason"], reason);
        }
    }

    fn bus_net_cases() -> [(&'static [u8], Value, Point, Point); 2] {
        [
            (CROSSING, crossing_edit(), (55.88, 96.52), (58.42, 99.06)),
            (ADJACENT, reverse_edit(), (105.41, 88.9), (107.95, 91.44)),
        ]
    }

    async fn add_bus_wire_entry(sheet: &Sheet, (x, y): Point, (bus_x, bus_y): Point) {
        for (tool, mut args) in [
            (
                "add_bus",
                json!({"x1":bus_x,"y1":bus_y,"x2":bus_x,"y2":bus_y + 2.54}),
            ),
            (
                "add_bus_entry",
                json!({"x":x,"y":y,"direction":"down_right"}),
            ),
        ] {
            let (result, body) = sheet.tool(tool, &mut args).await;
            assert_ne!(result["isError"], true, "{body:#}");
        }
    }

    async fn assert_bus_net_refusal(sheet: &Sheet, edit: Value) {
        let (result, body) = sheet.readonly(json!({"edit":edit})).await;
        assert_eq!(result["isError"], true, "{body:#}");
        assert_eq!(body["error"]["kind"], "invalid_argument");
        assert_eq!(
            body["error"]["reason"],
            "bus-connected nets are outside this action"
        );
    }

    #[tokio::test]
    async fn either_direction_refuses_a_bus_connected_net() {
        for (fixture, edit, (x, y), bus) in bus_net_cases() {
            for entry_x in [x, x + 0.00004] {
                let sheet = Sheet::new(fixture).await;
                add_bus_wire_entry(&sheet, (entry_x, y), bus).await;
                assert_bus_net_refusal(&sheet, edit.clone()).await;
                if edit["direction"] == "wire_to_labels" {
                    let (result, body) = sheet
                        .tool(
                            "check_schematic_wire_runs",
                            &mut json!({
                                "label_advice":{"long_run_mm":100,"wire_uuids":edit["wire_uuids"]}
                            }),
                        )
                        .await;
                    assert_ne!(result["isError"], true, "{body:#}");
                    assert_eq!(
                        body["label_advice"][0]["recommendation"], "keep_wire",
                        "{body:#}"
                    );
                    assert_eq!(
                        body["label_advice"][0]["reason"],
                        "unsupported_scope_or_aliases"
                    );
                }
            }
        }
    }

    #[tokio::test]
    async fn apply_refuses_a_stale_revision_before_touching_the_target() {
        let sheet = Sheet::new(CROSSING).await;
        let edit = crossing_edit();
        let (result, preview) = sheet.readonly(json!({"edit":edit})).await;
        assert_ne!(result["isError"], true, "{preview:#}");
        let revision = preview["revision"].as_str().unwrap();
        let (inputs, _) = revision.split_once('.').unwrap();
        for (stale, changed_sheet) in [
            (format!("{inputs}.different_candidate"), false),
            (revision.to_owned(), true),
        ] {
            if changed_sheet {
                fs::write(&sheet.path, [CROSSING, b"\n"].concat()).unwrap();
            }
            let (result, refused) = sheet.readonly(json!({"edit":edit, "revision":stale})).await;
            assert_eq!(result["isError"], true, "{refused:#}");
            assert_eq!(refused["error"]["kind"], "conflict");
        }
    }

    #[tokio::test]
    async fn changing_only_the_project_invalidates_the_preview() {
        let sheet = Sheet::named("crossing.kicad_sch", CROSSING).await;
        let project = sheet.path.with_extension("kicad_pro");
        fs::write(&project, CROSSING_PROJECT).unwrap();
        let edit = crossing_edit();
        let (result, preview) = sheet.readonly(json!({"edit":edit})).await;
        assert_ne!(result["isError"], true, "{preview:#}");
        let newer = format!("{CROSSING_PROJECT}\n");
        fs::write(&project, &newer).unwrap();
        let (result, body) = sheet
            .readonly(json!({"edit":edit,"revision":preview["revision"]}))
            .await;
        assert_eq!(result["isError"], true, "{body:#}");
        assert_eq!(body["error"]["kind"], "conflict");
        assert_eq!(fs::read_to_string(&project).unwrap(), newer);
    }

    #[tokio::test]
    async fn a_child_sheet_cannot_be_exchanged_in_isolation() {
        let sheet = Sheet::named(
            "child.kicad_sch",
            include_bytes!("../../../tests/fixtures/top_level_sheets_kicad10/child.kicad_sch"),
        )
        .await;
        let root =
            include_bytes!("../../../tests/fixtures/top_level_sheets_kicad10/toplevel.kicad_sch");
        let project =
            include_bytes!("../../../tests/fixtures/top_level_sheets_kicad10/toplevel.kicad_pro");
        fs::write(sheet._dir.path().join("toplevel.kicad_sch"), root).unwrap();
        fs::write(sheet._dir.path().join("toplevel.kicad_pro"), project).unwrap();
        let (result, body) = sheet.readonly(json!({"edit":crossing_edit()})).await;
        assert_eq!(result["isError"], true, "{body:#}");
        assert_eq!(body["error"]["reason"], "select a standalone KiCad sheet");
        assert_eq!(
            fs::read(sheet._dir.path().join("toplevel.kicad_sch")).unwrap(),
            root
        );
        assert_eq!(
            fs::read(sheet._dir.path().join("toplevel.kicad_pro")).unwrap(),
            project
        );
    }

    #[tokio::test]
    async fn a_save_before_readback_is_not_reported_as_success() {
        let sheet = Sheet::new(CROSSING).await;
        let edit = crossing_edit();
        let (result, preview) = sheet.readonly(json!({"edit":edit})).await;
        assert_ne!(result["isError"], true, "{preview:#}");
        let altered = format!("{}\n", std::str::from_utf8(CROSSING).unwrap());
        copperpour_sexp::writer::test_hooks::save_after_replace(&sheet.path, altered.clone());
        let (result, body) = sheet
            .call(json!({"edit":edit, "revision":preview["revision"]}))
            .await;
        assert_eq!(result["isError"], true, "{body:#}");
        assert_eq!(body["error"]["kind"], "mutation_outcome_uncertain");
        assert_eq!(fs::read_to_string(&sheet.path).unwrap(), altered);
    }

    #[tokio::test]
    async fn unresolved_variable_names_prevent_a_proven_fresh_name() {
        let sheet = Sheet::new(NAMES).await;
        sheet.add_label("${ALIAS}", 80.01, 111.76).await;
        let (result, body) = sheet
            .readonly(json!({"edit":{
                "direction":"wire_to_labels",
                "wire_uuids":["566e47a9-13f9-4bfe-8770-9f907db66b32"],
                "local_name":"SIG_NEW"
            }}))
            .await;
        assert_eq!(result["isError"], true, "{body:#}");
        assert!(body["message"]
            .as_str()
            .unwrap()
            .contains("variable-expanded"));
    }

    #[tokio::test]
    async fn an_unnamed_net_requires_acknowledgement_of_both_exact_names() {
        let sheet = Sheet::new(NAMES).await;
        let mut unnamed = names_edit();
        unnamed.as_object_mut().unwrap().remove("local_name");
        let (result, body) = sheet.readonly(json!({"edit":unnamed})).await;
        assert_eq!(result["isError"], true, "{body:#}");
        assert_eq!(
            body["error"]["reason"],
            "choose a fresh local_name for this unnamed net"
        );
        let edit = names_edit();
        let (result, preview) = sheet.readonly(json!({"edit":edit})).await;
        assert_ne!(result["isError"], true, "{preview:#}");
        assert_eq!(
            preview["rename"],
            json!({"from":"Net-(R1-Pad2)", "to":"/SIG_NEW"})
        );
        for ack in [
            None,
            Some(json!({"from":"wrong", "to":"/SIG_NEW"})),
            Some(json!({"from":"Net-(R1-Pad2)", "to":"wrong"})),
        ] {
            let mut args = json!({"edit":edit,"revision":preview["revision"]});
            if let Some(ack) = ack {
                args["acknowledge_rename"] = ack;
            }
            let (result, refused) = sheet.readonly(args).await;
            assert_eq!(result["isError"], true, "{refused:#}");
            assert_eq!(refused["error"]["kind"], "invalid_argument");
        }
        let (result, applied) = sheet
            .call(json!({
                "edit":edit, "revision":preview["revision"], "acknowledge_rename":preview["rename"]
            }))
            .await;
        assert_ne!(result["isError"], true, "{applied:#}");
        assert_eq!(applied["rename"], preview["rename"]);
        let sheet = Sheet::new(CROSSING).await;
        let edit = crossing_edit();
        let (result, preview) = sheet.readonly(json!({"edit":edit})).await;
        assert_ne!(result["isError"], true, "{preview:#}");
        for revision in [None, Some(&preview["revision"])] {
            let mut args = json!({
                "edit":edit,"acknowledge_rename":{"from":"/SIG_A","to":"/SIG_A"}
            });
            if let Some(revision) = revision {
                args["revision"] = revision.clone();
            }
            let (result, refusal) = sheet.readonly(args).await;
            assert_eq!(result["isError"], true, "{refusal:#}");
            assert_eq!(refusal["error"]["field"], "acknowledge_rename");
        }
    }

    async fn alternate_case() -> (Sheet, Value) {
        // Same temporary derivative as the connector-pin-signals native oracle.
        let fixture =
            include_str!("../../../tests/fixtures/connector_pin_signals_kicad10.kicad_sch");
        let old = "(pin \"4\"\n\t\t\t(uuid \"431beb79-2289-47fc-9e6e-d4c5dea03cee\")";
        let new = "(pin \"4\"\n\t\t\t(alternate \"TCK\")\n\t\t\t(uuid \"431beb79-2289-47fc-9e6e-d4c5dea03cee\")";
        assert_eq!(fixture.matches(old).count(), 1);
        let fixture = fixture
            .replace(old, new)
            .replace("(name \"SWDIO\"", "(name \"TCK\"");
        let library = "(symbol \"Connector_Generic:Conn_01x02\"";
        let (head, tail) = fixture.split_once(library).unwrap();
        let fixture = format!(
            "{head}{library}{}",
            tail.replacen("(name \"Pin_1\"", "(name \"1\"", 1)
        );
        let sheet = Sheet::new(fixture.as_bytes()).await;
        (
            sheet,
            json!({
                "direction": "wire_to_labels",
                "wire_uuids": [
                    "8b7b2067-9304-4d41-b2b2-43a15e04e352",
                    "57c2469f-55cd-42c0-b1b8-f43303933fd5",
                    "bc885cdd-1070-4926-88e9-74e5e1643323"
                ],
                "local_name": "TEST_ALT"
            }),
        )
    }

    #[tokio::test]
    async fn a_duplicate_shown_alternate_requires_the_exported_pad_suffix() {
        let (sheet, edit) = alternate_case().await;
        let (result, preview) = sheet.readonly(json!({"edit":edit})).await;
        assert_ne!(result["isError"], true, "{preview:#}");
        let ack = json!({"from":"Net-(J2-SWCLK-Pad4)", "to":"/TEST_ALT"});
        assert_eq!(preview["rename"], ack);
        let (result, refusal) = sheet
            .readonly(json!({
                "edit": edit, "revision": preview["revision"],
                "acknowledge_rename": {"from":"Net-(J2-SWCLK)", "to":"/TEST_ALT"}
            }))
            .await;
        assert_eq!(result["isError"], true, "{refusal:#}");
        assert_eq!(refusal["error"]["field"], "acknowledge_rename");
        let (result, applied) = sheet
            .call(json!({
                "edit": edit, "revision": preview["revision"], "acknowledge_rename": ack
            }))
            .await;
        assert_ne!(result["isError"], true, "{applied:#}");
    }

    #[tokio::test]
    async fn the_reverse_keeps_the_first_label_and_its_net_name() {
        let sheet = Sheet::named("adjacent.kicad_sch", ADJACENT).await;
        let (_, before) = read_schematic(&sheet.path).unwrap();
        let edit = reverse_edit();
        let (result, preview) = sheet.readonly(json!({"edit":edit})).await;
        assert_ne!(result["isError"], true, "{preview:#}");
        assert!(preview["rename"].is_null());
        let (result, applied) = sheet
            .call(json!({"edit":edit, "revision":preview["revision"]}))
            .await;
        assert_ne!(result["isError"], true, "{applied:#}");
        let (_, tree) = read_schematic(&sheet.path).unwrap();
        assert_new_wire_stroke(&before, &tree, &applied);
        let labels = copperpour_sexp::schematic::extract_labels(&tree);
        assert_eq!(labels.len(), 1);
        assert_eq!(labels[0].net, "SIG_C");
        assert_eq!(labels[0].uuid.as_deref(), edit["label_uuids"][0].as_str());
        assert!((wire_length(&before) - 22.86).abs() < 1e-6);
        assert!((wire_length(&tree) - 48.26).abs() < 1e-6);
    }

    async fn clearance_sheet() -> Sheet {
        // Add a drawn body caption without changing the pin definition.
        let marker = "\t\t\t(symbol \"R_0_1\"\n";
        let source = std::str::from_utf8(ADJACENT).unwrap();
        assert_eq!(source.matches(marker).count(), 1);
        let with_text = source.replace(marker, &format!(
            "{marker}\t\t\t\t(text \"BODY\"\n\t\t\t\t\t(at 0 0 0)\n\t\t\t\t\t(effects\n\t\t\t\t\t\t(font\n\t\t\t\t\t\t\t(size 0.5 0.5)\n\t\t\t\t\t\t)\n\t\t\t\t\t)\n\t\t\t\t)\n"
        ));
        Sheet::named("adjacent.kicad_sch", with_text.as_bytes()).await
    }

    async fn assert_unresolved_clearance_refusal(sheet: &Sheet) {
        let (result, body) = sheet.readonly(json!({"edit":reverse_edit()})).await;
        assert_eq!(result["isError"], true, "{body:#}");
        assert_eq!(body["error"]["kind"], "plan_blocked", "{body:#}");
        assert_eq!(
            body["error"]["reasons"],
            json!(["cannot establish clearance: unresolved bounds for R1, R2"]),
            "{body:#}"
        );
    }

    #[tokio::test]
    async fn a_bridge_refuses_unmeasured_library_text_with_known_pins() {
        assert_unresolved_clearance_refusal(&clearance_sheet().await).await;
    }

    #[tokio::test]
    async fn a_reverse_bridge_refuses_diagonal_anchors_on_the_same_net() {
        let sheet = Sheet::new(ADJACENT).await;
        let (result, body) = sheet
            .tool(
                "batch_add_wire",
                &mut json!({"wires":[
                    {"x1":107.95,"y1":88.9,"x2":107.95,"y2":86.36}
                ]}),
            )
            .await;
        assert_ne!(result["isError"], true, "{body:#}");
        let source = fs::read_to_string(&sheet.path).unwrap();
        assert_eq!(source.matches("(at 107.95 88.9 0)").count(), 1);
        fs::write(
            &sheet.path,
            source.replace("(at 107.95 88.9 0)", "(at 107.95 86.36 0)"),
        )
        .unwrap();
        let (result, body) = sheet.readonly(json!({"edit":reverse_edit()})).await;
        assert_eq!(result["isError"], true, "{body:#}");
        assert_eq!(
            body["error"]["reason"],
            "the selected anchors do not make one direct orthogonal wire"
        );
    }

    #[tokio::test]
    async fn a_fresh_name_cannot_be_bus_syntax_or_a_generated_net_name() {
        for name in ["D[0..7]", "Net-(R9-Pad1)", "A/B", " ", "A{slash}B"] {
            let sheet = Sheet::new(NAMES).await;
            let (result, body) = sheet
                .readonly(json!({"edit":{
                    "direction":"wire_to_labels",
                    "wire_uuids":["566e47a9-13f9-4bfe-8770-9f907db66b32"],
                    "local_name":name
                }}))
                .await;
            assert_eq!(result["isError"], true, "{name:?}: {body:#}");
            assert_eq!(body["error"]["kind"], "invalid_argument", "{body:#}");
            assert!(
                body["error"]["reason"]
                    .as_str()
                    .unwrap()
                    .contains("ordinary local name"),
                "{body:#}"
            );
        }
    }

    #[tokio::test]
    async fn the_reverse_removes_both_labels_only_if_a_third_keeps_the_net_named() {
        let sheet = Sheet::new(ADJACENT).await;
        sheet.add_label("SIG_C", 105.41, 88.9).await;
        let edit = reverse_edit();
        let (result, body) = sheet.readonly(json!({"edit":edit})).await;
        assert_ne!(result["isError"], true, "{body:#}");
        assert_eq!(body["removed_uuids"], edit["label_uuids"]);
        assert!(body["rename"].is_null());
    }

    #[tokio::test]
    async fn the_reverse_keeps_a_label_if_removing_both_would_split_a_pin_island() {
        let sheet = Sheet::new(ADJACENT).await;
        let (result, body) = sheet
            .tool(
                "batch_delete_schematic_wire",
                &mut json!({
                    "uuids":["88c14950-821b-4eb3-987c-8371b93ad899"]
                }),
            )
            .await;
        assert_ne!(result["isError"], true, "{body:#}");
        sheet.add_label("SIG_C", 97.79, 88.9).await;
        let edit = reverse_edit();
        let (result, body) = sheet.readonly(json!({"edit":edit})).await;
        assert_ne!(result["isError"], true, "{body:#}");
        assert_eq!(body["removed_uuids"], json!([edit["label_uuids"][1]]));
        assert_eq!(body["kept_label_uuids"], json!([edit["label_uuids"][0]]));
    }

    #[tokio::test]
    async fn reverse_labels_need_one_net_and_one_name() {
        for ((x, y), reason) in [
            (
                (105.41, 88.9),
                "the selected net does not have one ordinary local name",
            ),
            ((50.8, 50.8), "the selected labels are not on the same net"),
        ] {
            let sheet = Sheet::new(ADJACENT).await;
            sheet.add_label("SIG_OTHER", x, y).await;
            let (_, tree) = read_schematic(&sheet.path).unwrap();
            let other = copperpour_sexp::schematic::extract_labels(&tree)
                .into_iter()
                .find(|label| label.net == "SIG_OTHER")
                .unwrap();
            let (result, body) = sheet
                .readonly(json!({"edit":{
                    "direction":"labels_to_wire",
                    "label_uuids":[reverse_edit()["label_uuids"][0],other.uuid]
                }}))
                .await;
            assert_eq!(result["isError"], true, "{body:#}");
            assert_eq!(body["error"]["kind"], "invalid_argument", "{body:#}");
            assert_eq!(body["error"]["reason"], reason);
        }
    }

    async fn reverse_at_pin(sheet: &Sheet) -> Value {
        let (result, body) = sheet
            .tool(
                "batch_delete_schematic_wire",
                &mut json!({
                    "uuids":["46201b30-cf0e-445b-ac48-379253f92064"]
                }),
            )
            .await;
        assert_ne!(result["isError"], true, "{body:#}");
        let (result, body) = sheet
            .tool(
                "delete_schematic_net_label",
                &mut json!({
                    "net":"SIG_C","x":107.95,"y":88.9
                }),
            )
            .await;
        assert_ne!(result["isError"], true, "{body:#}");
        sheet.add_label("SIG_C", 105.41, 88.9).await;
        let (_, tree) = read_schematic(&sheet.path).unwrap();
        let bare = copperpour_sexp::schematic::extract_labels(&tree)
            .into_iter()
            .find(|label| at(label) == (105.41, 88.9))
            .unwrap();
        json!({
            "direction":"labels_to_wire",
            "label_uuids":[reverse_edit()["label_uuids"][1],bare.uuid]
        })
    }

    #[tokio::test]
    async fn the_reverse_can_start_at_a_label_on_a_bare_pin() {
        let sheet = Sheet::new(ADJACENT).await;
        let edit = reverse_at_pin(&sheet).await;
        let (result, body) = sheet.readonly(json!({"edit":edit})).await;
        assert_ne!(result["isError"], true, "{body:#}");
        assert_eq!(body["anchors"][1], json!([105.41, 88.9]));
    }

    #[tokio::test]
    async fn the_reverse_cannot_add_a_branch_at_a_pin_with_a_wire_exit() {
        let sheet = Sheet::new(ADJACENT).await;
        let edit = reverse_at_pin(&sheet).await;
        let (result, body) = sheet
            .tool(
                "batch_add_wire",
                &mut json!({"wires":[
                    {"x1":105.41,"y1":88.9,"x2":105.41,"y2":85.09}
                ]}),
            )
            .await;
        assert_ne!(result["isError"], true, "{body:#}");
        let (result, body) = sheet.readonly(json!({"edit":edit})).await;
        assert_eq!(result["isError"], true, "{body:#}");
        assert_eq!(
            body["error"]["reason"],
            "each selected label must sit on a pin or wire end, not a branch or through-run"
        );
    }

    #[tokio::test]
    async fn the_cut_cannot_strand_an_electrically_attached_label() {
        for (y, kind, reason) in [
            (
                96.5201,
                "plan_blocked",
                json!(["the cut would remove a wire with a label tap"]),
            ),
            (
                96.52,
                "invalid_argument",
                json!("the selected UUIDs match multiple runs"),
            ),
        ] {
            let sheet = Sheet::new(CROSSING).await;
            sheet.add_label("SIG_A", 100.0, y).await;
            let (result, body) = sheet.readonly(json!({"edit":crossing_edit()})).await;
            assert_eq!(result["isError"], true, "{body:#}");
            assert_eq!(body["error"]["kind"], kind, "{body:#}");
            let field = if kind == "plan_blocked" {
                "reasons"
            } else {
                "reason"
            };
            assert_eq!(body["error"][field], reason, "{body:#}");
        }
    }

    #[tokio::test]
    async fn a_power_pin_under_the_cut_is_not_an_electrical_tap() {
        let sheet = Sheet::named("crossing.kicad_sch", CROSSING).await;
        let (result, body) = sheet
            .tool(
                "add_power_symbol",
                &mut json!({"power_net":"GND","x":99.06,"y":96.52}),
            )
            .await;
        assert_ne!(result["isError"], true, "{body:#}");
        let (source, tree) = read_schematic(&sheet.path).unwrap();
        let dot = tree
            .find_all("junction")
            .into_iter()
            .find(|node| parse_at(node) == Some((99.06, 96.52, 0.0)))
            .expect("placement added a junction");
        let command = SchematicCommand::delete_items(
            &source,
            [ItemId::new(dot.find_str("uuid").unwrap()).unwrap()],
            "remove fixture junction",
        )
        .unwrap();
        let (without_dot, _) = prepare_command(&sheet.path, &source, &command).unwrap();
        write_atomic_if_unchanged(&sheet.path, &source, &without_dot).unwrap();
        let (result, body) = sheet.readonly(json!({"edit":crossing_edit()})).await;
        assert_ne!(result["isError"], true, "{body:#}");
        assert_eq!(body["status"], "preview");
    }

    #[tokio::test]
    async fn exchange_refuses_coordinates_that_round_on_load() {
        for (x, y) in [(100.00004, 96.5201), (100.0, 96.52004)] {
            let sheet = Sheet::new(CROSSING).await;
            sheet.add_label("SIG_A", x, y).await;
            let (result, body) = sheet.readonly(json!({"edit":crossing_edit()})).await;
            assert_eq!(result["isError"], true, "{body:#}");
            assert_eq!(body["error"]["kind"], "plan_blocked", "{body:#}");
            assert_eq!(
                body["error"]["reasons"],
                json!([
                    "cannot verify pin connectivity after KiCad rounds saved schematic coordinates"
                ])
            );
        }
    }

    #[tokio::test]
    async fn a_same_net_label_tap_on_the_reverse_bridge_is_refused() {
        for y in [88.9, 88.9001] {
            let sheet = Sheet::new(ADJACENT).await;
            sheet.add_label("SIG_C", 120.65, y).await;
            let (result, body) = sheet.readonly(json!({"edit":reverse_edit()})).await;
            assert_eq!(result["isError"], true, "{y}: {body:#}");
            assert!(body["message"].as_str().unwrap().contains("interior tap"));
        }
    }

    #[tokio::test]
    async fn a_reverse_bridge_cannot_enter_an_unrelated_bus() {
        let sheet = Sheet::new(ADJACENT).await;
        let mut bus = json!({"x1":120.65,"y1":85.09,"x2":120.65,"y2":92.71});
        let (result, body) = sheet.tool("add_bus", &mut bus).await;
        assert_ne!(result["isError"], true, "{body:#}");
        let (result, body) = sheet.readonly(json!({"edit":reverse_edit()})).await;
        assert_eq!(result["isError"], true, "{body:#}");
        assert_eq!(body["error"]["kind"], "plan_blocked", "{body:#}");
        assert_eq!(
            body["error"]["reasons"],
            json!(["the new wire would cross or touch a bus"])
        );
    }

    #[tokio::test]
    async fn a_reverse_bridge_cannot_touch_a_bus_entry_after_kicad_rounds_it() {
        let sheet = Sheet::new(ADJACENT).await;
        let (result, body) = sheet
            .tool(
                "add_bus",
                &mut json!({
                    "x1":123.19,"y1":90.17,"x2":123.19,"y2":95.25
                }),
            )
            .await;
        assert_ne!(result["isError"], true, "{body:#}");
        let (result, body) = sheet
            .tool(
                "add_bus_entry",
                &mut json!({
                    "x":120.65004,"y":88.90004,"direction":"down_right"
                }),
            )
            .await;
        assert_ne!(result["isError"], true, "{body:#}");
        let (_, tree) = read_schematic(&sheet.path).unwrap();
        let buses = extract_buses(&tree);
        assert!(crate::tools::sch_connectivity::bus_entry_wire_ends(
            &tree,
            &buses,
            0.5 / super::super::NM_PER_MM,
        )
        .is_empty());
        let (result, body) = sheet.readonly(json!({"edit":reverse_edit()})).await;
        assert_eq!(result["isError"], true, "{body:#}");
        assert_eq!(body["error"]["kind"], "plan_blocked", "{body:#}");
        assert_eq!(
            body["error"]["reasons"],
            json!(["the new wire would touch a bus entry"])
        );
    }

    #[tokio::test]
    async fn a_reverse_bridge_cannot_cross_the_middle_of_a_bus_entry() {
        let sheet = Sheet::new(ADJACENT).await;
        let (result, body) = sheet
            .tool(
                "add_bus",
                &mut json!({"x1":123.19,"y1":90.17,"x2":123.19,"y2":95.25}),
            )
            .await;
        assert_ne!(result["isError"], true, "{body:#}");
        let (result, body) = sheet
            .tool(
                "add_bus_entry",
                &mut json!({"x":120.65004,"y":87.63,"direction":"down_right"}),
            )
            .await;
        assert_ne!(result["isError"], true, "{body:#}");
        let (result, body) = sheet.readonly(json!({"edit":reverse_edit()})).await;
        assert_eq!(result["isError"], true, "{body:#}");
        assert_eq!(body["error"]["kind"], "plan_blocked", "{body:#}");
        assert_eq!(
            body["error"]["reasons"],
            json!(["the new wire would touch a bus entry"])
        );
    }

    fn explicit_crossing_edit() -> Value {
        let mut edit = crossing_edit();
        edit["anchors"] = json!([[55.88, 96.52], [199.39, 93.98]]);
        edit
    }

    #[tokio::test]
    async fn a_stub_cannot_touch_an_unrelated_wire_ending_at_its_anchor() {
        let sheet = Sheet::new(CROSSING).await;
        let (result, body) = sheet
            .tool(
                "batch_add_wire",
                &mut json!({"wires":[
                    {"x1":196.85,"y1":93.98,"x2":199.39,"y2":93.98}
                ]}),
            )
            .await;
        assert_ne!(result["isError"], true, "{body:#}");
        let (result, body) = sheet
            .readonly(json!({"edit":explicit_crossing_edit()}))
            .await;
        assert_eq!(result["isError"], true, "{body:#}");
        assert_eq!(
            body["error"]["reasons"],
            json!(["the new wire would touch a foreign net"])
        );
    }

    #[tokio::test]
    async fn a_stub_cannot_attach_a_foreign_label_at_its_anchor() {
        let sheet = Sheet::new(CROSSING).await;
        sheet.add_label("ZZ_OTHER", 199.39, 93.98).await;
        let (result, body) = sheet
            .readonly(json!({"edit":explicit_crossing_edit()}))
            .await;
        assert_eq!(result["isError"], true, "{body:#}");
        assert_eq!(
            body["error"]["reasons"],
            json!(["the new wire would touch a foreign label"])
        );
    }

    #[tokio::test]
    async fn an_exact_anchor_cannot_merge_two_pin_groups() {
        let sheet = Sheet::new(CROSSING).await;
        let (result, body) = sheet
            .tool(
                "batch_add_wire",
                &mut json!({"wires":[
                    {"x1":199.39,"y1":93.98,"x2":207.01,"y2":93.98},
                    {"x1":207.01,"y1":93.98,"x2":207.01,"y2":96.52}
                ]}),
            )
            .await;
        assert_ne!(result["isError"], true, "{body:#}");
        let (result, body) = sheet
            .readonly(json!({"edit":explicit_crossing_edit()}))
            .await;
        assert_eq!(result["isError"], true, "{body:#}");
        assert_eq!(
            body["error"]["reasons"],
            json!(["the change would split or merge a pin net"])
        );
    }

    #[tokio::test]
    async fn the_reverse_cannot_add_a_leg_to_a_through_run() {
        let sheet = Sheet::new(ADJACENT).await;
        let (result, body) = sheet
            .tool(
                "batch_add_wire",
                &mut json!({"wires":[
                    {"x1":107.95,"y1":88.9,"x2":107.95,"y2":83.82}
                ]}),
            )
            .await;
        assert_ne!(result["isError"], true, "{body:#}");
        let (result, body) = sheet.readonly(json!({"edit":{
            "direction":"labels_to_wire",
            "label_uuids":["30544d14-6e0a-47a2-aa69-bec165fbf2af", "1c5fd64f-c1d6-4285-bb22-fd1d9ed8de01"]
        }})).await;
        assert_eq!(result["isError"], true, "{body:#}");
        assert!(body["message"]
            .as_str()
            .unwrap()
            .contains("branch or through-run"));
    }

    #[tokio::test]
    async fn two_explicit_stubs_cannot_cross_each_other() {
        let sheet = Sheet::new(ADJACENT).await;
        let (result, body) = sheet
            .tool(
                "batch_add_wire",
                &mut json!({"wires":[
                    {"x1":152.4,"y1":30.48,"x2":153.67,"y2":30.48},
                    {"x1":153.67,"y1":30.48,"x2":153.67,"y2":31.75},
                    {"x1":149.86,"y1":30.48,"x2":152.4,"y2":30.48},
                    {"x1":153.67,"y1":31.75,"x2":156.21,"y2":31.75}
                ]}),
            )
            .await;
        assert_ne!(result["isError"], true, "{body:#}");
        sheet.add_label("TEST", 152.4, 30.48).await;
        sheet.add_label("TEST", 153.67, 31.75).await;
        let (_, tree) = read_schematic(&sheet.path).unwrap();
        let ids: Vec<_> = extract_wires(&tree)
            .into_iter()
            .filter(|wire| {
                point(wire.x2, wire.y2) == point(153.67, 30.48)
                    || point(wire.x1, wire.y1) == point(153.67, 30.48)
            })
            .map(|wire| wire.uuid.unwrap())
            .collect();
        assert_eq!(ids.len(), 2);
        let (result, body) = sheet
            .readonly(json!({"edit":{
                "direction":"wire_to_labels",
                "wire_uuids":ids,
                "anchors":[[152.4,33.02],[151.13,31.75]]
            }}))
            .await;
        assert_eq!(result["isError"], true, "{body:#}");
        assert!(body["message"].as_str().unwrap().contains("two new stubs"));
    }

    #[tokio::test]
    async fn an_undotted_crossing_on_the_reverse_bridge_is_refused() {
        let sheet = Sheet::new(ADJACENT).await;
        let (result, body) = sheet
            .tool(
                "batch_add_wire",
                &mut json!({"wires":[
                    {"x1":120.65,"y1":80.0,"x2":120.65,"y2":100.0}
                ]}),
            )
            .await;
        assert_ne!(result["isError"], true, "{body:#}");
        let edit = json!({
            "direction":"labels_to_wire",
            "label_uuids":["30544d14-6e0a-47a2-aa69-bec165fbf2af", "1c5fd64f-c1d6-4285-bb22-fd1d9ed8de01"]
        });
        let (result, body) = sheet.readonly(json!({"edit":edit})).await;
        assert_eq!(result["isError"], true, "{body:#}");
        assert!(
            body["message"]
                .as_str()
                .unwrap()
                .contains("cross another wire"),
            "{body:#}"
        );
    }

    #[tokio::test]
    async fn an_implicit_power_connection_is_not_an_unnamed_signal() {
        let sheet = Sheet::new(include_bytes!(
            "../../../tests/fixtures/hidden_power_pin/single_pin_nets.kicad_sch"
        ))
        .await;
        fs::write(
            sheet._dir.path().join("single_pin_nets_child.kicad_sch"),
            include_bytes!("../../../tests/fixtures/single_pin_nets_child.kicad_sch"),
        )
        .unwrap();
        let (result, body) = sheet
            .tool("delete_sheet", &mut json!({"sheet_name":"Child"}))
            .await;
        assert_ne!(result["isError"], true, "{body:#}");
        let (_, tree) = read_schematic(&sheet.path).unwrap();
        let (_, pins) = super::super::drawn_symbol_pin_inventory(&tree)
            .into_iter()
            .flatten()
            .find(|(symbol, _)| symbol.reference == "U99")
            .unwrap();
        let [a, b]: [Point; 2] = pins
            .into_iter()
            .map(|(pin, t)| pin_endpoint(&pin, t))
            .collect::<Vec<_>>()
            .try_into()
            .unwrap();
        let (result, body) = sheet
            .tool(
                "batch_add_wire",
                &mut json!({"wires":[
                    {"x1":a.0,"y1":a.1,"x2":b.0,"y2":b.1}
                ]}),
            )
            .await;
        assert_ne!(result["isError"], true, "{body:#}");
        let (_, tree) = read_schematic(&sheet.path).unwrap();
        let id = extract_wires(&tree)
            .into_iter()
            .find(|wire| {
                point(wire.x1, wire.y1) == point(a.0, a.1)
                    && point(wire.x2, wire.y2) == point(b.0, b.1)
            })
            .unwrap()
            .uuid
            .unwrap();
        let edit = json!({"direction":"wire_to_labels","wire_uuids":[id],"local_name":"SIG_NEW"});
        let (result, body) = sheet.readonly(json!({"edit":edit})).await;
        assert_eq!(result["isError"], true, "{body:#}");
        assert!(
            body["message"]
                .as_str()
                .unwrap()
                .contains("implicit power nets"),
            "{body:#}"
        );
        let (result, advice) = sheet
            .tool(
                "check_schematic_wire_runs",
                &mut json!({
                    "label_advice":{"wire_uuids":[id],"long_run_mm":1.0}
                }),
            )
            .await;
        assert_ne!(result["isError"], true, "{advice:#}");
        assert_eq!(
            advice["label_advice"][0]["label_choice"]["action"],
            "unsupported_scope_or_aliases"
        );
    }

    #[tokio::test]
    async fn a_name_already_used_elsewhere_cannot_be_acknowledged_as_a_rename() {
        let sheet = Sheet::new(NAMES).await;
        let edit = json!({
            "direction":"wire_to_labels",
            "wire_uuids":["566e47a9-13f9-4bfe-8770-9f907db66b32"],
            "local_name":"SIG_X"
        });
        let (result, body) = sheet.readonly(json!({"edit":edit})).await;
        assert_eq!(result["isError"], true, "{body:#}");
        assert!(body["message"]
            .as_str()
            .unwrap()
            .contains("already names another net"));
    }

    #[tokio::test]
    async fn a_power_rail_name_cannot_be_reused_for_a_fresh_local_label() {
        let sheet = Sheet::new(NAMES).await;
        let edit = json!({
            "direction":"wire_to_labels",
            "wire_uuids":["566e47a9-13f9-4bfe-8770-9f907db66b32"],
            "local_name":"+3V3"
        });
        let (result, body) = sheet.readonly(json!({"edit":edit})).await;
        assert_eq!(result["isError"], true, "{body:#}");
        assert_eq!(
            body["error"]["reason"],
            "the local name already names another net"
        );
    }

    async fn crossing_refuses_label_variant(label_type: &str, net: &str) {
        let sheet = Sheet::new(CROSSING).await;
        let (result, body) = sheet
            .tool(
                "add_schematic_net_label",
                &mut json!({"net":net,"x":199.39,"y":96.52,"label_type":label_type}),
            )
            .await;
        assert_ne!(result["isError"], true, "{body:#}");
        let (result, body) = sheet.readonly(json!({"edit":crossing_edit()})).await;
        assert_eq!(result["isError"], true, "{body:#}");
        assert_eq!(
            body["error"]["reason"],
            "the run does not have one ordinary local name"
        );
    }

    #[tokio::test]
    async fn the_run_must_have_one_ordinary_local_name() {
        for (kind, name) in [
            ("global_label", "SIG_A"),
            ("hierarchical_label", "SIG_A"),
            ("net_label", "SIG_OTHER"),
        ] {
            crossing_refuses_label_variant(kind, name).await;
        }
    }

    #[tokio::test]
    async fn two_existing_pin_labels_need_no_new_items() {
        let sheet = Sheet::new(CROSSING).await;
        sheet.add_label("SIG_A", 199.39, 96.52).await;
        let edit = crossing_edit();
        let (result, plan) = sheet.readonly(json!({"edit":edit})).await;
        assert_ne!(result["isError"], true, "{plan:#}");
        assert_eq!(plan["anchors"], json!([[55.88, 96.52], [199.39, 96.52]]));
        assert_eq!(plan["removed_uuids"], edit["wire_uuids"]);
        assert_eq!(plan["added_uuids"], json!([]));
        assert_eq!(plan["kept_label_uuids"].as_array().unwrap().len(), 2);
        assert!(plan["rename"].is_null());

        let (result, applied) = sheet
            .call(json!({"edit":edit,"revision":plan["revision"]}))
            .await;
        assert_ne!(result["isError"], true, "{applied:#}");
        assert_eq!(applied["status"], "applied");
        let (_, tree) = read_schematic(&sheet.path).unwrap();
        assert!(extract_wires(&tree)
            .iter()
            .all(|wire| { wire.uuid.as_deref() != edit["wire_uuids"][0].as_str() }));
        assert_eq!(
            copperpour_sexp::schematic::extract_labels(&tree)
                .iter()
                .filter(|label| label.net == "SIG_A")
                .count(),
            2
        );
    }

    #[tokio::test]
    async fn the_exchange_refuses_the_analog_feedback_branch() {
        let sheet = Sheet::new(include_bytes!(
            "../../../tests/fixtures/labels_vs_wires_kicad10/analog/analog.kicad_sch"
        ))
        .await;
        let (result, body) = sheet.readonly(json!({"edit":{
            "direction":"wire_to_labels",
            "wire_uuids":["2c916bc8-ab76-48b2-8781-3529b4c4a9e0","893a179b-551c-4296-93d4-3e0cc6c4ab00"]
        }})).await;
        assert_eq!(result["isError"], true, "{body:#}");
        assert!(body["message"]
            .as_str()
            .unwrap()
            .contains("neither may be a branch"));
    }

    #[tokio::test]
    async fn the_advisor_keeps_a_three_pin_net_wired_without_a_branch_at_the_run_end() {
        let sheet = Sheet::new(NAMES).await;
        let (result, body) = sheet
            .tool(
                "delete_schematic_net_label",
                &mut json!({
                    "net":"SIG_X","x":80.01,"y":132.08
                }),
            )
            .await;
        assert_ne!(result["isError"], true, "{body:#}");
        sheet.add_label("SIG_B", 80.01, 132.08).await;
        let (_, tree) = read_schematic(&sheet.path).unwrap();
        let run = extract_wires(&tree)
            .into_iter()
            .find(|wire| {
                point(wire.x1, wire.y1) == point(80.01, 71.12)
                    && point(wire.x2, wire.y2) == point(110.49, 71.12)
            })
            .unwrap();
        let (result, body) = sheet.tool("check_schematic_wire_runs", &mut json!({
            "label_advice":{
                "long_run_mm":20.0,
                "wire_uuids":[run.uuid],
                "blocks":[{"name":"unrelated","bounds":{"min":[90.0,70.0],"max":[100.0,73.0]}}]
            }
        })).await;
        assert_ne!(result["isError"], true, "{body:#}");
        let report = &body["label_advice"][0];
        assert_eq!(report["start"]["status"], "attached");
        assert_eq!(report["end"]["status"], "attached");
        assert_eq!(report["other_pins"][0]["reference"], "R9");
        assert_eq!(report["reason"], "keep_visible_trunk");
    }

    #[tokio::test]
    async fn an_explicit_stub_that_does_not_clear_is_not_moved() {
        let sheet = Sheet::new(CROSSING).await;
        let mut edit = crossing_edit();
        edit["anchors"] = json!([[55.88, 96.52], [200.66, 96.52]]);
        let (result, body) = sheet.readonly(json!({"edit":edit})).await;
        assert_eq!(result["isError"], true, "{body:#}");
        assert!(
            body["message"].as_str().unwrap().contains("do not clear"),
            "{body:#}"
        );
    }

    #[tokio::test]
    async fn automatic_anchors_try_another_short_stub_when_the_default_is_blocked() {
        let sheet = Sheet::new(CROSSING).await;
        sheet.add_label("BLOCK", 192.0, 96.0).await;
        let mut explicit = crossing_edit();
        explicit["anchors"] = json!([[55.88, 96.52], [196.85, 96.52]]);
        let (result, body) = sheet.readonly(json!({"edit":explicit})).await;
        assert_eq!(result["isError"], true, "{body:#}");
        let (result, preview) = sheet.readonly(json!({"edit":crossing_edit()})).await;
        assert_ne!(result["isError"], true, "{preview:#}");
        assert_ne!(preview["anchors"][1], explicit["anchors"][1]);
    }

    #[tokio::test]
    async fn a_cut_that_strands_a_junction_is_not_offered() {
        let sheet = Sheet::new(CROSSING).await;
        let (result, body) = sheet
            .tool(
                "batch_delete_schematic_wire",
                &mut json!({
                    "uuids":crossing_edit()["wire_uuids"]
                }),
            )
            .await;
        assert_ne!(result["isError"], true, "{body:#}");
        let (result, body) = sheet
            .tool(
                "batch_add_wire",
                &mut json!({"wires":[
                    {"x1":55.88,"y1":96.52,"x2":60.96,"y2":96.52},
                    {"x1":60.96,"y1":96.52,"x2":199.39,"y2":96.52}
                ]}),
            )
            .await;
        assert_ne!(result["isError"], true, "{body:#}");
        let (result, body) = sheet
            .tool("add_junction", &mut json!({"x":60.96,"y":96.52}))
            .await;
        assert_ne!(result["isError"], true, "{body:#}");
        sheet.add_label("SIG_A", 60.96, 96.52).await;
        let (_, tree) = read_schematic(&sheet.path).unwrap();
        let wire = extract_wires(&tree)
            .into_iter()
            .find(|w| {
                point(w.x1, w.y1) == point(60.96, 96.52)
                    && point(w.x2, w.y2) == point(199.39, 96.52)
            })
            .unwrap();
        let (result, advice) = sheet
            .tool(
                "check_schematic_wire_runs",
                &mut json!({
                    "label_advice":{"long_run_mm":100.0,"wire_uuids":[wire.uuid]}
                }),
            )
            .await;
        assert_ne!(result["isError"], true, "{advice:#}");
        let advice = &advice["label_advice"][0];
        let (result, preview) = sheet
            .readonly(json!({"edit":{
                "direction":"wire_to_labels", "wire_uuids":[wire.uuid]
            }}))
            .await;
        assert_eq!(result["isError"], true, "{preview:#}");
        assert!(
            preview["message"]
                .as_str()
                .unwrap()
                .contains("leave a junction"),
            "{preview:#}"
        );
        assert_eq!(advice["reason"], "dangling_cut_end");
        assert!(advice["label_anchor_estimate"].is_null());
    }

    #[tokio::test]
    async fn a_cut_cannot_leave_its_end_label_floating() {
        let sheet = Sheet::new(CROSSING).await;
        let (result, body) = sheet
            .tool(
                "batch_delete_schematic_wire",
                &mut json!({
                    "uuids":crossing_edit()["wire_uuids"]
                }),
            )
            .await;
        assert_ne!(result["isError"], true, "{body:#}");
        let (result, body) = sheet
            .tool(
                "batch_add_wire",
                &mut json!({"wires":[
                    {"x1":55.88,"y1":96.52,"x2":180.34,"y2":96.52}
                ]}),
            )
            .await;
        assert_ne!(result["isError"], true, "{body:#}");
        sheet.add_label("SIG_A", 180.34, 96.52).await;
        sheet.add_label("SIG_A", 199.39, 96.52).await;
        let (_, tree) = read_schematic(&sheet.path).unwrap();
        let uuid = extract_wires(&tree)
            .into_iter()
            .find(|wire| point(wire.x2, wire.y2) == point(180.34, 96.52))
            .unwrap()
            .uuid
            .unwrap();
        let (result, body) = sheet
            .readonly(json!({"edit":{
                "direction":"wire_to_labels","wire_uuids":[uuid]
            }}))
            .await;
        assert_eq!(result["isError"], true, "{body:#}");
        assert!(
            body["message"].as_str().unwrap().contains("dangling"),
            "{body:#}"
        );
        let (result, body) = sheet
            .tool(
                "check_schematic_wire_runs",
                &mut json!({
                    "label_advice":{"long_run_mm":100.0,"wire_uuids":[uuid]}
                }),
            )
            .await;
        assert_ne!(result["isError"], true, "{body:#}");
        assert_eq!(body["label_advice"][0]["reason"], "dangling_cut_end");
    }

    #[tokio::test]
    async fn advice_and_preview_reuse_a_nearby_label_on_a_retained_pin_stub() {
        let sheet = Sheet::new(CROSSING).await;
        let (result, body) = sheet
            .tool(
                "delete_schematic_net_label",
                &mut json!({
                    "net":"SIG_A","x":55.88,"y":96.52
                }),
            )
            .await;
        assert_ne!(result["isError"], true, "{body:#}");
        let (result, body) = sheet
            .tool(
                "batch_add_wire",
                &mut json!({"wires":[
                    {"x1":55.88,"y1":96.52,"x2":55.88,"y2":93.98}
                ]}),
            )
            .await;
        assert_ne!(result["isError"], true, "{body:#}");
        sheet.add_label("SIG_A", 55.88, 93.98).await;
        let edit = crossing_edit();
        let (result, advice) = sheet
            .tool(
                "check_schematic_wire_runs",
                &mut json!({
                    "label_advice":{"long_run_mm":100.0,"wire_uuids":edit["wire_uuids"]}
                }),
            )
            .await;
        assert_ne!(result["isError"], true, "{advice:#}");
        let (result, preview) = sheet.readonly(json!({"edit":edit})).await;
        assert_ne!(result["isError"], true, "{preview:#}");
        assert_eq!(preview["anchors"][0], json!([55.88, 93.98]));
        let report = &advice["label_advice"][0];
        assert_eq!(report["label_choice"]["labels_to_find_added"], 1);
        assert_eq!(
            report["label_anchor_estimate"]["anchors"],
            preview["anchors"]
        );
        assert_eq!(preview["removed_uuids"].as_array().unwrap().len(), 2);
        let (result, applied) = sheet
            .call(json!({"edit":edit,"revision":preview["revision"]}))
            .await;
        assert_ne!(result["isError"], true, "{applied:#}");
        let (_, tree) = read_schematic(&sheet.path).unwrap();
        assert!(!extract_junctions(&tree).contains(&(55.88, 96.52)));
    }

    #[tokio::test]
    async fn an_interior_tap_cannot_be_removed_with_its_whole_wire_uuid() {
        let sheet = Sheet::new(CROSSING).await;
        let mut remove = json!({"uuids":crossing_edit()["wire_uuids"]});
        let (result, body) = sheet.tool("batch_delete_schematic_wire", &mut remove).await;
        assert_ne!(result["isError"], true, "{body:#}");
        let mut added = json!({"wires":[
            {"x1":55.88,"y1":96.52,"x2":107.95,"y2":96.52},
            {"x1":107.95,"y1":96.52,"x2":199.39,"y2":96.52}
        ]});
        let (result, body) = sheet.tool("batch_add_wire", &mut added).await;
        assert_ne!(result["isError"], true, "{body:#}");
        sheet.add_label("SIG_A", 100.0, 96.52).await;
        let (_, tree) = read_schematic(&sheet.path).unwrap();
        let uuids: Vec<_> = extract_wires(&tree)
            .into_iter()
            .filter(|wire| {
                point(wire.x1, wire.y1) == point(107.95, 96.52)
                    || point(wire.x2, wire.y2) == point(107.95, 96.52)
            })
            .map(|wire| wire.uuid.unwrap())
            .collect();
        assert_eq!(uuids.len(), 2);
        let (result, body) = sheet
            .readonly(json!({
                "edit":{"direction":"wire_to_labels","wire_uuids":uuids}
            }))
            .await;
        assert_eq!(result["isError"], true, "{body:#}");
        assert!(
            body["message"].as_str().unwrap().contains("interior tap"),
            "{body:#}"
        );
    }
}
