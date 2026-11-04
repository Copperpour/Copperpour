//! The copper check `set_component_placements`, `move_component` and
//! `rotate_component` run on a live board before they move anything (#96).
//!
//! KiCad gives a via the net of the copper it lands on, and a pad dropped on
//! another net's track shorts it; neither shows up as an error on the move,
//! and DRC cannot see the re-netted via at all. So before the commit this asks
//! KiCad for each moved footprint's pad outlines on every copper layer the pad
//! has copper on, carries them to the planned pose with the placement's own
//! transforms ([`placement_xforms`]), and tests them against every via, track
//! and arc of another net on the same layer. Only overlap counts (distance
//! zero or less): it is not a clearance check, and zones and the pads of other
//! footprints, including others in the same batch, are not tested.

use crate::mcp::error::ToolErrorKind;
use crate::mcp::protocol::{CallToolResult, ToolContent};
use anyhow::Context;
use copperpour_ipc::builders::{any_is, nm_to_mm, vec2};
use copperpour_ipc::client::KiCadIpcClient;
use copperpour_ipc::gen::kiapi;
use copperpour_ipc::transform::{placement_xforms, transform_polygon, Xform};
use copperpour_ipc::{PreflightCause, PreflightRefusal};
use kiapi::board::types::BoardLayer;
use kiapi::common::types::{DocumentSpecifier, Vector2};
use prost::Message;
use serde::Serialize;
use serde_json::{json, Value};

/// Arcs are flattened into chords no further than this from the arc, and the
/// test widens by the same distance wherever it flattened one, so a chord
/// never hides an overlap the arc has. KiCad's own pad outlines are
/// approximated to 5 µm by default.
const ARC_CHORD_ERROR_NM: f64 = 100.0;

/// Float rounding allowance, below KiCad's 1 nm grid: a gap of a whole
/// nanometre is never read as touching.
const TOUCH_TOLERANCE_NM: f64 = 0.5;

/// The schema description of the `dry_run` argument the three tools share.
pub(crate) const DRY_RUN_DESCRIPTION: &str =
    "Report the copper check and each footprint's current and target pose without moving \
     anything. Default false.";

/// The schema description of the `allow_copper_conflicts` argument.
pub(crate) const ALLOW_COPPER_CONFLICTS_DESCRIPTION: &str =
    "Apply even when a moved pad would overlap a via, track or arc of another net, for a caller \
     that removes that copper next. The conflicts are still reported. Default false.";

/// Why the closed-file path reports the check as not run.
const NO_LIVE_BOARD_DETAIL: &str =
    "No live KiCad holds this board. The copper check needs KiCad's own pad shapes over IPC, so \
     it did not run on the saved file: a pad may land on another net's via or track unreported.";

/// The two options the check adds to each placement tool.
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct CheckOptions {
    pub(crate) dry_run: bool,
    pub(crate) allow_copper_conflicts: bool,
}

impl CheckOptions {
    /// Both options default to false when omitted or `null`; any other
    /// non-boolean is refused naming the field, never read as false.
    pub(crate) fn from_args(args: &Value) -> Result<Self, CallToolResult> {
        Ok(Self {
            dry_run: optional_bool(args, "dry_run")?,
            allow_copper_conflicts: optional_bool(args, "allow_copper_conflicts")?,
        })
    }
}

fn optional_bool(args: &Value, key: &str) -> Result<bool, CallToolResult> {
    match &args[key] {
        Value::Null => Ok(false),
        Value::Bool(value) => Ok(*value),
        _ => Err(crate::tools::invalid_arg(key, "must be a boolean")),
    }
}

/// A requested pose change, before it is resolved against what KiCad holds.
#[derive(Clone, Debug)]
pub(crate) enum PoseRequest {
    /// `move_component`: a new position, the held rotation kept.
    Move {
        reference: String,
        x_mm: f64,
        y_mm: f64,
    },
    /// `rotate_component`: a new rotation, the held position kept.
    Rotate {
        reference: String,
        rotation_deg: f64,
    },
    /// `set_component_placements`: both.
    Set(copperpour_ipc::types::IpcFootprintPlacement),
}

impl PoseRequest {
    fn reference(&self) -> &str {
        match self {
            Self::Move { reference, .. } | Self::Rotate { reference, .. } => reference,
            Self::Set(placement) => &placement.reference,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct Pose {
    position: Vector2,
    rotation_deg: f64,
}

impl Pose {
    fn held_by(footprint: &kiapi::board::types::FootprintInstance) -> Self {
        Self {
            position: footprint.position.unwrap_or_default(),
            rotation_deg: footprint
                .orientation
                .as_ref()
                .map(|angle| angle.value_degrees)
                .unwrap_or(0.0),
        }
    }

    fn report(&self) -> Value {
        json!({
            "x": nm_to_mm(self.position.x_nm),
            "y": nm_to_mm(self.position.y_nm),
            "rotation": self.rotation_deg,
        })
    }
}

/// One footprint's planned move: the pose KiCad holds it at, the pose the
/// placement would give it, and the transforms between them.
struct PlannedPose {
    reference: String,
    current: Pose,
    target: Pose,
    xforms: Vec<Xform>,
    pads: Vec<PadCopper>,
}

struct PadCopper {
    kiid: String,
    number: String,
    net: String,
}

/// A moved pad that would overlap copper of another net.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub(crate) struct CopperConflict {
    reference: String,
    pad: String,
    pad_net: String,
    layer: String,
    item_kind: &'static str,
    item_uuid: String,
    item_net: String,
}

impl CopperConflict {
    fn describe(&self) -> String {
        format!(
            "{} pad {} ({}) on {} overlaps {} {} ({})",
            self.reference,
            self.pad,
            net_label(&self.pad_net),
            self.layer,
            self.item_kind,
            self.item_uuid,
            net_label(&self.item_net)
        )
    }
}

fn net_label(net: &str) -> &str {
    if net.is_empty() {
        "no net"
    } else {
        net
    }
}

/// What the check found, and what it read to find it.
pub(crate) struct PlacementCheck {
    poses: Vec<PlannedPose>,
    conflicts: Vec<CopperConflict>,
    pad_count: usize,
    layers: Vec<String>,
    via_count: usize,
    track_count: usize,
    arc_count: usize,
}

impl PlacementCheck {
    fn has_conflicts(&self) -> bool {
        !self.conflicts.is_empty()
    }

    /// The `copper_check` object of a response, built from the check alone.
    pub(crate) fn report(&self) -> Value {
        json!({
            "status": if self.has_conflicts() { "conflicts" } else { "clear" },
            "conflict_count": self.conflicts.len(),
            "conflicts": self.conflicts,
            "coverage": {
                "pad_count": self.pad_count,
                "layers": self.layers,
                "via_count": self.via_count,
                "track_count": self.track_count,
                "arc_count": self.arc_count,
            },
        })
    }

    fn placements(&self) -> Vec<Value> {
        self.poses
            .iter()
            .map(|pose| {
                json!({
                    "reference": pose.reference,
                    "current": pose.current.report(),
                    "target": pose.target.report(),
                })
            })
            .collect()
    }

    /// The refusal for conflicts the caller did not allow. Nothing was sent
    /// to KiCad but reads, which is what makes `plan_blocked` honest.
    pub(crate) fn blocked_result(&self, operation: &str) -> CallToolResult {
        let reasons: Vec<String> = self
            .conflicts
            .iter()
            .map(CopperConflict::describe)
            .collect();
        let shown = reasons
            .iter()
            .take(3)
            .cloned()
            .collect::<Vec<_>>()
            .join("; ");
        let more = match reasons.len() {
            0..=3 => String::new(),
            n => format!("; and {} more", n - 3),
        };
        let message = format!(
            "{operation} refused: {count} copper conflict{plural}: {shown}{more}. Nothing was \
             moved. A pad left on another net's copper shorts it, and KiCad silently re-nets a \
             via a pad lands on. Choose another position, or pass allow_copper_conflicts: true to \
             apply anyway and fix that copper next; dry_run: true reports the check without \
             moving anything.",
            count = reasons.len(),
            plural = if reasons.len() == 1 { "" } else { "s" },
        );
        let body = json!({
            "message": message,
            "error": ToolErrorKind::PlanBlocked {
                operation: operation.to_string(),
                reasons,
            },
            "copper_check": self.report(),
        });
        CallToolResult {
            content: vec![ToolContent::Text {
                text: body.to_string(),
            }],
            is_error: true,
        }
    }

    /// A dry run's answer: the check, and each footprint's current and
    /// target pose. Nothing was moved.
    pub(crate) fn dry_run_result(&self) -> CallToolResult {
        let placements = self.placements();
        CallToolResult::json(&json!({
            "dry_run": true,
            "count": placements.len(),
            "placements": placements,
            "source": "ipc",
            "copper_check": self.report(),
        }))
    }

    /// An applied response with the check added, and a warning when the
    /// caller allowed conflicts that are now on the board.
    pub(crate) fn applied_result(&self, mut response: Value) -> CallToolResult {
        response["copper_check"] = self.report();
        if self.has_conflicts() {
            response["warning"] = json!(format!(
                "Applied with {} copper conflict{} because allow_copper_conflicts was set. KiCad \
                 does not report them: a via under another net's pad is re-netted silently and a \
                 pad on a track shorts it. Remove or reroute that copper next.",
                self.conflicts.len(),
                if self.conflicts.len() == 1 { "" } else { "s" }
            ));
        }
        CallToolResult::json(&response)
    }
}

/// The `copper_check` object of the closed-file path, which cannot run it.
/// It must never read as a pass (GOVERNANCE.md, Evidence).
pub(crate) fn not_run_on_closed_board() -> Value {
    json!({
        "status": "not_run",
        "reason": "no_live_board",
        "detail": NO_LIVE_BOARD_DETAIL,
    })
}

/// What a placement tool's IPC closure hands back.
pub(crate) enum Checked<T> {
    /// The check passed, or found conflicts the caller allowed, and the
    /// placement was applied.
    Applied { applied: T, check: PlacementCheck },
    /// Conflicts the caller did not allow: nothing was moved.
    Blocked(PlacementCheck),
    /// A dry run: nothing was moved.
    DryRun(PlacementCheck),
}

/// Check `requests` against the live board, then run `apply` only when the
/// check passed, or found conflicts the caller allowed, and this is not a dry
/// run. A check that cannot complete is an error, so nothing is applied on
/// evidence that was never gathered.
pub(crate) fn check_then_apply<T>(
    client: &KiCadIpcClient,
    document: DocumentSpecifier,
    requests: &[PoseRequest],
    options: CheckOptions,
    apply: impl FnOnce(&KiCadIpcClient) -> anyhow::Result<T>,
) -> anyhow::Result<Checked<T>> {
    let check = check_live(client, document, requests)?;
    if options.dry_run {
        return Ok(Checked::DryRun(check));
    }
    if check.has_conflicts() && !options.allow_copper_conflicts {
        return Ok(Checked::Blocked(check));
    }
    let applied = apply(client)?;
    Ok(Checked::Applied { applied, check })
}

/// Resolve `requests` against the footprints KiCad holds and test the moved
/// pads against the board's vias, tracks and arcs. Reads only.
pub(crate) fn check_live(
    client: &KiCadIpcClient,
    document: DocumentSpecifier,
    requests: &[PoseRequest],
) -> anyhow::Result<PlacementCheck> {
    use kiapi::common::types::KiCadObjectType as Kot;

    let mut footprints = Vec::new();
    for item in client.get_items_in(document.clone(), Kot::KotPcbFootprint)? {
        if any_is(&item, "kiapi.board.types.FootprintInstance") {
            footprints.push(
                kiapi::board::types::FootprintInstance::decode(item.value.as_slice())
                    .context("KiCad returned an unreadable footprint")?,
            );
        }
    }
    let poses = plan_poses(&footprints, requests)?;
    test_copper(client, document, poses).map_err(|error| {
        // Copperpour's refusal, over the failure it rests on (#240).
        let refusal = PreflightRefusal::check_incomplete(
            "the copper check could not complete, so nothing was moved",
            &error,
        );
        error.context(refusal)
    })
}

/// The structured kind of a refusal Copperpour made inside `tool`'s
/// placement (#240), naming the argument it is about as `tool` names it:
/// `reference` or `rotation`, and `placements[i].reference` or
/// `placements[i].rotation` in a batch.
pub(crate) fn refusal_kind(
    tool: &str,
    requests: &[PoseRequest],
    refusal: &PreflightRefusal,
) -> ToolErrorKind {
    let argument = |reference: &str, name: &str| {
        let index = requests
            .iter()
            .position(|request| request.reference() == reference);
        match index {
            Some(index) if matches!(requests[index], PoseRequest::Set(_)) => {
                format!("placements[{index}].{name}")
            }
            _ => name.to_string(),
        }
    };
    crate::tools::pcb_board::preflight_refusal_kind(tool, refusal, argument)
}

/// Test the planned pads against the board's vias, tracks and arcs.
fn test_copper(
    client: &KiCadIpcClient,
    document: DocumentSpecifier,
    poses: Vec<PlannedPose>,
) -> anyhow::Result<PlacementCheck> {
    use kiapi::common::types::KiCadObjectType as Kot;

    let mut check = PlacementCheck {
        poses,
        conflicts: Vec::new(),
        pad_count: 0,
        layers: Vec::new(),
        via_count: 0,
        track_count: 0,
        arc_count: 0,
    };
    if check.poses.iter().all(|pose| pose.pads.is_empty()) {
        return Ok(check);
    }

    let copper: Vec<BoardLayer> = client
        .get_enabled_layers_in(document.clone())?
        .layers
        .iter()
        .filter_map(|layer| BoardLayer::try_from(layer.id).ok())
        .filter(|layer| is_copper(*layer))
        .collect();
    anyhow::ensure!(
        !copper.is_empty(),
        "KiCad reported no enabled copper layer on this board"
    );

    let vias = decode_all::<kiapi::board::types::Via>(
        client.get_items_in(document.clone(), Kot::KotPcbVia)?,
        "kiapi.board.types.Via",
    )?;
    let tracks = decode_all::<kiapi::board::types::Track>(
        client.get_items_in(document.clone(), Kot::KotPcbTrace)?,
        "kiapi.board.types.Track",
    )?;
    let arcs = decode_all::<kiapi::board::types::Arc>(
        client.get_items_in(document.clone(), Kot::KotPcbArc)?,
        "kiapi.board.types.Arc",
    )?;
    check.via_count = vias.len();
    check.track_count = tracks.len();
    check.arc_count = arcs.len();

    // `GetPadShapeAsPolygon` answers for layers a pad has no copper on, so
    // presence decides which layers hold copper. A pair KiCad leaves out or
    // calls unknown is tested as present: a needless test, never a missed one.
    let via_ids: Vec<String> = vias.iter().map(|via| kiid(&via.id)).collect();
    let mut ids: Vec<String> = check
        .poses
        .iter()
        .flat_map(|pose| pose.pads.iter().map(|pad| pad.kiid.clone()))
        .collect();
    ids.extend(via_ids.iter().cloned());
    let presence = client.check_padstack_presence_in(document.clone(), &ids, &copper)?;
    let present = |id: &str, layer: BoardLayer| {
        presence.get(&(id.to_string(), layer as i32))
            != Some(&kiapi::board::commands::PadstackPresence::PspNotPresent)
    };

    let mut found = Vec::new();
    let mut tested_pads = std::collections::HashSet::new();
    for (layer_index, &layer) in copper.iter().enumerate() {
        let on_layer: Vec<(usize, usize)> = check
            .poses
            .iter()
            .enumerate()
            .flat_map(|(pose_index, pose)| {
                pose.pads
                    .iter()
                    .enumerate()
                    .filter(|(_, pad)| present(&pad.kiid, layer))
                    .map(move |(pad_index, _)| (pose_index, pad_index))
            })
            .collect();
        if on_layer.is_empty() {
            continue;
        }
        let layer_name = layer_label(layer);
        let pad_ids: Vec<String> = on_layer
            .iter()
            .map(|&(pose, pad)| check.poses[pose].pads[pad].kiid.clone())
            .collect();
        let outlines = client.get_pad_shapes_as_polygons_in(document.clone(), &pad_ids, layer)?;

        let mut obstacles = Vec::new();
        for (via, id) in vias.iter().zip(&via_ids) {
            if present(id, layer) {
                obstacles.push(via_obstacle(via, layer)?);
            }
        }
        for track in tracks.iter().filter(|track| track.layer == layer as i32) {
            obstacles.push(track_obstacle(track)?);
        }
        for arc in arcs.iter().filter(|arc| arc.layer == layer as i32) {
            obstacles.push(arc_obstacle(arc)?);
        }

        for (&(pose_index, pad_index), mut outline) in on_layer.iter().zip(outlines) {
            let pose = &check.poses[pose_index];
            let pad = &pose.pads[pad_index];
            for xform in &pose.xforms {
                transform_polygon(&mut outline, xform);
            }
            let region = Region::from_polygon(&outline).with_context(|| {
                format!(
                    "KiCad's outline of {} pad {} on {layer_name}",
                    pose.reference, pad.number
                )
            })?;
            tested_pads.insert(pad.kiid.clone());
            for (obstacle_index, obstacle) in obstacles.iter().enumerate() {
                if same_net(&pad.net, &obstacle.net) || !region.touches_any(&obstacle.capsules) {
                    continue;
                }
                found.push((
                    (pose_index, pad_index, layer_index, obstacle_index),
                    CopperConflict {
                        reference: pose.reference.clone(),
                        pad: pad.number.clone(),
                        pad_net: pad.net.clone(),
                        layer: layer_name.clone(),
                        item_kind: obstacle.kind,
                        item_uuid: obstacle.uuid.clone(),
                        item_net: obstacle.net.clone(),
                    },
                ));
            }
        }
        check.layers.push(layer_name);
    }
    found.sort_by_key(|(order, _)| *order);
    check.conflicts = found.into_iter().map(|(_, conflict)| conflict).collect();
    check.pad_count = tested_pads.len();
    Ok(check)
}

/// Resolve each request against the footprint it names, as the placement
/// that follows will: a batch refuses a reference the board holds twice,
/// while a single move or rotation takes the first, as `move_footprint` and
/// `rotate_footprint` do.
fn plan_poses(
    footprints: &[kiapi::board::types::FootprintInstance],
    requests: &[PoseRequest],
) -> anyhow::Result<Vec<PlannedPose>> {
    let mut poses = Vec::with_capacity(requests.len());
    let mut missing = Vec::new();
    for request in requests {
        let reference = request.reference();
        let mut matches = footprints
            .iter()
            .filter(|footprint| footprint_reference(footprint) == reference);
        let Some(footprint) = matches.next() else {
            missing.push(reference);
            continue;
        };
        if matches!(request, PoseRequest::Set(_)) && matches.next().is_some() {
            return Err(PreflightRefusal::new(
                PreflightCause::DuplicateReference {
                    reference: reference.to_string(),
                },
                format!("footprint reference '{reference}' appears more than once on the board"),
            )
            .into());
        }
        let current = Pose::held_by(footprint);
        let target = match request {
            PoseRequest::Move { x_mm, y_mm, .. } => Pose {
                position: vec2(*x_mm, *y_mm),
                rotation_deg: current.rotation_deg,
            },
            PoseRequest::Rotate { rotation_deg, .. } => Pose {
                position: current.position,
                rotation_deg: *rotation_deg,
            },
            PoseRequest::Set(placement) => Pose {
                position: vec2(placement.x, placement.y),
                rotation_deg: placement.rotation,
            },
        };
        poses.push(PlannedPose {
            reference: reference.to_string(),
            current,
            target,
            xforms: placement_xforms(footprint, target.position, target.rotation_deg),
            pads: pads_of(footprint, reference)?,
        });
    }
    let reason = match missing.as_slice() {
        [] => return Ok(poses),
        [one] => format!("footprint {one} not found on board"),
        many => format!("footprints {} not found on board", many.join(", ")),
    };
    Err(PreflightRefusal::new(
        PreflightCause::MissingReferences {
            references: missing
                .iter()
                .map(|reference| reference.to_string())
                .collect(),
        },
        reason,
    )
    .into())
}

fn footprint_reference(footprint: &kiapi::board::types::FootprintInstance) -> &str {
    footprint
        .reference_field
        .as_ref()
        .and_then(|field| field.text.as_ref())
        .and_then(|text| text.text.as_ref())
        .map(|text| text.text.as_str())
        .unwrap_or("")
}

fn pads_of(
    footprint: &kiapi::board::types::FootprintInstance,
    reference: &str,
) -> anyhow::Result<Vec<PadCopper>> {
    let Some(definition) = footprint.definition.as_ref() else {
        return Ok(Vec::new());
    };
    let mut pads = Vec::new();
    for item in &definition.items {
        if !any_is(item, "kiapi.board.types.Pad") {
            continue;
        }
        let pad = kiapi::board::types::Pad::decode(item.value.as_slice())
            .with_context(|| format!("footprint '{reference}' has an unreadable pad"))?;
        let pad_kiid = kiid(&pad.id);
        anyhow::ensure!(
            !pad_kiid.is_empty(),
            "footprint '{reference}' pad '{}' has no KIID to ask KiCad its shape by",
            pad.number
        );
        pads.push(PadCopper {
            kiid: pad_kiid,
            number: pad.number,
            net: pad.net.map(|net| net.name).unwrap_or_default(),
        });
    }
    Ok(pads)
}

fn decode_all<M: Message + Default>(
    items: Vec<prost_types::Any>,
    type_name: &str,
) -> anyhow::Result<Vec<M>> {
    items
        .iter()
        .filter(|item| any_is(item, type_name))
        .map(|item| {
            M::decode(item.value.as_slice())
                .with_context(|| format!("KiCad returned an unreadable {type_name}"))
        })
        .collect()
}

fn kiid(id: &Option<kiapi::common::types::Kiid>) -> String {
    id.as_ref().map(|id| id.value.clone()).unwrap_or_default()
}

fn is_copper(layer: BoardLayer) -> bool {
    let value = layer as i32;
    (BoardLayer::BlFCu as i32..=BoardLayer::BlBCu as i32).contains(&value)
}

fn layer_label(layer: BoardLayer) -> String {
    copperpour_ipc::builders::layer_name(layer)
        .unwrap_or(layer.as_str_name())
        .to_string()
}

/// Two items share a net only when both name the same one. Unconnected
/// copper (no net) is never the same net as anything, so an unconnected pad
/// on unconnected copper still conflicts: they would be joined.
fn same_net(pad_net: &str, item_net: &str) -> bool {
    !pad_net.is_empty() && pad_net == item_net
}

type Point = (f64, f64);

fn point(vector: Vector2) -> Point {
    (vector.x_nm as f64, vector.y_nm as f64)
}

/// A via, track or arc of the board as the check sees it: its identity, its
/// net, and its copper as segments swept by a disk.
struct Obstacle {
    kind: &'static str,
    uuid: String,
    net: String,
    capsules: Vec<Capsule>,
}

/// The points within `radius_nm` of the segment `a`-`b`. A via is one with
/// `a == b`.
#[derive(Clone, Copy, Debug)]
struct Capsule {
    a: Point,
    b: Point,
    radius_nm: f64,
}

fn net_name(net: &Option<kiapi::board::types::Net>) -> String {
    net.as_ref().map(|net| net.name.clone()).unwrap_or_default()
}

/// A via as a disk of its pad diameter on `layer`.
fn via_obstacle(via: &kiapi::board::types::Via, layer: BoardLayer) -> anyhow::Result<Obstacle> {
    let uuid = kiid(&via.id);
    let center = point(
        via.position
            .with_context(|| format!("via {uuid} has no position"))?,
    );
    let diameter = via_diameter_nm(via, layer).with_context(|| format!("via {uuid}"))?;
    Ok(Obstacle {
        kind: "via",
        net: net_name(&via.net),
        capsules: vec![Capsule {
            a: center,
            b: center,
            radius_nm: diameter / 2.0,
        }],
        uuid,
    })
}

/// The via's pad diameter on `layer`: that layer's entry, the one shape of a
/// normal padstack, or the inner-layer entry (`In1.Cu`) of a front/inner/back
/// one. Failing those, the largest it has, which can only over-report.
fn via_diameter_nm(via: &kiapi::board::types::Via, layer: BoardLayer) -> anyhow::Result<f64> {
    use kiapi::board::types::PadStackType;
    let stack = via.pad_stack.as_ref().context("no padstack")?;
    let entry_for = |wanted: BoardLayer| {
        stack
            .copper_layers
            .iter()
            .find(|entry| entry.layer == wanted as i32)
    };
    let entry = entry_for(layer).or_else(|| match PadStackType::try_from(stack.r#type) {
        Ok(PadStackType::PstNormal) => stack.copper_layers.first(),
        Ok(PadStackType::PstFrontInnerBack)
            if layer != BoardLayer::BlFCu && layer != BoardLayer::BlBCu =>
        {
            entry_for(BoardLayer::BlIn1Cu)
        }
        _ => None,
    });
    let size_of = |entry: &kiapi::board::types::PadStackLayer| {
        entry.size.map(|size| size.x_nm.max(size.y_nm))
    };
    let diameter = entry
        .and_then(size_of)
        .or_else(|| stack.copper_layers.iter().filter_map(size_of).max())
        .context("no copper size")?;
    anyhow::ensure!(diameter > 0, "copper size {diameter} nm");
    Ok(diameter as f64)
}

fn track_obstacle(track: &kiapi::board::types::Track) -> anyhow::Result<Obstacle> {
    let uuid = kiid(&track.id);
    let (Some(start), Some(end)) = (track.start, track.end) else {
        anyhow::bail!("track {uuid} has no end points");
    };
    let width = track
        .width
        .with_context(|| format!("track {uuid} has no width"))?
        .value_nm;
    Ok(Obstacle {
        kind: "track",
        net: net_name(&track.net),
        capsules: vec![Capsule {
            a: point(start),
            b: point(end),
            radius_nm: width as f64 / 2.0,
        }],
        uuid,
    })
}

/// An arc track as chords, each widened by the chord error as well as half
/// the width, so the chords cover all the copper the arc has.
fn arc_obstacle(arc: &kiapi::board::types::Arc) -> anyhow::Result<Obstacle> {
    let uuid = kiid(&arc.id);
    let (Some(start), Some(mid), Some(end)) = (arc.start, arc.mid, arc.end) else {
        anyhow::bail!("arc {uuid} lacks a start, mid or end point");
    };
    let width = arc
        .width
        .with_context(|| format!("arc {uuid} has no width"))?
        .value_nm;
    let radius_nm = width as f64 / 2.0 + ARC_CHORD_ERROR_NM;
    let chords = flatten_arc(point(start), point(mid), point(end));
    Ok(Obstacle {
        kind: "arc",
        net: net_name(&arc.net),
        capsules: chords
            .windows(2)
            .map(|pair| Capsule {
                a: pair[0],
                b: pair[1],
                radius_nm,
            })
            .collect(),
        uuid,
    })
}

/// The points of the arc from `start` through `mid` to `end`, chords no
/// further than [`ARC_CHORD_ERROR_NM`] from it. Both ends are exact.
fn flatten_arc(start: Point, mid: Point, end: Point) -> Vec<Point> {
    use std::f64::consts::{FRAC_PI_2, TAU};

    // Work relative to `start`, so the products stay small.
    let (bx, by) = (mid.0 - start.0, mid.1 - start.1);
    let (cx, cy) = (end.0 - start.0, end.1 - start.1);
    let closed = cx == 0.0 && cy == 0.0;
    let chord_len = cx.hypot(cy);
    // Nearly straight: start-mid-end is already within the chord error.
    if !closed && (bx * cy - by * cx).abs() / chord_len <= ARC_CHORD_ERROR_NM {
        return vec![start, mid, end];
    }
    let (ux, uy, radius) = if closed {
        // A whole circle: `mid` is the far side of it.
        (bx / 2.0, by / 2.0, bx.hypot(by) / 2.0)
    } else {
        let d = 2.0 * (bx * cy - by * cx);
        let (b2, c2) = (bx * bx + by * by, cx * cx + cy * cy);
        let (ux, uy) = ((cy * b2 - by * c2) / d, (bx * c2 - cx * b2) / d);
        (ux, uy, ux.hypot(uy))
    };
    if !radius.is_finite() || radius == 0.0 {
        return vec![start, mid, end];
    }
    let angle = |x: f64, y: f64| (y - uy).atan2(x - ux);
    let a0 = angle(0.0, 0.0);
    let to_mid = (angle(bx, by) - a0).rem_euclid(TAU);
    let to_end = (angle(cx, cy) - a0).rem_euclid(TAU);
    let sweep = if closed {
        TAU
    } else if to_mid <= to_end {
        to_end
    } else {
        to_end - TAU
    };
    let step = if ARC_CHORD_ERROR_NM >= radius {
        FRAC_PI_2
    } else {
        2.0 * (1.0 - ARC_CHORD_ERROR_NM / radius).acos()
    };
    let count = ((sweep.abs() / step).ceil() as usize).max(1);
    let mut points = Vec::with_capacity(count + 1);
    points.push(start);
    for index in 1..count {
        let theta = a0 + sweep * index as f64 / count as f64;
        points.push((
            start.0 + ux + radius * theta.cos(),
            start.1 + uy + radius * theta.sin(),
        ));
    }
    points.push(end);
    points
}

/// A pad's copper on one layer: an outline and its holes as closed rings of
/// points, with the chord error it carries if any of its edges were arcs.
struct Region {
    rings: Vec<Vec<Point>>,
    slack_nm: f64,
    min: Point,
    max: Point,
}

impl Region {
    fn from_polygon(polygon: &kiapi::common::types::PolygonWithHoles) -> anyhow::Result<Self> {
        let outline = polygon.outline.as_ref().context("no outline")?;
        let mut flattened_arc = false;
        let mut rings = vec![ring_points(outline, &mut flattened_arc)?];
        anyhow::ensure!(
            rings[0].len() >= 3,
            "the outline has {} points, too few to enclose copper",
            rings[0].len()
        );
        for hole in &polygon.holes {
            rings.push(ring_points(hole, &mut flattened_arc)?);
        }
        let (mut min, mut max) = (rings[0][0], rings[0][0]);
        for &(x, y) in &rings[0] {
            min = (min.0.min(x), min.1.min(y));
            max = (max.0.max(x), max.1.max(y));
        }
        Ok(Self {
            rings,
            slack_nm: if flattened_arc {
                ARC_CHORD_ERROR_NM
            } else {
                0.0
            },
            min,
            max,
        })
    }

    /// Even-odd over the outline and its holes: inside the outline and
    /// outside every hole.
    fn contains(&self, (px, py): Point) -> bool {
        let mut inside = false;
        for ring in &self.rings {
            let Some(&last) = ring.last() else {
                continue;
            };
            let mut previous = last;
            for &current in ring {
                let ((xi, yi), (xj, yj)) = (current, previous);
                if (yi > py) != (yj > py) && px < (xj - xi) * (py - yi) / (yj - yi) + xi {
                    inside = !inside;
                }
                previous = current;
            }
        }
        inside
    }

    fn edges(&self) -> impl Iterator<Item = (Point, Point)> + '_ {
        self.rings.iter().flat_map(|ring| {
            ring.iter()
                .zip(ring.iter().cycle().skip(1))
                .map(|(&a, &b)| (a, b))
        })
    }

    /// Whether the capsule's copper and this region's share any point: the
    /// segment has a point inside, or comes within the radius of an edge.
    /// Touching counts; a gap of a nanometre does not.
    fn touches(&self, capsule: &Capsule) -> bool {
        let reach = capsule.radius_nm + self.slack_nm + TOUCH_TOLERANCE_NM;
        let (lo, hi) = (
            (capsule.a.0.min(capsule.b.0), capsule.a.1.min(capsule.b.1)),
            (capsule.a.0.max(capsule.b.0), capsule.a.1.max(capsule.b.1)),
        );
        if lo.0 - reach > self.max.0
            || hi.0 + reach < self.min.0
            || lo.1 - reach > self.max.1
            || hi.1 + reach < self.min.1
        {
            return false;
        }
        self.contains(capsule.a)
            || self.contains(capsule.b)
            || self
                .edges()
                .any(|(p, q)| segment_distance(p, q, capsule.a, capsule.b) <= reach)
    }

    fn touches_any(&self, capsules: &[Capsule]) -> bool {
        capsules.iter().any(|capsule| self.touches(capsule))
    }
}

/// One ring's points, arcs flattened.
fn ring_points(
    polyline: &kiapi::common::types::PolyLine,
    flattened_arc: &mut bool,
) -> anyhow::Result<Vec<Point>> {
    use kiapi::common::types::poly_line_node::Geometry;
    let mut points = Vec::with_capacity(polyline.nodes.len());
    for node in &polyline.nodes {
        match &node.geometry {
            Some(Geometry::Point(vertex)) => points.push(point(*vertex)),
            Some(Geometry::Arc(arc)) => {
                let (Some(start), Some(mid), Some(end)) = (arc.start, arc.mid, arc.end) else {
                    anyhow::bail!("an arc node lacks a start, mid or end point");
                };
                points.extend(flatten_arc(point(start), point(mid), point(end)));
                *flattened_arc = true;
            }
            None => anyhow::bail!("a node has no geometry"),
        }
    }
    Ok(points)
}

fn cross(o: Point, a: Point, b: Point) -> f64 {
    (a.0 - o.0) * (b.1 - o.1) - (a.1 - o.1) * (b.0 - o.0)
}

fn point_segment_distance(p: Point, a: Point, b: Point) -> f64 {
    let (dx, dy) = (b.0 - a.0, b.1 - a.1);
    let length_squared = dx * dx + dy * dy;
    if length_squared == 0.0 {
        return (p.0 - a.0).hypot(p.1 - a.1);
    }
    let t = (((p.0 - a.0) * dx + (p.1 - a.1) * dy) / length_squared).clamp(0.0, 1.0);
    (p.0 - (a.0 + t * dx)).hypot(p.1 - (a.1 + t * dy))
}

/// The distance between two segments: zero when they cross, else the
/// nearest an end of one comes to the other. Touching ends and collinear
/// overlaps land in the second case, at zero.
fn segment_distance(p1: Point, p2: Point, q1: Point, q2: Point) -> f64 {
    let (d1, d2) = (cross(q1, q2, p1), cross(q1, q2, p2));
    let (d3, d4) = (cross(p1, p2, q1), cross(p1, p2, q2));
    if d1 * d2 < 0.0 && d3 * d4 < 0.0 {
        return 0.0;
    }
    point_segment_distance(p1, q1, q2)
        .min(point_segment_distance(p2, q1, q2))
        .min(point_segment_distance(q1, p1, p2))
        .min(point_segment_distance(q2, p1, p2))
}

#[cfg(test)]
mod geometry_tests {
    use super::*;
    use kiapi::common::types::{poly_line_node, PolyLine, PolyLineNode, PolygonWithHoles};

    fn vector(x: i64, y: i64) -> Vector2 {
        Vector2 { x_nm: x, y_nm: y }
    }

    fn polyline(points: &[(i64, i64)]) -> PolyLine {
        PolyLine {
            nodes: points
                .iter()
                .map(|&(x, y)| PolyLineNode {
                    geometry: Some(poly_line_node::Geometry::Point(vector(x, y))),
                })
                .collect(),
            closed: true,
        }
    }

    fn region(points: &[(i64, i64)]) -> Region {
        Region::from_polygon(&PolygonWithHoles {
            outline: Some(polyline(points)),
            holes: vec![],
        })
        .unwrap()
    }

    /// An 0.8 mm square pad centred on the origin.
    fn square_pad() -> Region {
        region(&[
            (-400_000, -400_000),
            (400_000, -400_000),
            (400_000, 400_000),
            (-400_000, 400_000),
        ])
    }

    fn via(x: f64, y: f64, diameter: f64) -> Capsule {
        Capsule {
            a: (x, y),
            b: (x, y),
            radius_nm: diameter / 2.0,
        }
    }

    fn track(a: Point, b: Point, width: f64) -> Capsule {
        Capsule {
            a,
            b,
            radius_nm: width / 2.0,
        }
    }

    #[test]
    fn a_via_over_a_pad_edge_overlaps_it() {
        assert!(square_pad().touches(&via(600_000.0, 0.0, 600_000.0)));
        assert!(
            square_pad().touches(&via(0.0, 0.0, 600_000.0)),
            "fully on the pad"
        );
    }

    #[test]
    fn a_via_just_touching_a_pad_counts_and_one_a_nanometre_clear_does_not() {
        assert!(square_pad().touches(&via(700_000.0, 0.0, 600_000.0)));
        assert!(!square_pad().touches(&via(700_001.0, 0.0, 600_000.0)));
        // At a corner, the distance is to the corner itself.
        let diagonal = 400_000.0 + 300_000.0 / std::f64::consts::SQRT_2;
        assert!(square_pad().touches(&via(diagonal, diagonal, 600_000.0)));
        assert!(!square_pad().touches(&via(diagonal + 1.0, diagonal + 1.0, 600_000.0)));
    }

    #[test]
    fn a_track_touches_a_pad_at_half_its_width_and_not_a_nanometre_further() {
        let alongside = |y: f64| track((-1e6, y), (1e6, y), 250_000.0);
        assert!(square_pad().touches(&alongside(500_000.0)));
        assert!(square_pad().touches(&alongside(525_000.0)), "touching");
        assert!(!square_pad().touches(&alongside(525_001.0)), "near miss");
        // A track ending short of the pad still reaches it with its round end.
        let ending = |x: f64| track((2e6, 0.0), (x, 0.0), 250_000.0);
        assert!(square_pad().touches(&ending(525_000.0)));
        assert!(!square_pad().touches(&ending(525_001.0)));
        assert!(
            square_pad().touches(&ending(-2e6)),
            "crossing right through"
        );
    }

    /// The case point sampling missed (#96): a 45° track crossing a rotated
    /// pad, with no vertex of either inside the other. The pad is KiCad's own
    /// outline of R1's pad 1 at 45°.
    #[test]
    fn a_45_degree_track_crossing_a_rotated_pad_with_no_vertex_inside_overlaps() {
        let rotated = kiapi::board::commands::PadShapeAsPolygonResponse::decode(
            include_bytes!("../../tests/fixtures/issue_96_polygons_case2_rot45_F_Cu.ipc.bin")
                .as_slice(),
        )
        .unwrap();
        let pad = Region::from_polygon(&rotated.polygons[0]).unwrap();
        let centre = ((pad.min.0 + pad.max.0) / 2.0, (pad.min.1 + pad.max.1) / 2.0);
        for (dx, dy) in [(1.0, 1.0), (1.0, -1.0)] {
            let reach = 2e6;
            let crossing = track(
                (centre.0 - reach * dx, centre.1 - reach * dy),
                (centre.0 + reach * dx, centre.1 + reach * dy),
                100_000.0,
            );
            assert!(!pad.contains(crossing.a) && !pad.contains(crossing.b));
            assert!(
                pad.rings[0].iter().all(|&vertex| point_segment_distance(
                    vertex, crossing.a, crossing.b
                ) > crossing.radius_nm),
                "no pad vertex lies on the track"
            );
            assert!(pad.touches(&crossing), "direction ({dx}, {dy})");
        }
    }

    #[test]
    fn copper_in_a_pads_hole_is_not_on_its_copper() {
        let ring = Region::from_polygon(&PolygonWithHoles {
            outline: Some(polyline(&[
                (-1_000_000, -1_000_000),
                (1_000_000, -1_000_000),
                (1_000_000, 1_000_000),
                (-1_000_000, 1_000_000),
            ])),
            holes: vec![polyline(&[
                (-500_000, -500_000),
                (500_000, -500_000),
                (500_000, 500_000),
                (-500_000, 500_000),
            ])],
        })
        .unwrap();
        assert!(!ring.contains((0.0, 0.0)));
        assert!(ring.contains((750_000.0, 0.0)));
        assert!(!ring.touches(&via(0.0, 0.0, 200_000.0)));
        assert!(
            ring.touches(&via(0.0, 0.0, 1_000_000.0)),
            "reaching the hole's edge"
        );
    }

    #[test]
    fn only_the_same_named_net_is_the_same_net() {
        assert!(same_net("NET_C", "NET_C"));
        assert!(!same_net("NET_A", "NET_C"));
        assert!(!same_net("", "NET_C"));
        assert!(!same_net("", ""), "unconnected copper is no net");
    }

    /// An arc whose chord is far from the pad but whose copper bows onto it.
    fn bowing_arc(mid_y: i64) -> Obstacle {
        arc_obstacle(&kiapi::board::types::Arc {
            id: Some(kiapi::common::types::Kiid {
                value: "arc".into(),
            }),
            start: Some(vector(-2_000_000, 2_000_000)),
            mid: Some(vector(0, mid_y)),
            end: Some(vector(2_000_000, 2_000_000)),
            width: Some(kiapi::common::types::Distance { value_nm: 250_000 }),
            ..Default::default()
        })
        .unwrap()
    }

    #[test]
    fn an_arc_track_is_tested_along_its_curve_not_its_chord() {
        // With its mid at y = 600 000 the arc's copper reaches y = 475 000,
        // inside the pad's edge at y = 500 000; at 700 000 it stops 75 µm
        // short. The chord, at y = 2 000 000, is far from the pad either way.
        let pad = region(&[
            (-400_000, 0),
            (400_000, 0),
            (400_000, 500_000),
            (-400_000, 500_000),
        ]);
        assert!(pad.touches_any(&bowing_arc(600_000).capsules));
        assert!(
            !pad.touches_any(&bowing_arc(700_000).capsules),
            "75 µm clear"
        );
    }

    /// A pad reaching 10 nm into an arc track's copper, where the arc is
    /// furthest from its chords, still overlaps: the chords are widened by
    /// the chord error.
    #[test]
    fn an_arc_tracks_chords_never_hide_an_overlap_between_them() {
        let (radius, width) = (5_000_000.0f64, 250_000.0);
        let on_circle =
            |angle: f64| vector((radius * angle.cos()) as i64, (radius * angle.sin()) as i64);
        let track = arc_obstacle(&kiapi::board::types::Arc {
            id: Some(kiapi::common::types::Kiid {
                value: "arc".into(),
            }),
            start: Some(on_circle(-std::f64::consts::FRAC_PI_4)),
            mid: Some(on_circle(0.0)),
            end: Some(on_circle(std::f64::consts::FRAC_PI_4)),
            width: Some(kiapi::common::types::Distance {
                value_nm: width as i64,
            }),
            ..Default::default()
        })
        .unwrap();
        // Halfway between two chord ends, and a square pad there whose inner
        // edge lies 10 nm inside the arc's outer edge.
        let (a, b) = (track.capsules[0].a, track.capsules[0].b);
        let theta = (a.1.atan2(a.0) + b.1.atan2(b.0)) / 2.0;
        let (half, inner) = (100_000.0, radius + width / 2.0 - 10.0);
        let (radial, tangent) = ((theta.cos(), theta.sin()), (-theta.sin(), theta.cos()));
        let corner = |r: f64, t: f64| (r * radial.0 + t * tangent.0, r * radial.1 + t * tangent.1);
        let rings = vec![vec![
            corner(inner, -half),
            corner(inner + 2.0 * half, -half),
            corner(inner + 2.0 * half, half),
            corner(inner, half),
        ]];
        let (xs, ys): (Vec<f64>, Vec<f64>) = rings[0].iter().copied().unzip();
        let pad = Region {
            min: (
                xs.iter().copied().fold(f64::INFINITY, f64::min),
                ys.iter().copied().fold(f64::INFINITY, f64::min),
            ),
            max: (
                xs.iter().copied().fold(f64::NEG_INFINITY, f64::max),
                ys.iter().copied().fold(f64::NEG_INFINITY, f64::max),
            ),
            rings,
            slack_nm: 0.0,
        };
        assert!(pad.touches_any(&track.capsules));
        // The chords alone, at half the width, stop short: the test needs the
        // widening.
        let bare: Vec<Capsule> = track
            .capsules
            .iter()
            .map(|capsule| Capsule {
                radius_nm: width / 2.0,
                ..*capsule
            })
            .collect();
        assert!(!pad.touches_any(&bare));
    }

    #[test]
    fn flattened_arcs_stay_within_the_chord_error_and_follow_the_mid_point() {
        let radius = 5_000_000.0;
        let on_circle = |angle: f64| (radius * angle.cos(), radius * angle.sin());
        let (start, mid, end) = (on_circle(0.0), on_circle(0.6), on_circle(1.2));
        let points = flatten_arc(start, mid, end);
        assert_eq!((points[0], points[points.len() - 1]), (start, end));
        for pair in points.windows(2) {
            for point in [pair[0], pair[1]] {
                assert!((point.0.hypot(point.1) - radius).abs() < 1e-3);
            }
            let chord_mid = ((pair[0].0 + pair[1].0) / 2.0, (pair[0].1 + pair[1].1) / 2.0);
            let sagitta = radius - chord_mid.0.hypot(chord_mid.1);
            assert!(sagitta <= ARC_CHORD_ERROR_NM + 1e-6, "{sagitta}");
        }
        // The short way round, through `mid`, not the 5.1 rad complement.
        assert!(points.len() < 200, "{}", points.len());
        let nearest_to_mid = points
            .windows(2)
            .map(|pair| point_segment_distance(mid, pair[0], pair[1]))
            .fold(f64::INFINITY, f64::min);
        assert!(nearest_to_mid <= ARC_CHORD_ERROR_NM);
    }

    /// A round pad made of two arc nodes: a via touching the true circle
    /// between two chord ends still counts.
    #[test]
    fn a_pad_outline_of_arcs_never_hides_an_overlap_between_its_chords() {
        let arc = |start: (i64, i64), mid: (i64, i64), end: (i64, i64)| PolyLineNode {
            geometry: Some(poly_line_node::Geometry::Arc(
                kiapi::common::types::ArcStartMidEnd {
                    start: Some(vector(start.0, start.1)),
                    mid: Some(vector(mid.0, mid.1)),
                    end: Some(vector(end.0, end.1)),
                },
            )),
        };
        let disc = Region::from_polygon(&PolygonWithHoles {
            outline: Some(PolyLine {
                nodes: vec![
                    arc((500_000, 0), (0, 500_000), (-500_000, 0)),
                    arc((-500_000, 0), (0, -500_000), (500_000, 0)),
                ],
                closed: true,
            }),
            holes: vec![],
        })
        .unwrap();
        assert_eq!(disc.slack_nm, ARC_CHORD_ERROR_NM);
        assert!(disc.contains((0.0, 0.0)));
        let angle = 0.3f64;
        let at = |distance: f64| via(distance * angle.cos(), distance * angle.sin(), 600_000.0);
        assert!(disc.touches(&at(800_000.0)), "touching the true circle");
        assert!(!disc.touches(&at(800_000.0 + 2.0 * ARC_CHORD_ERROR_NM)));
    }

    #[test]
    fn the_options_default_to_false_and_refuse_a_non_boolean() {
        let options = CheckOptions::from_args(&json!({ "dry_run": null })).unwrap();
        assert!(!options.dry_run && !options.allow_copper_conflicts);
        let options =
            CheckOptions::from_args(&json!({ "dry_run": true, "allow_copper_conflicts": true }))
                .unwrap();
        assert!(options.dry_run && options.allow_copper_conflicts);
        let refused = CheckOptions::from_args(&json!({ "allow_copper_conflicts": "yes" }))
            .err()
            .unwrap();
        let text = match &refused.content[0] {
            ToolContent::Text { text } => text.clone(),
            other => panic!("{other:?}"),
        };
        let body: Value = serde_json::from_str(&text).unwrap();
        assert_eq!(body["error"]["kind"], "invalid_argument");
        assert_eq!(body["error"]["field"], "allow_copper_conflicts");
    }
}

/// The transform the check moves KiCad's `before` outlines by, against
/// KiCad's own outlines at the planned poses (`issue_96_ipc.README.md`,
/// `issue_96_layers_ipc.README.md`), and the poses it plans.
#[cfg(test)]
mod transform_evidence_tests {
    use super::*;
    use kiapi::board::commands::PadShapeAsPolygonResponse;

    const R1_PADS: [&str; 2] = [
        "868530b1-e4d1-41a1-9f30-416b9442cd39",
        "82783ffe-437f-4e7a-a719-240bb65baadd",
    ];

    fn r1() -> kiapi::board::types::FootprintInstance {
        kiapi::common::commands::GetItemsResponse::decode(
            include_bytes!("../../tests/fixtures/issue_96_footprints.ipc.bin").as_slice(),
        )
        .unwrap()
        .items
        .iter()
        .map(|item| kiapi::board::types::FootprintInstance::decode(item.value.as_slice()).unwrap())
        .find(|footprint| footprint_reference(footprint) == "R1")
        .unwrap()
    }

    fn outline(
        response: &PadShapeAsPolygonResponse,
        pad: &str,
    ) -> kiapi::common::types::PolygonWithHoles {
        let index = response.pads.iter().position(|id| id.value == pad).unwrap();
        response.polygons[index].clone()
    }

    fn vertices(polygon: &kiapi::common::types::PolygonWithHoles) -> Vec<(i64, i64)> {
        use kiapi::common::types::poly_line_node::Geometry;
        polygon
            .outline
            .as_ref()
            .unwrap()
            .nodes
            .iter()
            .map(|node| match node.geometry {
                Some(Geometry::Point(point)) => (point.x_nm, point.y_nm),
                ref other => panic!("KiCad sent {other:?}"),
            })
            .collect()
    }

    /// The same closed ring within `tolerance_nm` on each axis, wherever
    /// either starts.
    fn same_ring(ours: &[(i64, i64)], kicad: &[(i64, i64)], tolerance_nm: i64) -> bool {
        ours.len() == kicad.len()
            && (0..kicad.len()).any(|shift| {
                ours.iter().enumerate().all(|(index, &(x, y))| {
                    let (kx, ky) = kicad[(index + shift) % kicad.len()];
                    (x - kx).abs() <= tolerance_nm && (y - ky).abs() <= tolerance_nm
                })
            })
    }

    #[test]
    fn moved_outlines_match_kicads_own_at_the_planned_pose() {
        let before = PadShapeAsPolygonResponse::decode(
            include_bytes!("../../tests/fixtures/issue_96_polygons_before_F_Cu.ipc.bin").as_slice(),
        )
        .unwrap();
        let r1 = r1();
        let poses: [(&[u8], f64); 3] = [
            (
                include_bytes!("../../tests/fixtures/issue_96_polygons_case2_F_Cu.ipc.bin"),
                0.0,
            ),
            (
                include_bytes!("../../tests/fixtures/issue_96_polygons_case2_rot90_F_Cu.ipc.bin"),
                90.0,
            ),
            (
                include_bytes!("../../tests/fixtures/issue_96_polygons_case2_rot45_F_Cu.ipc.bin"),
                45.0,
            ),
        ];
        for (capture, rotation) in poses {
            let kicad = PadShapeAsPolygonResponse::decode(capture).unwrap();
            let xforms = placement_xforms(&r1, vec2(120.825, 100.0), rotation);
            for pad in R1_PADS {
                let mut moved = outline(&before, pad);
                for xform in &xforms {
                    transform_polygon(&mut moved, xform);
                }
                let (ours, theirs) = (vertices(&moved), vertices(&outline(&kicad, pad)));
                assert!(
                    same_ring(&ours, &theirs, 2),
                    "pad {pad} at {rotation}°:\n ours  {ours:?}\n KiCad {theirs:?}"
                );
                // The test can tell poses apart: the pre-move outline is not it.
                assert!(!same_ring(&vertices(&outline(&before, pad)), &theirs, 2));
            }
        }
    }

    /// The four-layer board's footprints as KiCad 10.0.8 holds them
    /// (`issue_96_layers_ipc.README.md`): R1 at (110, 95) 0°, R3 at
    /// (110, 105) 90°, and two R4s.
    fn layers_footprints() -> Vec<kiapi::board::types::FootprintInstance> {
        kiapi::common::commands::GetItemsResponse::decode(
            include_bytes!("../../tests/fixtures/issue_96_layers_footprints.ipc.bin").as_slice(),
        )
        .unwrap()
        .items
        .iter()
        .map(|item| kiapi::board::types::FootprintInstance::decode(item.value.as_slice()).unwrap())
        .collect()
    }

    fn move_to(reference: &str, x_mm: f64, y_mm: f64) -> PoseRequest {
        PoseRequest::Move {
            reference: reference.to_string(),
            x_mm,
            y_mm,
        }
    }

    /// `move_component` changes the position only: R3, which KiCad holds at
    /// 90°, is planned at 90° at its new position, not at 0°.
    #[test]
    fn a_move_keeps_the_rotation_kicad_holds() {
        let poses = plan_poses(&layers_footprints(), &[move_to("R3", 175.0, 90.0)]).unwrap();
        assert_eq!(
            poses[0].current,
            Pose {
                position: vec2(110.0, 105.0),
                rotation_deg: 90.0,
            }
        );
        assert_eq!(
            poses[0].target,
            Pose {
                position: vec2(175.0, 90.0),
                rotation_deg: 90.0,
            }
        );
    }

    /// Spots A-E move R1 (held at 0°) and spot F moves R3 (held at 90°),
    /// each as `move_component` plans it. KiCad's `before` outlines carried
    /// by the planned transforms match KiCad's own outlines with the
    /// footprint moved there (measured: 0 nm, and 1 nm at A, where `mm_to_nm`
    /// truncates 130.825 mm to 130 824 999 nm), so F also shows the held 90°
    /// is what a move keeps.
    #[test]
    fn moved_outlines_match_kicads_own_on_the_four_layer_board() {
        let before = PadShapeAsPolygonResponse::decode(
            include_bytes!("../../tests/fixtures/issue_96_layers_polygons_before_F_Cu.ipc.bin")
                .as_slice(),
        )
        .unwrap();
        let footprints = layers_footprints();
        let spots: [(&str, &str, f64, &[u8]); 6] = [
            (
                "A",
                "R1",
                130.825,
                include_bytes!("../../tests/fixtures/issue_96_layers_polygons_A_F_Cu.ipc.bin"),
            ),
            (
                "B",
                "R1",
                140.825,
                include_bytes!("../../tests/fixtures/issue_96_layers_polygons_B_F_Cu.ipc.bin"),
            ),
            (
                "C",
                "R1",
                146.825,
                include_bytes!("../../tests/fixtures/issue_96_layers_polygons_C_F_Cu.ipc.bin"),
            ),
            (
                "D",
                "R1",
                155.825,
                include_bytes!("../../tests/fixtures/issue_96_layers_polygons_D_F_Cu.ipc.bin"),
            ),
            (
                "E",
                "R1",
                165.825,
                include_bytes!("../../tests/fixtures/issue_96_layers_polygons_E_F_Cu.ipc.bin"),
            ),
            (
                "F",
                "R3",
                175.0,
                include_bytes!("../../tests/fixtures/issue_96_layers_polygons_F_F_Cu.ipc.bin"),
            ),
        ];
        for (spot, reference, x, capture) in spots {
            let kicad = PadShapeAsPolygonResponse::decode(capture).unwrap();
            let poses = plan_poses(&footprints, &[move_to(reference, x, 90.0)]).unwrap();
            assert_eq!(poses[0].pads.len(), 2, "spot {spot}");
            for pad in &poses[0].pads {
                let mut moved = outline(&before, &pad.kiid);
                for xform in &poses[0].xforms {
                    transform_polygon(&mut moved, xform);
                }
                let (ours, theirs) = (vertices(&moved), vertices(&outline(&kicad, &pad.kiid)));
                assert!(
                    same_ring(&ours, &theirs, 2),
                    "spot {spot}, {reference} pad {}:\n ours  {ours:?}\n KiCad {theirs:?}",
                    pad.number
                );
                assert!(!same_ring(
                    &vertices(&outline(&before, &pad.kiid)),
                    &theirs,
                    2
                ));
            }
        }
    }
}

/// The three tools through served `tools/call`, against a KiCad double that
/// replays KiCad 10.0.8's recorded answers for the #96 repro board
/// (`issue_96_ipc.README.md`).
#[cfg(test)]
mod served_tests {
    use crate::mcp::handler::McpHandler;
    use crate::test_support::MockIpcServer;
    use crate::tools::pcb_board::board_mock::{spawn_kicad_holding_items, ItemStore};
    use copperpour_ipc::builders::{any_type_name, pack_any};
    use copperpour_ipc::gen::kiapi;
    use kiapi::board::commands as board_commands;
    use kiapi::board::types::BoardLayer;
    use kiapi::common::types::KiCadObjectType as Kot;
    use prost::Message;
    use serde_json::{json, Value};
    use std::sync::{Arc, Mutex};

    const FOOTPRINTS: &[u8] = include_bytes!("../../tests/fixtures/issue_96_footprints.ipc.bin");
    const TRACKS: &[u8] = include_bytes!("../../tests/fixtures/issue_96_tracks.ipc.bin");
    const VIAS: &[u8] = include_bytes!("../../tests/fixtures/issue_96_vias.ipc.bin");
    const ARCS: &[u8] = include_bytes!("../../tests/fixtures/issue_96_arcs.ipc.bin");
    const ENABLED_LAYERS: &[u8] =
        include_bytes!("../../tests/fixtures/issue_96_enabled_layers.ipc.bin");
    const PRESENCE: &[u8] = include_bytes!("../../tests/fixtures/issue_96_presence.ipc.bin");
    const OUTLINES_F_CU: &[u8] =
        include_bytes!("../../tests/fixtures/issue_96_polygons_before_F_Cu.ipc.bin");
    const OUTLINES_B_CU: &[u8] =
        include_bytes!("../../tests/fixtures/issue_96_polygons_before_B_Cu.ipc.bin");
    const BOARD: &str = include_str!("../../tests/fixtures/issue_96.kicad_pcb");

    // The repro board's copper, as KiCad names it.
    const LONE_VIA: &str = "424e71ba-90b9-4287-835a-410e3510db12";
    const TIED_VIA: &str = "df431fc3-7c5e-4772-b3f8-33744514f439";
    const R2_TRACK: &str = "0335fe60-230b-4695-95ac-698836799eeb";
    const STUB: &str = "3236141c-2c15-4b0f-9c9f-b39f826067c1";
    const TIED_TRACK: &str = "e86c73df-a536-483a-8c1e-cf0b7f96cf64";

    /// One board's recorded KiCad answers, as [`replay_from`] serves them.
    struct Capture {
        /// The file name KiCad holds the board under.
        board_name: &'static str,
        board: &'static str,
        footprints: &'static [u8],
        tracks: &'static [u8],
        vias: &'static [u8],
        arcs: &'static [u8],
        enabled_layers: &'static [u8],
        presence: &'static [u8],
        /// `GetPadShapeAsPolygon` for every pad as placed, by layer.
        outlines: &'static [(BoardLayer, &'static [u8])],
    }

    /// The #96 repro board (`issue_96_ipc.README.md`).
    static ISSUE_96: Capture = Capture {
        board_name: "issue_96.kicad_pcb",
        board: BOARD,
        footprints: FOOTPRINTS,
        tracks: TRACKS,
        vias: VIAS,
        arcs: ARCS,
        enabled_layers: ENABLED_LAYERS,
        presence: PRESENCE,
        outlines: &[
            (BoardLayer::BlFCu, OUTLINES_F_CU),
            (BoardLayer::BlBCu, OUTLINES_B_CU),
        ],
    };

    /// The four-layer board with copper on B.Cu, blind vias and arc tracks
    /// (`issue_96_layers_ipc.README.md`).
    static LAYERS: Capture = Capture {
        board_name: "issue_96_layers.kicad_pcb",
        board: include_str!("../../tests/fixtures/issue_96_layers.kicad_pcb"),
        footprints: include_bytes!("../../tests/fixtures/issue_96_layers_footprints.ipc.bin"),
        tracks: include_bytes!("../../tests/fixtures/issue_96_layers_tracks.ipc.bin"),
        vias: include_bytes!("../../tests/fixtures/issue_96_layers_vias.ipc.bin"),
        arcs: include_bytes!("../../tests/fixtures/issue_96_layers_arcs.ipc.bin"),
        enabled_layers: include_bytes!(
            "../../tests/fixtures/issue_96_layers_enabled_layers.ipc.bin"
        ),
        presence: include_bytes!("../../tests/fixtures/issue_96_layers_presence.ipc.bin"),
        outlines: &[
            (
                BoardLayer::BlFCu,
                include_bytes!("../../tests/fixtures/issue_96_layers_polygons_before_F_Cu.ipc.bin"),
            ),
            (
                BoardLayer::BlIn1Cu,
                include_bytes!(
                    "../../tests/fixtures/issue_96_layers_polygons_before_In1_Cu.ipc.bin"
                ),
            ),
            (
                BoardLayer::BlIn2Cu,
                include_bytes!(
                    "../../tests/fixtures/issue_96_layers_polygons_before_In2_Cu.ipc.bin"
                ),
            ),
            (
                BoardLayer::BlBCu,
                include_bytes!("../../tests/fixtures/issue_96_layers_polygons_before_B_Cu.ipc.bin"),
            ),
        ],
    };

    // The four-layer board's NET_C copper on F.Cu, as KiCad names it.
    const VIA_F_TO_IN1_CU: &str = "ab6f8834-1aac-4fb7-b536-bb2b9e1d69cf";
    const F_CU_ARC: &str = "37d64abc-9509-4951-b192-03a717da9b67";
    const F_CU_TRACK: &str = "2b77c1c4-22b4-48fe-bffd-db391fd65958";

    fn recorded(type_name: &str, bytes: &[u8]) -> prost_types::Any {
        prost_types::Any {
            type_url: format!("type.googleapis.com/{type_name}"),
            value: bytes.to_vec(),
        }
    }

    /// [`replay_from`] the #96 repro board.
    fn replay(command: &prost_types::Any) -> Option<prost_types::Any> {
        replay_from(&ISSUE_96, command)
    }

    /// KiCad's recorded answer to the commands the check sends, cut down to
    /// the items and layers asked for, as KiCad answers. Footprints come from
    /// the store, so an applied move is read back.
    fn replay_from(capture: &Capture, command: &prost_types::Any) -> Option<prost_types::Any> {
        match any_type_name(command) {
            "kiapi.common.commands.GetItems" => {
                let request =
                    kiapi::common::commands::GetItems::decode(command.value.as_slice()).unwrap();
                let bytes = match request.types.as_slice() {
                    [kind] if *kind == Kot::KotPcbTrace as i32 => capture.tracks,
                    [kind] if *kind == Kot::KotPcbVia as i32 => capture.vias,
                    [kind] if *kind == Kot::KotPcbArc as i32 => capture.arcs,
                    _ => return None,
                };
                Some(recorded("kiapi.common.commands.GetItemsResponse", bytes))
            }
            "kiapi.board.commands.GetBoardEnabledLayers" => Some(recorded(
                "kiapi.board.commands.BoardEnabledLayersResponse",
                capture.enabled_layers,
            )),
            "kiapi.board.commands.CheckPadstackPresenceOnLayers" => {
                let request =
                    board_commands::CheckPadstackPresenceOnLayers::decode(command.value.as_slice())
                        .unwrap();
                let mut answer =
                    board_commands::PadstackPresenceResponse::decode(capture.presence).unwrap();
                answer.entries.retain(|entry| {
                    request.items.contains(entry.item.as_ref().unwrap())
                        && request.layers.contains(&entry.layer)
                });
                Some(pack_any(
                    &answer,
                    "kiapi.board.commands.PadstackPresenceResponse",
                ))
            }
            "kiapi.board.commands.GetPadShapeAsPolygon" => {
                let request =
                    board_commands::GetPadShapeAsPolygon::decode(command.value.as_slice()).unwrap();
                let &(_, bytes) = capture
                    .outlines
                    .iter()
                    .find(|(layer, _)| *layer as i32 == request.layer)?;
                let all = board_commands::PadShapeAsPolygonResponse::decode(bytes).unwrap();
                let mut answer = board_commands::PadShapeAsPolygonResponse::default();
                for pad in &request.pads {
                    if let Some(index) = all.pads.iter().position(|id| id == pad) {
                        answer.pads.push(all.pads[index].clone());
                        answer.polygons.push(all.polygons[index].clone());
                    }
                }
                Some(pack_any(
                    &answer,
                    "kiapi.board.commands.PadShapeAsPolygonResponse",
                ))
            }
            _ => None,
        }
    }

    struct Kicad {
        _dir: tempfile::TempDir,
        board: std::path::PathBuf,
        /// The board file as written, to prove it unchanged.
        board_text: &'static str,
        store: Arc<Mutex<ItemStore>>,
        sent: Arc<Mutex<Vec<String>>>,
        handler: McpHandler,
        _server: MockIpcServer,
    }

    /// A KiCad holding the repro board as recorded. `answer` stands in for
    /// it on the check's commands; [`replay`] is the faithful one.
    async fn kicad_with(
        answer: impl Fn(&prost_types::Any) -> Option<prost_types::Any> + Send + 'static,
    ) -> Kicad {
        kicad_holding(&ISSUE_96, answer).await
    }

    /// A KiCad holding the four-layer board, replaying its recorded answers.
    async fn layers_kicad() -> Kicad {
        kicad_holding(&LAYERS, |command| replay_from(&LAYERS, command)).await
    }

    /// A KiCad holding `capture`'s board, its footprints in the store.
    async fn kicad_holding(
        capture: &Capture,
        answer: impl Fn(&prost_types::Any) -> Option<prost_types::Any> + Send + 'static,
    ) -> Kicad {
        let dir = tempfile::tempdir().unwrap();
        let board = dir.path().join(capture.board_name);
        std::fs::write(&board, capture.board).unwrap();
        let store = Arc::new(Mutex::new(ItemStore {
            items: kiapi::common::commands::GetItemsResponse::decode(capture.footprints)
                .unwrap()
                .items,
            ..Default::default()
        }));
        let sent = Arc::new(Mutex::new(Vec::new()));
        let log = sent.clone();
        let server = spawn_kicad_holding_items(&board, vec![], store.clone(), |_| {}, {
            move |command, _| {
                log.lock().unwrap().push(command_name(command));
                answer(command)
            }
        });
        let handler = McpHandler::new(crate::tools::ServerConfig {
            ipc_address: server.address().to_string(),
            eager_toolsets: true,
            ..Default::default()
        })
        .await
        .unwrap();
        Kicad {
            _dir: dir,
            board,
            board_text: capture.board,
            store,
            sent,
            handler,
            _server: server,
        }
    }

    async fn kicad() -> Kicad {
        kicad_with(replay).await
    }

    /// The command's short name, with what an `EndCommit` did to the commit:
    /// `EndCommit(drop)` or `EndCommit(commit)`.
    fn command_name(command: &prost_types::Any) -> String {
        let name = any_type_name(command).rsplit('.').next().unwrap();
        if name != "EndCommit" {
            return name.to_string();
        }
        let end = kiapi::common::commands::EndCommit::decode(command.value.as_slice()).unwrap();
        match kiapi::common::commands::CommitAction::try_from(end.action) {
            Ok(kiapi::common::commands::CommitAction::CmaDrop) => "EndCommit(drop)".to_string(),
            Ok(kiapi::common::commands::CommitAction::CmaCommit) => "EndCommit(commit)".to_string(),
            other => format!("EndCommit({other:?})"),
        }
    }

    impl Kicad {
        async fn call(&self, tool: &str, mut arguments: Value) -> (bool, Value) {
            arguments["board"] = json!(self.board.to_string_lossy());
            let response = self
                .handler
                .handle_message(json!({
                    "jsonrpc": "2.0",
                    "id": 96,
                    "method": "tools/call",
                    "params": { "name": tool, "arguments": arguments }
                }))
                .await
                .unwrap();
            let result = response.result.unwrap();
            let text = result["content"][0]["text"].as_str().unwrap();
            (
                result["isError"] == json!(true),
                serde_json::from_str(text).unwrap_or_else(|_| json!(text)),
            )
        }

        fn sent(&self, command: &str) -> bool {
            self.sent.lock().unwrap().iter().any(|sent| sent == command)
        }

        /// Asserts nothing that could change the board reached KiCad.
        fn assert_untouched(&self) {
            assert_eq!(
                self.store.lock().unwrap().updates,
                0,
                "UpdateItems reached KiCad"
            );
            assert!(!self.sent("UpdateItems") && !self.sent("BeginCommit"));
        }

        /// Asserts every command that reached KiCad only read the board.
        fn assert_only_reads(&self) {
            let sent = self.sent.lock().unwrap();
            assert!(!sent.is_empty());
            for command in sent.iter() {
                assert!(
                    matches!(
                        command.as_str(),
                        "GetItems"
                            | "GetBoardEnabledLayers"
                            | "CheckPadstackPresenceOnLayers"
                            | "GetPadShapeAsPolygon"
                    ),
                    "{command} reached KiCad: {sent:?}"
                );
            }
        }

        /// The anchor and angle KiCad holds for `reference`, in nm and degrees.
        fn held(&self, reference: &str) -> (i64, i64, f64) {
            let store = self.store.lock().unwrap();
            let footprint = store
                .items
                .iter()
                .map(|item| {
                    kiapi::board::types::FootprintInstance::decode(item.value.as_slice()).unwrap()
                })
                .find(|footprint| super::footprint_reference(footprint) == reference)
                .unwrap();
            let position = footprint.position.unwrap();
            (
                position.x_nm,
                position.y_nm,
                footprint
                    .orientation
                    .map_or(0.0, |angle| angle.value_degrees),
            )
        }
    }

    /// One expected conflict, written out from the board's geometry rather
    /// than read from the code under test.
    fn conflict(reference: &str, pad: &str, pad_net: &str, kind: &str, uuid: &str) -> Value {
        json!({
            "reference": reference,
            "pad": pad,
            "pad_net": pad_net,
            "layer": "F.Cu",
            "item_kind": kind,
            "item_uuid": uuid,
            "item_net": "NET_C",
        })
    }

    fn r1_to(x: f64, y: f64) -> Value {
        json!({ "placements": [{ "reference": "R1", "x": x, "y": y, "rotation": 0.0 }] })
    }

    fn assert_refused(body: &Value, operation: &str, expected: &[Value]) {
        assert_eq!(body["error"]["kind"], "plan_blocked", "{body:#}");
        assert_eq!(body["error"]["operation"], operation, "{body:#}");
        assert_eq!(body["copper_check"]["status"], "conflicts", "{body:#}");
        assert_eq!(
            body["copper_check"]["conflicts"],
            json!(expected),
            "{body:#}"
        );
        assert_eq!(
            body["error"]["reasons"].as_array().map(Vec::len),
            Some(expected.len()),
            "{body:#}"
        );
        assert!(
            body["message"]
                .as_str()
                .unwrap()
                .contains("Nothing was moved"),
            "{body:#}"
        );
    }

    /// Case 1: pad 1 (114.2 - 0.825 = 113.375, ±0.4) lies on the 0.25 mm
    /// track along y = 106; pad 2 (115.025 ± 0.4) covers both the track's end
    /// and the via at (115, 106). DRC catches this one.
    #[tokio::test]
    async fn case_1_on_the_tied_track_and_via_is_refused_before_any_update() {
        let kicad = kicad().await;
        let (is_error, body) = kicad
            .call("set_component_placements", r1_to(114.2, 106.0))
            .await;
        assert!(is_error, "{body:#}");
        assert_refused(
            &body,
            "set_component_placements",
            &[
                conflict("R1", "1", "NET_A", "track", TIED_TRACK),
                conflict("R1", "2", "NET_B", "via", TIED_VIA),
                conflict("R1", "2", "NET_B", "track", TIED_TRACK),
            ],
        );
        kicad.assert_untouched();
        assert_eq!(kicad.held("R1"), (110_000_000, 95_000_000, 0.0));
    }

    /// Case 2: pad 1 lands at (120, 100) on the lone via, the case DRC cannot
    /// see. KiCad reports the pads' outlines on B.Cu too, where the via also
    /// is, but the pads have no copper there, so nothing is reported on B.Cu.
    #[tokio::test]
    async fn case_2_on_the_lone_via_is_refused_on_f_cu_only() {
        let kicad = kicad().await;
        let (is_error, body) = kicad
            .call("set_component_placements", r1_to(120.825, 100.0))
            .await;
        assert!(is_error, "{body:#}");
        assert_refused(
            &body,
            "set_component_placements",
            &[conflict("R1", "1", "NET_A", "via", LONE_VIA)],
        );
        assert_eq!(body["copper_check"]["coverage"]["layers"], json!(["F.Cu"]));
        assert_eq!(body["copper_check"]["coverage"]["pad_count"], 2);
        kicad.assert_untouched();
    }

    /// Case 3: both pads (125 ± 0.4 and 126.65 ± 0.4) lie on the stub from
    /// x = 125 to 128.
    #[tokio::test]
    async fn case_3_on_the_stub_is_refused() {
        let kicad = kicad().await;
        let (is_error, body) = kicad
            .call("set_component_placements", r1_to(125.825, 100.0))
            .await;
        assert!(is_error, "{body:#}");
        assert_refused(
            &body,
            "set_component_placements",
            &[
                conflict("R1", "1", "NET_A", "track", STUB),
                conflict("R1", "2", "NET_B", "track", STUB),
            ],
        );
        kicad.assert_untouched();
    }

    #[tokio::test]
    async fn a_dry_run_reports_the_conflicts_and_poses_and_moves_nothing() {
        let kicad = kicad().await;
        let mut arguments = r1_to(120.825, 100.0);
        arguments["dry_run"] = json!(true);
        let (is_error, body) = kicad.call("set_component_placements", arguments).await;
        assert!(!is_error, "{body:#}");
        assert_eq!(body["dry_run"], true);
        assert_eq!(body["source"], "ipc");
        assert_eq!(
            body["copper_check"]["conflicts"],
            json!([conflict("R1", "1", "NET_A", "via", LONE_VIA)])
        );
        assert_eq!(
            body["placements"],
            json!([{
                "reference": "R1",
                "current": { "x": 110.0, "y": 95.0, "rotation": 0.0 },
                "target": { "x": 120.825, "y": 100.0, "rotation": 0.0 },
            }])
        );
        kicad.assert_untouched();
    }

    #[tokio::test]
    async fn allowed_conflicts_are_applied_and_still_reported() {
        let kicad = kicad().await;
        let mut arguments = r1_to(120.825, 100.0);
        arguments["allow_copper_conflicts"] = json!(true);
        let (is_error, body) = kicad.call("set_component_placements", arguments).await;
        assert!(!is_error, "{body:#}");
        assert_eq!(kicad.store.lock().unwrap().updates, 1);
        assert_eq!(kicad.held("R1"), (120_825_000, 100_000_000, 0.0));
        assert_eq!(body["placements"][0]["x"], 120.825, "read back: {body:#}");
        assert_eq!(body["copper_check"]["status"], "conflicts");
        assert_eq!(
            body["copper_check"]["conflicts"],
            json!([conflict("R1", "1", "NET_A", "via", LONE_VIA)])
        );
        assert!(body["warning"]
            .as_str()
            .unwrap()
            .contains("allow_copper_conflicts"));
    }

    #[tokio::test]
    async fn a_clear_move_applies_with_an_empty_list() {
        let kicad = kicad().await;
        let (is_error, body) = kicad
            .call("set_component_placements", r1_to(110.0, 90.0))
            .await;
        assert!(!is_error, "{body:#}");
        assert_eq!(kicad.held("R1"), (110_000_000, 90_000_000, 0.0));
        assert_eq!(body["copper_check"]["status"], "clear");
        assert_eq!(body["copper_check"]["conflicts"], json!([]));
        assert_eq!(
            body["copper_check"]["coverage"],
            json!({
                "pad_count": 2,
                "layers": ["F.Cu"],
                "via_count": 2,
                "track_count": 3,
                "arc_count": 0,
            })
        );
        assert!(body.get("warning").is_none(), "{body:#}");
    }

    /// R2's pad 1 is NET_C and sits on the NET_C track it is routed to; a
    /// small move keeps it there and pad 2 (NET_D) clear. Overlap with
    /// copper of the pad's own net is not a conflict.
    #[tokio::test]
    async fn copper_of_the_pads_own_net_is_not_a_conflict() {
        let kicad = kicad().await;
        let (is_error, body) = kicad
            .call(
                "set_component_placements",
                json!({ "placements": [{ "reference": "R2", "x": 100.0, "y": 109.9, "rotation": 0.0 }] }),
            )
            .await;
        assert!(!is_error, "{body:#}");
        assert_eq!(body["copper_check"]["status"], "clear", "{body:#}");
        assert_eq!(kicad.held("R2"), (100_000_000, 109_900_000, 0.0));
    }

    #[tokio::test]
    async fn move_component_refuses_the_same_way() {
        let kicad = kicad().await;
        let (is_error, body) = kicad
            .call(
                "move_component",
                json!({ "reference": "R1", "x": 120.825, "y": 100.0 }),
            )
            .await;
        assert!(is_error, "{body:#}");
        assert_refused(
            &body,
            "move_component",
            &[conflict("R1", "1", "NET_A", "via", LONE_VIA)],
        );
        kicad.assert_untouched();
    }

    /// Turning R2 half round about (100, 110) puts pad 2 (NET_D) where pad 1
    /// was, on the end of pad 1's NET_C track.
    #[tokio::test]
    async fn rotate_component_refuses_the_same_way() {
        let kicad = kicad().await;
        let (is_error, body) = kicad
            .call(
                "rotate_component",
                json!({ "reference": "R2", "rotation": 180.0 }),
            )
            .await;
        assert!(is_error, "{body:#}");
        assert_refused(
            &body,
            "rotate_component",
            &[conflict("R2", "2", "NET_D", "track", R2_TRACK)],
        );
        kicad.assert_untouched();
    }

    #[tokio::test]
    async fn move_and_rotate_report_a_clear_check_when_they_apply() {
        let kicad = kicad().await;
        let (is_error, body) = kicad
            .call(
                "move_component",
                json!({ "reference": "R1", "x": 110.0, "y": 90.0 }),
            )
            .await;
        assert!(!is_error, "{body:#}");
        assert_eq!(body["copper_check"]["status"], "clear", "{body:#}");
        let (is_error, body) = kicad
            .call(
                "rotate_component",
                json!({ "reference": "R1", "rotation": 90.0 }),
            )
            .await;
        assert!(!is_error, "{body:#}");
        assert_eq!(body["copper_check"]["status"], "clear", "{body:#}");
        assert_eq!(kicad.store.lock().unwrap().updates, 2);
    }

    /// A pad KiCad gives no outline for cannot be checked, so the move is
    /// refused, never applied on a check that did not run. The refusal is
    /// Copperpour's, decided before any commit, and no argument is at fault:
    /// `plan_blocked`, naming the tool (#240).
    #[tokio::test]
    async fn a_check_that_cannot_complete_moves_nothing() {
        let reason = format!(
            "the copper check could not complete, so nothing was moved: KiCad returned no \
             copper outline for pad {} on F.Cu",
            first_pad_kiid(FOOTPRINTS, "R1")
        );
        let mut placement = r1_to(110.0, 90.0);
        placement["allow_copper_conflicts"] = json!(true);
        for (tool, what, arguments) in [
            (
                "set_component_placements",
                "component placement batch",
                placement,
            ),
            (
                "move_component",
                "move",
                json!({ "reference": "R1", "x": 110.0, "y": 90.0 }),
            ),
            (
                "rotate_component",
                "rotation",
                json!({ "reference": "R1", "rotation": 90.0 }),
            ),
        ] {
            let kicad = kicad_with(|command| {
                if any_type_name(command) == "kiapi.board.commands.GetPadShapeAsPolygon" {
                    return Some(pack_any(
                        &board_commands::PadShapeAsPolygonResponse::default(),
                        "kiapi.board.commands.PadShapeAsPolygonResponse",
                    ));
                }
                replay(command)
            })
            .await;
            let (is_error, body) = kicad.call(tool, arguments).await;
            assert!(is_error, "{tool}: {body:#}");
            kicad.assert_own_refusal(
                &body,
                what,
                &reason,
                json!({ "kind": "plan_blocked", "operation": tool, "reasons": [reason] }),
                &[],
            );
            kicad.assert_only_reads();
        }
    }

    /// With no live KiCad the file is still edited, but the check is reported
    /// as not run, never as clear; a dry run there writes nothing.
    #[tokio::test]
    async fn the_closed_file_path_reports_the_check_as_not_run() {
        let dir = tempfile::tempdir().unwrap();
        let board = dir.path().join("issue_96.kicad_pcb");
        std::fs::write(&board, BOARD).unwrap();
        let handler = McpHandler::new(crate::tools::ServerConfig {
            ipc_address: String::new(),
            eager_toolsets: true,
            ..Default::default()
        })
        .await
        .unwrap();
        let call = |tool: &'static str, mut arguments: Value| {
            arguments["board"] = json!(board.to_string_lossy());
            let handler = &handler;
            async move {
                let response = handler
                    .handle_message(json!({
                        "jsonrpc": "2.0",
                        "id": 96,
                        "method": "tools/call",
                        "params": { "name": tool, "arguments": arguments }
                    }))
                    .await
                    .unwrap();
                let result = response.result.unwrap();
                let body: Value =
                    serde_json::from_str(result["content"][0]["text"].as_str().unwrap()).unwrap();
                (result["isError"] == json!(true), body)
            }
        };
        let not_run = |body: &Value| {
            assert_eq!(body["source"], "file", "{body:#}");
            assert_eq!(body["copper_check"]["status"], "not_run", "{body:#}");
            assert_eq!(body["copper_check"]["reason"], "no_live_board", "{body:#}");
            assert!(body["copper_check"].get("conflicts").is_none(), "{body:#}");
        };

        let mut dry_run = r1_to(120.825, 100.0);
        dry_run["dry_run"] = json!(true);
        let (is_error, body) = call("set_component_placements", dry_run).await;
        assert!(!is_error, "{body:#}");
        not_run(&body);
        assert_eq!(body["dry_run"], true);
        assert_eq!(
            body["placements"],
            json!([{
                "reference": "R1",
                "current": { "x": 110.0, "y": 95.0, "rotation": 0.0 },
                "target": { "x": 120.825, "y": 100.0, "rotation": 0.0 },
            }])
        );
        for (tool, arguments, target) in [
            (
                "move_component",
                json!({ "reference": "R1", "x": 120.825, "y": 100.0, "dry_run": true }),
                json!({ "x": 120.825, "y": 100.0, "rotation": 0.0 }),
            ),
            (
                "rotate_component",
                json!({ "reference": "R1", "rotation": 90.0, "dry_run": true }),
                json!({ "x": 110.0, "y": 95.0, "rotation": 90.0 }),
            ),
        ] {
            let (is_error, body) = call(tool, arguments).await;
            assert!(!is_error, "{tool}: {body:#}");
            not_run(&body);
            assert_eq!(body["placements"][0]["target"], target, "{tool}: {body:#}");
        }
        assert_eq!(
            std::fs::read_to_string(&board).unwrap(),
            BOARD,
            "a dry run writes nothing"
        );

        let (is_error, body) = call(
            "move_component",
            json!({ "reference": "R1", "x": 120.825, "y": 100.0 }),
        )
        .await;
        assert!(!is_error, "{body:#}");
        not_run(&body);
        let saved = std::fs::read_to_string(&board).unwrap();
        assert!(saved.contains("(at 120.825 100)"), "the move was written");

        for (tool, arguments) in [
            (
                "rotate_component",
                json!({ "reference": "R1", "rotation": 90.0 }),
            ),
            ("set_component_placements", r1_to(110.0, 95.0)),
        ] {
            let (is_error, body) = call(tool, arguments).await;
            assert!(!is_error, "{tool}: {body:#}");
            not_run(&body);
        }
    }

    /// A dry run moving R1 to `(x, 90)` on the four-layer board, which puts
    /// pad 1 at `(x - 0.825, 90)` on the NET_C copper of one spot. Its pads
    /// have copper on F.Cu only, so they are tested there alone, and only
    /// reads reach KiCad. Returns `copper_check`.
    async fn r1_dry_run_on_the_four_layer_board(x: f64) -> Value {
        let kicad = layers_kicad().await;
        let mut arguments = r1_to(x, 90.0);
        arguments["dry_run"] = json!(true);
        let (is_error, body) = kicad.call("set_component_placements", arguments).await;
        assert!(!is_error, "{body:#}");
        let placement = &body["placements"][0];
        assert_eq!(body["count"], 1, "{body:#}");
        assert_eq!(placement["reference"], "R1", "{body:#}");
        assert_eq!(
            placement["current"],
            json!({ "x": 110.0, "y": 95.0, "rotation": 0.0 }),
            "{body:#}"
        );
        // To the nanometre: `mm_to_nm` truncates, so 130.825 mm is planned,
        // and applied, at 130 824 999 nm.
        let target = &placement["target"];
        assert!(
            (target["x"].as_f64().unwrap() - x).abs() * 1e6 < 1.5,
            "{body:#}"
        );
        assert_eq!(target["y"], 90.0, "{body:#}");
        assert_eq!(target["rotation"], 0.0, "{body:#}");
        assert_eq!(
            body["copper_check"]["coverage"],
            json!({
                "pad_count": 2,
                "layers": ["F.Cu"],
                "via_count": 2,
                "track_count": 2,
                "arc_count": 2,
            }),
            "{body:#}"
        );
        kicad.assert_only_reads();
        kicad.assert_untouched();
        body["copper_check"].clone()
    }

    fn assert_clear(check: &Value) {
        assert_eq!(check["status"], "clear", "{check:#}");
        assert_eq!(check["conflicts"], json!([]), "{check:#}");
    }

    /// Spot A: both pads, at (130, 90) and (131.65, 90), lie over the NET_C
    /// track (128, 90)-(134, 90), which is on B.Cu.
    #[tokio::test]
    async fn a_pad_over_a_track_on_another_layer_is_clear() {
        assert_clear(&r1_dry_run_on_the_four_layer_board(130.825).await);
    }

    /// Spot B: pad 1 lands on the blind via at (140, 90), which spans In2.Cu
    /// to B.Cu. KiCad reports it not present on F.Cu.
    #[tokio::test]
    async fn a_pad_over_a_blind_via_absent_from_its_layer_is_clear() {
        assert_clear(&r1_dry_run_on_the_four_layer_board(140.825).await);
    }

    /// Spot C: pad 1 lands on the blind via at (146, 90), which spans F.Cu
    /// to In1.Cu.
    #[tokio::test]
    async fn a_pad_over_a_blind_via_on_its_layer_conflicts() {
        let check = r1_dry_run_on_the_four_layer_board(146.825).await;
        assert_eq!(check["status"], "conflicts", "{check:#}");
        assert_eq!(
            check["conflicts"],
            json!([conflict("R1", "1", "NET_A", "via", VIA_F_TO_IN1_CU)]),
            "{check:#}"
        );
    }

    /// Spot D: the F.Cu arc (153.5, 91)-(155, 90)-(156.5, 91) peaks at
    /// (155, 90), inside pad 1 (155 ± 0.4, 90 ± 0.475), while its chord
    /// along y = 91 passes 0.4 mm below the pad: only the curve reaches it.
    /// Pad 2 clears the arc by about 31 µm at its rounded corner.
    #[tokio::test]
    async fn a_pad_over_an_arc_on_its_layer_conflicts() {
        let check = r1_dry_run_on_the_four_layer_board(155.825).await;
        assert_eq!(check["status"], "conflicts", "{check:#}");
        assert_eq!(
            check["conflicts"],
            json!([conflict("R1", "1", "NET_A", "arc", F_CU_ARC)]),
            "{check:#}"
        );
    }

    /// Spot E: the same arc on B.Cu, at (163.5, 91)-(165, 90)-(166.5, 91).
    #[tokio::test]
    async fn a_pad_over_an_arc_on_another_layer_is_clear() {
        assert_clear(&r1_dry_run_on_the_four_layer_board(165.825).await);
    }

    /// Spot A applied: a move whose only copper is on another layer goes
    /// through.
    #[tokio::test]
    async fn a_clear_move_over_copper_on_another_layer_applies() {
        let kicad = layers_kicad().await;
        let (is_error, body) = kicad
            .call("set_component_placements", r1_to(130.825, 90.0))
            .await;
        assert!(!is_error, "{body:#}");
        assert_eq!(kicad.store.lock().unwrap().updates, 1);
        let (x, y, rotation) = kicad.held("R1");
        assert!(
            (x - 130_825_000).abs() <= 1 && (y, rotation) == (90_000_000, 0.0),
            "R1 held at {:?}",
            (x, y, rotation)
        );
        assert_eq!(body["copper_check"]["status"], "clear", "{body:#}");
        assert_eq!(body["copper_check"]["conflicts"], json!([]), "{body:#}");
        assert!(body.get("warning").is_none(), "{body:#}");
    }

    /// Spot F: R3, held at 90°, moved to (175, 90) stays at 90°, so pad 2
    /// lands at (175 ± 0.475, 89.175 ± 0.4) on the NET_C track along
    /// y = 88.9 (± 0.125). At 0° both pads would clear it.
    #[tokio::test]
    async fn move_component_checks_the_rotation_kicad_holds() {
        let kicad = layers_kicad().await;
        let expected = [conflict("R3", "2", "NET_F", "track", F_CU_TRACK)];
        let (is_error, body) = kicad
            .call(
                "move_component",
                json!({ "reference": "R3", "x": 175.0, "y": 90.0, "dry_run": true }),
            )
            .await;
        assert!(!is_error, "{body:#}");
        assert_eq!(
            body["placements"],
            json!([{
                "reference": "R3",
                "current": { "x": 110.0, "y": 105.0, "rotation": 90.0 },
                "target": { "x": 175.0, "y": 90.0, "rotation": 90.0 },
            }]),
            "{body:#}"
        );
        assert_eq!(
            body["copper_check"]["conflicts"],
            json!(expected),
            "{body:#}"
        );

        let (is_error, body) = kicad
            .call(
                "move_component",
                json!({ "reference": "R3", "x": 175.0, "y": 90.0 }),
            )
            .await;
        assert!(is_error, "{body:#}");
        assert_refused(&body, "move_component", &expected);
        kicad.assert_untouched();
        assert_eq!(kicad.held("R3"), (110_000_000, 105_000_000, 90.0));
    }

    /// Two footprints are R4. A batch refuses a reference the board holds
    /// twice in the check, before any commit, as Copperpour's own refusal
    /// naming the placement's reference (#240). A single move or rotation
    /// takes the first, as `move_footprint` does, so only the batch refuses.
    #[tokio::test]
    async fn a_batch_naming_a_reference_held_twice_is_refused_as_copperpours_own() {
        let kicad = layers_kicad().await;
        let (is_error, body) = kicad
            .call(
                "set_component_placements",
                json!({
                    "placements": [
                        { "reference": "R1", "x": 110.0, "y": 95.0, "rotation": 0.0 },
                        { "reference": "R4", "x": 130.0, "y": 110.0, "rotation": 0.0 },
                    ],
                }),
            )
            .await;
        assert!(is_error, "{body:#}");
        let reason = "footprint reference 'R4' appears more than once on the board";
        kicad.assert_own_refusal(
            &body,
            "component placement batch",
            reason,
            invalid_argument("placements[1].reference", reason),
            &[],
        );
        kicad.assert_only_reads();
    }

    /// Two footprints are R4. A batch refuses a reference the board holds
    /// twice, and so does its dry run, rather than checking one of them.
    #[tokio::test]
    async fn a_dry_run_naming_a_reference_held_twice_is_refused() {
        let kicad = layers_kicad().await;
        let (is_error, body) = kicad
            .call(
                "set_component_placements",
                json!({
                    "placements": [{ "reference": "R4", "x": 130.0, "y": 110.0, "rotation": 0.0 }],
                    "dry_run": true,
                }),
            )
            .await;
        assert!(is_error, "{body:#}");
        assert!(
            body.to_string()
                .contains("footprint reference 'R4' appears more than once on the board"),
            "{body:#}"
        );
        kicad.assert_only_reads();
        kicad.assert_untouched();
    }

    // ─── Refusals Copperpour makes itself inside a live placement (#240) ────

    /// The #240 board (`issue_240_ipc.README.md`): Y1, the stock crystal with
    /// one added rounded rectangle on F.Fab; U1, the stock Honeywell_ABPDAN
    /// and its two textboxes; U2, the stock DIP-8 and its reference point,
    /// which moves with it since #270. No copper, so every check is clear.
    static ISSUE_240: Capture = Capture {
        board_name: "issue240.kicad_pcb",
        board: include_str!("../../tests/fixtures/issue_240.kicad_pcb"),
        footprints: include_bytes!("../../tests/fixtures/issue_240_footprints.ipc.bin"),
        tracks: include_bytes!("../../tests/fixtures/issue_240_tracks.ipc.bin"),
        vias: include_bytes!("../../tests/fixtures/issue_240_vias.ipc.bin"),
        arcs: include_bytes!("../../tests/fixtures/issue_240_arcs.ipc.bin"),
        enabled_layers: include_bytes!("../../tests/fixtures/issue_240_enabled_layers.ipc.bin"),
        presence: include_bytes!("../../tests/fixtures/issue_240_presence.ipc.bin"),
        outlines: &[
            (
                BoardLayer::BlFCu,
                include_bytes!("../../tests/fixtures/issue_240_polygons_F_Cu.ipc.bin"),
            ),
            (
                BoardLayer::BlBCu,
                include_bytes!("../../tests/fixtures/issue_240_polygons_B_Cu.ipc.bin"),
            ),
        ],
    };

    async fn issue_240_kicad() -> Kicad {
        kicad_holding(&ISSUE_240, |command| replay_from(&ISSUE_240, command)).await
    }

    /// The #270 board (`issue_270_unknown_ipc.README.md`): J1, the stock
    /// 1×4 pin header with one added QR barcode, a type the transform does
    /// not carry. No copper, so every check is clear.
    static ISSUE_270_UNKNOWN: Capture = Capture {
        board_name: "issue270_unknown.kicad_pcb",
        board: include_str!("../../tests/fixtures/issue_270_unknown.kicad_pcb"),
        footprints: include_bytes!("../../tests/fixtures/issue_270_unknown_footprints.ipc.bin"),
        tracks: include_bytes!("../../tests/fixtures/issue_270_unknown_tracks.ipc.bin"),
        vias: include_bytes!("../../tests/fixtures/issue_270_unknown_vias.ipc.bin"),
        arcs: include_bytes!("../../tests/fixtures/issue_270_unknown_arcs.ipc.bin"),
        enabled_layers: include_bytes!(
            "../../tests/fixtures/issue_270_unknown_enabled_layers.ipc.bin"
        ),
        presence: include_bytes!("../../tests/fixtures/issue_270_unknown_presence.ipc.bin"),
        outlines: &[
            (
                BoardLayer::BlFCu,
                include_bytes!("../../tests/fixtures/issue_270_unknown_polygons_F_Cu.ipc.bin"),
            ),
            (
                BoardLayer::BlBCu,
                include_bytes!("../../tests/fixtures/issue_270_unknown_polygons_B_Cu.ipc.bin"),
            ),
        ],
    };

    async fn issue_270_unknown_kicad() -> Kicad {
        kicad_holding(&ISSUE_270_UNKNOWN, |command| {
            replay_from(&ISSUE_270_UNKNOWN, command)
        })
        .await
    }

    // The refusals, written out rather than read from the code under test.
    const ROUNDED_RECTANGLE: &str = "cannot rotate footprint 'Y1' by a non-90° angle: its \
         rounded-rectangle graphic on F.Fab has no off-axis form; use a multiple of 90°";
    const TEXTBOX: &str =
        "cannot rotate footprint 'U1' by a non-90° angle: it contains a textbox; use a multiple \
         of 90°";
    const BARCODE: &str = "footprint 'J1' contains an item of unsupported type \
         'kiapi.board.types.Barcode'; refusing to move/rotate it via IPC to avoid detaching it";

    /// The commit commands of a write refused inside its commit: the commit
    /// is dropped, never pushed.
    const DROPPED: &[&str] = &["BeginCommit", "EndCommit(drop)"];

    fn invalid_argument(field: &str, reason: &str) -> Value {
        json!({ "kind": "invalid_argument", "field": field, "reason": reason })
    }

    fn unsupported(tool: &str) -> Value {
        json!({ "kind": "unsupported_capability", "capability": tool, "kicad_version": null })
    }

    /// The KIID of `reference`'s first pad in a recorded `GetItemsResponse`.
    fn first_pad_kiid(footprints: &[u8], reference: &str) -> String {
        kiapi::common::commands::GetItemsResponse::decode(footprints)
            .unwrap()
            .items
            .iter()
            .map(|item| {
                kiapi::board::types::FootprintInstance::decode(item.value.as_slice()).unwrap()
            })
            .find(|footprint| super::footprint_reference(footprint) == reference)
            .unwrap()
            .definition
            .unwrap()
            .items
            .iter()
            .find(|item| any_type_name(item) == "kiapi.board.types.Pad")
            .map(|item| kiapi::board::types::Pad::decode(item.value.as_slice()).unwrap())
            .and_then(|pad| pad.id)
            .unwrap()
            .value
    }

    impl Kicad {
        /// The commit commands KiCad received, in order.
        fn commits(&self) -> Vec<String> {
            self.sent
                .lock()
                .unwrap()
                .iter()
                .filter(|command| command.contains("Commit"))
                .cloned()
                .collect()
        }

        /// Asserts a refusal Copperpour made itself: worded as its own and
        /// never as KiCad's, with `error` exactly, the board file as written,
        /// no command that adds, changes or deletes an item, and `commits` as
        /// the commit commands KiCad received.
        fn assert_own_refusal(
            &self,
            body: &Value,
            what: &str,
            reason: &str,
            error: Value,
            commits: &[&str],
        ) {
            assert_eq!(
                body["message"],
                format!("The {what} was not written: {reason}. The board file was not modified."),
                "{body:#}"
            );
            let text = body.to_string();
            assert!(
                !text.contains("KiCAD rejected") && !text.contains("may hold this board open"),
                "{text}"
            );
            assert_eq!(body["error"], error, "{body:#}");
            assert_eq!(
                std::fs::read_to_string(&self.board).unwrap(),
                self.board_text
            );
            {
                let store = self.store.lock().unwrap();
                assert_eq!((store.creates, store.updates, store.deletes), (0, 0, 0));
            }
            for mutation in ["CreateItems", "UpdateItems", "DeleteItems"] {
                assert!(!self.sent(mutation), "{mutation} reached KiCad");
            }
            assert_eq!(self.commits(), commits, "{:?}", self.sent.lock().unwrap());
        }

        /// Where KiCad holds `reference`'s one reference point, in nm.
        fn held_point(&self, reference: &str) -> (i64, i64) {
            let store = self.store.lock().unwrap();
            let footprint = store
                .items
                .iter()
                .map(|item| {
                    kiapi::board::types::FootprintInstance::decode(item.value.as_slice()).unwrap()
                })
                .find(|footprint| super::footprint_reference(footprint) == reference)
                .unwrap();
            let points: Vec<_> = footprint
                .definition
                .unwrap()
                .items
                .iter()
                .filter(|item| any_type_name(item) == "kiapi.board.types.ReferencePoint")
                .map(|item| {
                    kiapi::board::types::ReferencePoint::decode(item.value.as_slice()).unwrap()
                })
                .collect();
            assert_eq!(points.len(), 1, "{reference}: {points:?}");
            let position = points[0].position.unwrap();
            (position.x_nm, position.y_nm)
        }
    }

    /// #240's own case: a non-90° rotation of a footprint holding a rounded
    /// rectangle. `rotate_component` refuses it before any commit opens; the
    /// batch refuses it inside its commit, which it drops.
    #[tokio::test]
    async fn a_rounded_rectangle_refuses_an_off_axis_rotation_as_copperpours_own() {
        let kicad = issue_240_kicad().await;
        let (is_error, body) = kicad
            .call(
                "rotate_component",
                json!({ "reference": "Y1", "rotation": -45.0 }),
            )
            .await;
        assert!(is_error, "{body:#}");
        kicad.assert_own_refusal(
            &body,
            "rotation",
            ROUNDED_RECTANGLE,
            invalid_argument("rotation", ROUNDED_RECTANGLE),
            &[],
        );

        let kicad = issue_240_kicad().await;
        let (is_error, body) = kicad
            .call(
                "set_component_placements",
                json!({
                    "placements": [
                        { "reference": "U1", "x": 125.0, "y": 95.0, "rotation": 0.0 },
                        { "reference": "Y1", "x": 110.0, "y": 95.0, "rotation": -45.0 },
                    ],
                }),
            )
            .await;
        assert!(is_error, "{body:#}");
        kicad.assert_own_refusal(
            &body,
            "component placement batch",
            ROUNDED_RECTANGLE,
            invalid_argument("placements[1].rotation", ROUNDED_RECTANGLE),
            DROPPED,
        );
    }

    /// A textbox refuses an off-axis rotation the same way: before any
    /// commit for `rotate_component`, inside the batch's dropped commit.
    #[tokio::test]
    async fn a_textbox_refuses_an_off_axis_rotation_as_copperpours_own() {
        let kicad = issue_240_kicad().await;
        let (is_error, body) = kicad
            .call(
                "rotate_component",
                json!({ "reference": "U1", "rotation": 30.0 }),
            )
            .await;
        assert!(is_error, "{body:#}");
        kicad.assert_own_refusal(
            &body,
            "rotation",
            TEXTBOX,
            invalid_argument("rotation", TEXTBOX),
            &[],
        );

        let kicad = issue_240_kicad().await;
        let (is_error, body) = kicad
            .call(
                "set_component_placements",
                json!({ "placements": [{ "reference": "U1", "x": 125.0, "y": 95.0, "rotation": 30.0 }] }),
            )
            .await;
        assert!(is_error, "{body:#}");
        kicad.assert_own_refusal(
            &body,
            "component placement batch",
            TEXTBOX,
            invalid_argument("placements[0].rotation", TEXTBOX),
            DROPPED,
        );
    }

    /// An item Copperpour cannot carry along, here J1's barcode, refuses
    /// every move of its footprint, of any angle: `move_component` and `rotate_component`
    /// before any commit, the batch and `align_components` inside their
    /// commits, which they drop. The footprint is a valid argument, so the
    /// refusal is `unsupported_capability`, naming the tool.
    #[tokio::test]
    async fn an_unsupported_item_refuses_every_move_as_copperpours_own() {
        for (tool, what, arguments, commits) in [
            (
                "move_component",
                "move",
                json!({ "reference": "J1", "x": 115.0, "y": 95.0 }),
                &[][..],
            ),
            (
                "rotate_component",
                "rotation",
                json!({ "reference": "J1", "rotation": 90.0 }),
                &[][..],
            ),
            (
                "set_component_placements",
                "component placement batch",
                json!({ "placements": [{ "reference": "J1", "x": 115.0, "y": 95.0, "rotation": 0.0 }] }),
                DROPPED,
            ),
            (
                "align_components",
                "alignment",
                json!({ "references": ["J1"], "axis": "x", "value": 115.0 }),
                DROPPED,
            ),
        ] {
            let kicad = issue_270_unknown_kicad().await;
            let (is_error, body) = kicad.call(tool, arguments).await;
            assert!(is_error, "{tool}: {body:#}");
            kicad.assert_own_refusal(&body, what, BARCODE, unsupported(tool), commits);
        }
    }

    /// U2's reference point, which KiCad 10.0.8 sent (3.81, 3.81) mm from
    /// its anchor at (140, 95) mm, moves and turns with it in the
    /// `UpdateItems` each placement tool commits (#270). KiCad re-creates
    /// the point where that message says, so one left behind would detach
    /// from its package. The expected poses are worked out by hand: a
    /// rotation of 90° turns (+3.81, +3.81) about the anchor to
    /// (+3.81, −3.81), KiCad's counterclockwise with Y down.
    #[tokio::test]
    async fn a_reference_point_follows_its_footprint() {
        for (tool, arguments, anchor, point) in [
            (
                "move_component",
                json!({ "reference": "U2", "x": 145.0, "y": 95.0 }),
                (145_000_000, 95_000_000, 0.0),
                (148_810_000, 98_810_000),
            ),
            (
                "rotate_component",
                json!({ "reference": "U2", "rotation": 90.0 }),
                (140_000_000, 95_000_000, 90.0),
                (143_810_000, 91_190_000),
            ),
            (
                "set_component_placements",
                json!({ "placements": [{ "reference": "U2", "x": 145.0, "y": 95.0, "rotation": 90.0 }] }),
                (145_000_000, 95_000_000, 90.0),
                (148_810_000, 91_190_000),
            ),
            (
                "align_components",
                json!({ "references": ["U2"], "axis": "x", "value": 145.0 }),
                (145_000_000, 95_000_000, 0.0),
                (148_810_000, 98_810_000),
            ),
        ] {
            let kicad = issue_240_kicad().await;
            assert_eq!(kicad.held_point("U2"), (143_810_000, 98_810_000));
            let (is_error, body) = kicad.call(tool, arguments).await;
            assert!(!is_error, "{tool}: {body:#}");
            assert_eq!(kicad.held("U2"), anchor, "{tool}");
            assert_eq!(kicad.held_point("U2"), point, "{tool}");
            assert_eq!(kicad.store.lock().unwrap().updates, 1, "{tool}");
            assert!(!kicad.sent("EndCommit(drop)"), "{tool}");
        }
    }

    /// A reference no footprint carries: the three placement tools refuse
    /// it in the check, before any commit, and `align_components` inside its
    /// commit, which it drops. Each names its own argument.
    #[tokio::test]
    async fn a_missing_reference_is_refused_as_copperpours_own() {
        let in_check = "footprint R9 not found on board";
        let in_align = "footprint 'R9' not found";
        for (tool, what, arguments, reason, field, commits) in [
            (
                "move_component",
                "move",
                json!({ "reference": "R9", "x": 110.0, "y": 90.0 }),
                in_check,
                "reference",
                &[][..],
            ),
            (
                "rotate_component",
                "rotation",
                json!({ "reference": "R9", "rotation": 90.0 }),
                in_check,
                "reference",
                &[][..],
            ),
            (
                "set_component_placements",
                "component placement batch",
                json!({
                    "placements": [
                        { "reference": "R1", "x": 110.0, "y": 95.0, "rotation": 0.0 },
                        { "reference": "R9", "x": 110.0, "y": 90.0, "rotation": 0.0 },
                    ],
                }),
                in_check,
                "placements[1].reference",
                &[][..],
            ),
            (
                "align_components",
                "alignment",
                json!({ "references": ["R9"], "axis": "x", "value": 110.0 }),
                in_align,
                "references[0]",
                DROPPED,
            ),
        ] {
            let kicad = kicad().await;
            let (is_error, body) = kicad.call(tool, arguments).await;
            assert!(is_error, "{tool}: {body:#}");
            kicad.assert_own_refusal(
                &body,
                what,
                reason,
                invalid_argument(field, reason),
                commits,
            );
        }
    }

    /// The control: KiCad itself refusing the batch's `UpdateItems` with an
    /// error status is still KiCad's rejection, worded as before and with no
    /// error kind, and the commit is dropped.
    #[tokio::test]
    async fn kicads_own_rejection_keeps_its_wording() {
        use crate::tools::pcb_board::board_mock::board_document;
        use kiapi::common::{ApiResponse, ApiResponseStatus, ApiStatusCode};

        let dir = tempfile::tempdir().unwrap();
        let board = dir.path().join(ISSUE_96.board_name);
        std::fs::write(&board, ISSUE_96.board).unwrap();
        let store = Arc::new(Mutex::new(ItemStore {
            items: kiapi::common::commands::GetItemsResponse::decode(FOOTPRINTS)
                .unwrap()
                .items,
            ..Default::default()
        }));
        let sent = Arc::new(Mutex::new(Vec::new()));
        let (log, held) = (sent.clone(), store.clone());
        let documents = vec![board_document(&board.to_string_lossy())];
        let server = MockIpcServer::spawn("kicad-refusing-updates", move |request| {
            let command = request.message.expect("a command");
            let answer = |status: ApiStatusCode, error: &str, message| ApiResponse {
                status: Some(ApiResponseStatus {
                    status: status as i32,
                    error_message: error.to_string(),
                }),
                header: None,
                message,
            };
            let name = command_name(&command);
            let message = match name.as_str() {
                "GetOpenDocuments" => {
                    return answer(
                        ApiStatusCode::AsOk,
                        "",
                        Some(pack_any(
                            &kiapi::common::commands::GetOpenDocumentsResponse {
                                documents: documents.clone(),
                            },
                            "kiapi.common.commands.GetOpenDocumentsResponse",
                        )),
                    )
                }
                "UpdateItems" => {
                    log.lock().unwrap().push(name);
                    return answer(
                        ApiStatusCode::AsBadRequest,
                        "mock KiCad refuses this update",
                        None,
                    );
                }
                "BeginCommit" => Some(pack_any(
                    &kiapi::common::commands::BeginCommitResponse {
                        id: Some(kiapi::common::types::Kiid {
                            value: "mock-commit".to_string(),
                        }),
                    },
                    "kiapi.common.commands.BeginCommitResponse",
                )),
                end if end.starts_with("EndCommit") => Some(pack_any(
                    &kiapi::common::commands::EndCommitResponse {},
                    "kiapi.common.commands.EndCommitResponse",
                )),
                _ => replay(&command).or_else(|| {
                    // The footprints, the one class `replay` leaves to the store.
                    Some(pack_any(
                        &kiapi::common::commands::GetItemsResponse {
                            header: None,
                            status: kiapi::common::types::ItemRequestStatus::IrsOk as i32,
                            items: held.lock().unwrap().items.clone(),
                        },
                        "kiapi.common.commands.GetItemsResponse",
                    ))
                    .filter(|_| name == "GetItems")
                }),
            };
            log.lock().unwrap().push(name);
            answer(ApiStatusCode::AsOk, "", message)
        });
        let handler = McpHandler::new(crate::tools::ServerConfig {
            ipc_address: server.address().to_string(),
            eager_toolsets: true,
            ..Default::default()
        })
        .await
        .unwrap();
        let kicad = Kicad {
            _dir: dir,
            board,
            board_text: ISSUE_96.board,
            store,
            sent,
            handler,
            _server: server,
        };

        let (is_error, body) = kicad
            .call("set_component_placements", r1_to(110.0, 90.0))
            .await;
        assert!(is_error, "{body:#}");
        let text = body.as_str().expect("a plain-text error, with no kind");
        assert_eq!(
            text,
            "KiCAD rejected the component placement batch over IPC: KiCad batch failed; changes \
             dropped: KiCad IPC error: mock KiCad refuses this update (AS_BAD_REQUEST). The board \
             file was not modified — KiCAD is reachable and may hold this board open, so editing \
             the file directly could be silently overwritten."
        );
        assert_eq!(kicad.commits(), DROPPED, "{:?}", kicad.sent.lock().unwrap());
        assert!(kicad.sent("UpdateItems"));
        assert_eq!(std::fs::read_to_string(&kicad.board).unwrap(), BOARD);
    }
}
