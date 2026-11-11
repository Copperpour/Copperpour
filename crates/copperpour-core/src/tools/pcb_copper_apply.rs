//! `apply_copper`: create a batch of tracks and vias on the live board
//! in one KiCad commit (#95).
//!
//! The plan is checked before anything is written: every length rounded to the
//! nanometre as KiCad's board reader rounds, each layer a copper layer the
//! board has, each net one the board has (never net 0, no net), no size of zero
//! or less, no drill as large as its via, no via span that is not its type's,
//! no zero-length track. A dry run, the
//! default, returns that normalized plan and a `plan_revision` bound to it, to
//! the document KiCad named for the board, and to the board's net table and
//! enabled layers, so a renamed net makes the revision stale. An apply needs
//! the revision.
//!
//! An apply is one commit plus a read-back, not an atomic transaction. KiCad
//! answers `CreateItems` per item, and its answer echoes the request: it names
//! the requested net even for an item it then stores on the net of the copper
//! it touches (recorded on KiCad 10.0.8, `tests/fixtures/issue_95_ipc.README.md`).
//! So every created item is read back by KIID after the commit, and the
//! response is built from that read-back. If KiCad stored any item on another
//! net, the whole batch is removed again in a second commit and the call is
//! refused; if KiCad created only some of the items, the commit is dropped.
//! Either way the items are then looked up once more, and a removal that
//! cannot be confirmed is reported as uncertain, naming the KIIDs to inspect.

use crate::hex::encode_hex;
use crate::mcp::error::ToolErrorKind;
use crate::mcp::protocol::CallToolResult;
use crate::tool;
use crate::tools::footprint_graphics::point_schema;
use crate::tools::pcb_board_outline::{mm_text, COORDINATE_LIMIT_NM};
use crate::tools::{
    get_path, invalid_arg, ipc_failure_result, with_bound_board_ipc_classified, BoardAccess,
    BoardBinding, ToolContext, ToolDef,
};
use copperpour_ipc::builders::{self, any_is, nm_to_mm, pack_any};
use copperpour_ipc::client::{IpcItemCreation, IpcOutcomeUnknown, KiCadIpcClient};
use copperpour_ipc::gen::kiapi;
use copperpour_ipc::{IpcEnabledLayers, IpcNet};
use kiapi::board::types::{BoardLayer, ViaType};
use kiapi::common::types::{DocumentSpecifier, Vector2};
use prost::Message;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::collections::{BTreeSet, HashMap, HashSet};
use std::path::Path;

const TRACK: &str = "kiapi.board.types.Track";
const VIA: &str = "kiapi.board.types.Via";

/// What KiCad's Edit > Undo names the batch.
const UNDO_DESCRIPTION: &str = "Apply copper";

/// What it names the second commit that removes a batch KiCad re-netted.
const ROLLBACK_DESCRIPTION: &str = "Remove apply_copper batch stored on other nets";

/// How many faults a refusal spells out before it only counts the rest.
const LISTED_FAULTS: usize = 20;

pub(crate) fn tool() -> ToolDef {
    let mut track = serde_json::Map::new();
    track.insert("start".to_string(), point_schema());
    track.insert("end".to_string(), point_schema());
    track.insert(
        "width".to_string(),
        json!({ "type": "number", "exclusiveMinimum": 0, "description": "Track width in mm" }),
    );
    track.insert(
        "layer".to_string(),
        json!({ "type": "string", "description": "A copper layer the board has: F.Cu, In1.Cu … In30.Cu or B.Cu" }),
    );
    track.insert(
        "net".to_string(),
        json!({ "type": "string", "description": "Name of a net the board has" }),
    );
    let mut via = serde_json::Map::new();
    via.insert("position".to_string(), point_schema());
    via.insert(
        "drill".to_string(),
        json!({ "type": "number", "exclusiveMinimum": 0, "description": "Drill diameter in mm, smaller than size" }),
    );
    via.insert(
        "size".to_string(),
        json!({ "type": "number", "exclusiveMinimum": 0, "description": "Via pad diameter in mm" }),
    );
    via.insert(
        "net".to_string(),
        json!({ "type": "string", "description": "Name of a net the board has" }),
    );
    via.insert(
        "via_type".to_string(),
        json!({
            "type": "string",
            "enum": ["through", "blind", "buried", "micro"],
            "default": "through",
            "description": "through: F.Cu to B.Cu; blind: F.Cu or B.Cu to an inner layer; \
                            buried: two inner layers; micro: any two layers"
        }),
    );
    via.insert(
        "start_layer".to_string(),
        json!({ "type": "string", "description": "First copper layer the drill spans; required unless via_type is through" }),
    );
    via.insert(
        "end_layer".to_string(),
        json!({ "type": "string", "description": "Last copper layer the drill spans; required unless via_type is through" }),
    );

    tool!(
        "apply_copper",
        "Create a batch of straight tracks and vias on the board KiCad holds open, in \
         one KiCad commit (one undo step), without saving. A dry run, the default, checks every \
         item and returns the normalized plan (millimetres rounded to the nanometre, layers and \
         nets resolved) and a plan_revision; apply with dry_run=false and that plan_revision as \
         expected_plan_revision. Refused before anything is written: a net the board does not \
         have (an empty net is refused, never bound to no net), a layer that is not a copper \
         layer of the board, a width, drill or size of zero or less, a drill not smaller than \
         its via, a via span that is not its via_type's, a zero-length track, an empty plan, \
         a missing or stale revision. KiCad \
         answers per item and can store an item on the net of the copper it touches, so this \
         is one commit plus a read-back, not an atomic transaction: every created item is read \
         back by UUID after the commit and the response comes from that read-back. If KiCad \
         stored any item on another net, the whole batch is removed again in a second commit \
         and the call fails with plan_blocked naming each item and the net KiCad chose; if \
         KiCad created only some items, the commit is dropped and the call fails. A removal \
         that cannot be confirmed returns mutation_outcome_uncertain with the UUIDs to inspect. \
         Copper already on the board is not matched or skipped; no arcs.",
        json!({
            "type": "object",
            "properties": {
                "board": { "type": "string", "description": "Path of the .kicad_pcb KiCad holds open" },
                "tracks": {
                    "type": "array",
                    "description": "Straight track segments to create; points {x, y} in mm",
                    "items": {
                        "type": "object",
                        "properties": track,
                        "required": ["start", "end", "width", "layer", "net"]
                    }
                },
                "vias": {
                    "type": "array",
                    "description": "Vias to create; position {x, y} in mm",
                    "items": {
                        "type": "object",
                        "properties": via,
                        "required": ["position", "drill", "size", "net"]
                    }
                },
                "dry_run": {
                    "type": "boolean",
                    "default": true,
                    "description": "Check the plan and return it with its plan_revision without writing"
                },
                "expected_plan_revision": {
                    "type": "string",
                    "description": "Required when dry_run is false: the plan_revision of a current dry run"
                }
            },
            "required": ["board"],
            "additionalProperties": false
        }),
        |args, ctx| async move { handle_apply_copper(args, ctx).await }
    )
    .with_board_access(BoardAccess::LiveOnly)
}

// ─── The request ──────────────────────────────────────────────────────────────

/// A point in KiCad's board unit, the nanometre.
type Point = (i64, i64);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    Track,
    Via,
}

impl Kind {
    fn noun(self) -> &'static str {
        match self {
            Self::Track => "track",
            Self::Via => "via",
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
struct TrackRequest {
    start: Point,
    end: Point,
    width: i64,
    layer: BoardLayer,
    net: String,
}

#[derive(Debug, Clone, PartialEq)]
struct ViaRequest {
    position: Point,
    drill: i64,
    size: i64,
    net: String,
    via_type: ViaType,
    /// The drill's ends as the request gives them; F.Cu and B.Cu for a
    /// through via that gives none.
    start_layer: BoardLayer,
    end_layer: BoardLayer,
}

impl ViaRequest {
    /// The drill's ends, the one nearer F.Cu first, as KiCad orders them:
    /// `BL_F_Cu`, `BL_In1_Cu` … `BL_In30_Cu`, `BL_B_Cu` ascend.
    fn span(&self) -> (BoardLayer, BoardLayer) {
        let (start, end) = (self.start_layer, self.end_layer);
        if (start as i32) <= (end as i32) {
            (start, end)
        } else {
            (end, start)
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq)]
struct Request {
    tracks: Vec<TrackRequest>,
    vias: Vec<ViaRequest>,
}

/// Why the plan cannot be applied, under the argument path that names it, as
/// `tracks[3].width`.
#[derive(Debug, Clone, PartialEq)]
struct Fault {
    field: String,
    reason: String,
}

impl Fault {
    fn new(field: impl Into<String>, reason: impl Into<String>) -> Self {
        Self {
            field: field.into(),
            reason: reason.into(),
        }
    }
}

/// Every fault at once, so a large plan is corrected in one pass. The first
/// one fills the structured `invalid_argument`; the message lists them all.
fn refusal(faults: &[Fault]) -> CallToolResult {
    let first = &faults[0];
    let listed: Vec<String> = faults
        .iter()
        .take(LISTED_FAULTS)
        .map(|fault| format!("{}: {}", fault.field, fault.reason))
        .collect();
    let more = faults.len().saturating_sub(LISTED_FAULTS);
    CallToolResult::error_kind(
        ToolErrorKind::InvalidArgument {
            field: first.field.clone(),
            reason: first.reason.clone(),
        },
        format!(
            "apply_copper refused the plan before writing anything ({} fault{}): {}{}. Nothing \
             was written.",
            faults.len(),
            if faults.len() == 1 { "" } else { "s" },
            listed.join("; "),
            if more > 0 {
                format!("; and {more} more")
            } else {
                String::new()
            }
        ),
    )
}

/// The request as given, checked for everything that does not need the board.
fn parse_request(args: &Value) -> Result<Request, Vec<Fault>> {
    let mut faults = Vec::new();
    let mut request = Request::default();
    for key in ["tracks", "vias"] {
        let items = match &args[key] {
            Value::Null => continue,
            Value::Array(items) => items,
            _ => {
                faults.push(Fault::new(key, "must be an array"));
                continue;
            }
        };
        for (index, item) in items.iter().enumerate() {
            let field = format!("{key}[{index}]");
            if !item.is_object() {
                faults.push(Fault::new(field, "must be an object"));
                continue;
            }
            if key == "tracks" {
                if let Some(track) = parse_track(item, &field, &mut faults) {
                    request.tracks.push(track);
                }
            } else if let Some(via) = parse_via(item, &field, &mut faults) {
                request.vias.push(via);
            }
        }
    }
    if faults.is_empty() && request.tracks.is_empty() && request.vias.is_empty() {
        faults.push(Fault::new(
            "tracks",
            "tracks and vias are both empty, so there is nothing to create",
        ));
    }
    if faults.is_empty() {
        Ok(request)
    } else {
        Err(faults)
    }
}

fn parse_track(value: &Value, field: &str, faults: &mut Vec<Fault>) -> Option<TrackRequest> {
    let before = faults.len();
    let start = point(&value["start"], &format!("{field}.start"), faults);
    let end = point(&value["end"], &format!("{field}.end"), faults);
    let width = length(&value["width"], &format!("{field}.width"), faults);
    let layer = copper_layer(&value["layer"], &format!("{field}.layer"), faults);
    let net = net_name(&value["net"], &format!("{field}.net"), faults);
    if let (Some(start), Some(end)) = (start, end) {
        if start == end {
            faults.push(Fault::new(
                format!("{field}.end"),
                format!(
                    "is the start point {} once rounded to the nanometre: a track of zero length",
                    point_text(start)
                ),
            ));
        }
    }
    if faults.len() != before {
        return None;
    }
    Some(TrackRequest {
        start: start?,
        end: end?,
        width: width?,
        layer: layer?,
        net: net?,
    })
}

fn parse_via(value: &Value, field: &str, faults: &mut Vec<Fault>) -> Option<ViaRequest> {
    let before = faults.len();
    let position = point(&value["position"], &format!("{field}.position"), faults);
    let drill = length(&value["drill"], &format!("{field}.drill"), faults);
    let size = length(&value["size"], &format!("{field}.size"), faults);
    let net = net_name(&value["net"], &format!("{field}.net"), faults);
    let span = via_span(value, field, faults);
    if let (Some(drill), Some(size)) = (drill, size) {
        if drill >= size {
            faults.push(Fault::new(
                format!("{field}.drill"),
                format!(
                    "is {} mm, not smaller than the via size {} mm",
                    mm_text(drill),
                    mm_text(size)
                ),
            ));
        }
    }
    if faults.len() != before {
        return None;
    }
    let (via_type, start_layer, end_layer) = span?;
    Some(ViaRequest {
        position: position?,
        drill: drill?,
        size: size?,
        net: net?,
        via_type,
        start_layer,
        end_layer,
    })
}

/// A via's type and the copper layers its drill spans, as given. A through
/// via spans F.Cu to B.Cu, so its layers may be left out; KiCad resets any
/// other span of one to that (`PCB_VIA::SanitizeLayers`). KiCad tells a blind
/// via from a buried one by its span alone (`PCB_VIA::IsBlindVia`,
/// `IsBuriedVia`): a blind via has exactly one end on F.Cu or B.Cu, a buried
/// via neither. A micro via may join any two layers.
fn via_span(
    value: &Value,
    field: &str,
    faults: &mut Vec<Fault>,
) -> Option<(ViaType, BoardLayer, BoardLayer)> {
    let before = faults.len();
    let type_field = format!("{field}.via_type");
    let via_type = match &value["via_type"] {
        Value::Null => Some(("through", ViaType::VtThrough)),
        given => {
            let known = VIA_TYPES
                .into_iter()
                .find(|(name, _)| given.as_str() == Some(*name));
            if known.is_none() {
                faults.push(Fault::new(
                    &type_field,
                    "must be through, blind, buried or micro",
                ));
            }
            known
        }
    };
    let mut ends = Vec::new();
    for key in ["start_layer", "end_layer"] {
        if !value[key].is_null() {
            ends.push(copper_layer(&value[key], &format!("{field}.{key}"), faults));
        }
    }
    if faults.len() != before {
        return None;
    }
    let (name, via_type) = via_type?;
    let (start, end) = match ends[..] {
        [] if via_type == ViaType::VtThrough => (BoardLayer::BlFCu, BoardLayer::BlBCu),
        [Some(a), Some(b)] if a == b => {
            faults.push(Fault::new(
                format!("{field}.end_layer"),
                format!(
                    "is {}, as start_layer is: a via joins two layers",
                    layer_text(a)
                ),
            ));
            return None;
        }
        [Some(a), Some(b)] => (a, b),
        _ => {
            let missing = if value["start_layer"].is_null() {
                "start_layer"
            } else {
                "end_layer"
            };
            let reason = if via_type == ViaType::VtThrough {
                "a through via takes both start_layer and end_layer or neither".to_string()
            } else {
                format!("a {name} via needs both start_layer and end_layer")
            };
            faults.push(Fault::new(format!("{field}.{missing}"), reason));
            return None;
        }
    };
    let outer = |layer: BoardLayer| matches!(layer, BoardLayer::BlFCu | BoardLayer::BlBCu);
    let rule = match via_type {
        ViaType::VtThrough if !(outer(start) && outer(end)) => "a through via spans F.Cu to B.Cu",
        ViaType::VtBlind if outer(start) == outer(end) => {
            "a blind via has exactly one end on F.Cu or B.Cu"
        }
        ViaType::VtBuried if outer(start) || outer(end) => {
            "a buried via has neither end on F.Cu or B.Cu"
        }
        _ => return Some((via_type, start, end)),
    };
    faults.push(Fault::new(
        type_field,
        format!(
            "is {name}, but {} to {} is not its span: {rule}",
            layer_text(start),
            layer_text(end)
        ),
    ));
    None
}

/// The via types, named as `query_vias` names them.
const VIA_TYPES: [(&str, ViaType); 4] = [
    ("through", ViaType::VtThrough),
    ("blind", ViaType::VtBlind),
    ("buried", ViaType::VtBuried),
    ("micro", ViaType::VtMicro),
];

/// A via type as [`VIA_TYPES`] names it; `None` for any other.
fn via_type_name(via_type: ViaType) -> Option<&'static str> {
    VIA_TYPES
        .iter()
        .find(|(_, known)| *known == via_type)
        .map(|(name, _)| *name)
}

/// Millimetres rounded to the nanometre, as KiCad's board reader rounds
/// (`parseBoardUnits`), within the range a board holds. It rounds and checks
/// the range itself rather than calling `mm_to_nm`, whose cast would turn NaN
/// into 0 before any check saw it (as `parse_point` in `pcb_board_outline.rs`).
fn to_nm(mm: f64) -> Option<i64> {
    let nm = (mm * 1e6).round();
    (nm.is_finite() && nm.abs() <= COORDINATE_LIMIT_NM as f64).then_some(nm as i64)
}

fn beyond_limit(mm: f64) -> String {
    format!(
        "{mm} mm is beyond the ±{} mm KiCad keeps on a board",
        mm_text(COORDINATE_LIMIT_NM)
    )
}

fn point(value: &Value, field: &str, faults: &mut Vec<Fault>) -> Option<Point> {
    if !value.is_object() {
        faults.push(Fault::new(field, "must be a point {x, y} in mm"));
        return None;
    }
    let x = coordinate(&value["x"], &format!("{field}.x"), faults);
    let y = coordinate(&value["y"], &format!("{field}.y"), faults);
    Some((x?, y?))
}

fn coordinate(value: &Value, field: &str, faults: &mut Vec<Fault>) -> Option<i64> {
    let Some(mm) = value.as_f64() else {
        faults.push(Fault::new(field, "must be a number of millimetres"));
        return None;
    };
    let nm = to_nm(mm);
    if nm.is_none() {
        faults.push(Fault::new(field, beyond_limit(mm)));
    }
    nm
}

/// A width, drill or size: at least one nanometre once rounded.
fn length(value: &Value, field: &str, faults: &mut Vec<Fault>) -> Option<i64> {
    let nm = coordinate(value, field, faults)?;
    if nm <= 0 {
        faults.push(Fault::new(
            field,
            format!(
                "must be greater than 0 mm once rounded to the nanometre; got {} mm",
                value.as_f64().unwrap_or_default()
            ),
        ));
        return None;
    }
    Some(nm)
}

/// A copper layer this build can name to KiCad. Whether the board has it is
/// checked against the board in [`resolve`].
fn copper_layer(value: &Value, field: &str, faults: &mut Vec<Fault>) -> Option<BoardLayer> {
    let Some(name) = value.as_str() else {
        faults.push(Fault::new(field, "must be a layer name such as 'F.Cu'"));
        return None;
    };
    match builders::try_layer_from_name(name) {
        Err(error) => {
            faults.push(Fault::new(field, format!("{error:#}")));
            None
        }
        Ok(layer) if !is_copper(layer) => {
            faults.push(Fault::new(
                field,
                format!("'{name}' is not a copper layer: F.Cu, In1.Cu … In30.Cu or B.Cu"),
            ));
            None
        }
        Ok(layer) => Some(layer),
    }
}

/// `BL_F_Cu`, `BL_In1_Cu` … `BL_In30_Cu`, `BL_B_Cu` are contiguous in
/// `board_types.proto` (3 to 34).
fn is_copper(layer: BoardLayer) -> bool {
    (BoardLayer::BlFCu as i32..=BoardLayer::BlBCu as i32).contains(&(layer as i32))
}

fn net_name(value: &Value, field: &str, faults: &mut Vec<Fault>) -> Option<String> {
    match value.as_str() {
        None => {
            faults.push(Fault::new(field, "must be a net name"));
            None
        }
        Some("") => {
            faults.push(Fault::new(
                field,
                "names no net: apply_copper never creates copper without a net (net 0)",
            ));
            None
        }
        Some(net) => Some(net.to_string()),
    }
}

fn point_text((x, y): Point) -> String {
    format!("({}, {})", mm_text(x), mm_text(y))
}

fn point_json((x, y): Point) -> Value {
    json!({ "x": nm_to_mm(x), "y": nm_to_mm(y) })
}

fn layer_text(layer: BoardLayer) -> &'static str {
    builders::layer_name(layer).unwrap_or("an unnamed layer")
}

// ─── The plan against the board ───────────────────────────────────────────────

/// The request resolved against the board KiCad holds: each net's code read
/// from KiCad's net table.
#[derive(Debug, Clone, PartialEq)]
struct Plan {
    tracks: Vec<(TrackRequest, i32)>,
    vias: Vec<(ViaRequest, i32)>,
}

/// Resolve every net and check every layer, collecting all faults.
fn resolve(
    request: &Request,
    nets: &[IpcNet],
    enabled: &IpcEnabledLayers,
) -> Result<Plan, Vec<Fault>> {
    // Net 0 is KiCad's "no net"; it is never a target, whatever its name.
    let codes: HashMap<&str, i32> = nets
        .iter()
        .filter(|net| net.netcode != 0 && !net.name.is_empty())
        .map(|net| (net.name.as_str(), net.netcode))
        .collect();
    let layers: BTreeSet<i32> = enabled.layers.iter().map(|layer| layer.id).collect();
    let mut faults = Vec::new();
    let lookup = |net: &str, field: String, faults: &mut Vec<Fault>| match codes.get(net) {
        Some(code) => Some(*code),
        None => {
            faults.push(Fault::new(field, format!("the board has no net '{net}'")));
            None
        }
    };
    let mut plan = Plan {
        tracks: Vec::with_capacity(request.tracks.len()),
        vias: Vec::with_capacity(request.vias.len()),
    };
    for (index, track) in request.tracks.iter().enumerate() {
        if !layers.contains(&(track.layer as i32)) {
            faults.push(Fault::new(
                format!("tracks[{index}].layer"),
                format!(
                    "the board has no layer {}: its copper layers are {}",
                    layer_text(track.layer),
                    copper_layers_text(&layers)
                ),
            ));
        }
        if let Some(code) = lookup(&track.net, format!("tracks[{index}].net"), &mut faults) {
            plan.tracks.push((track.clone(), code));
        }
    }
    for (index, via) in request.vias.iter().enumerate() {
        for (key, layer) in [
            ("start_layer", via.start_layer),
            ("end_layer", via.end_layer),
        ] {
            if !layers.contains(&(layer as i32)) {
                faults.push(Fault::new(
                    format!("vias[{index}].{key}"),
                    format!(
                        "the board has no layer {}: its copper layers are {}",
                        layer_text(layer),
                        copper_layers_text(&layers)
                    ),
                ));
            }
        }
        if let Some(code) = lookup(&via.net, format!("vias[{index}].net"), &mut faults) {
            plan.vias.push((via.clone(), code));
        }
    }
    if faults.is_empty() {
        Ok(plan)
    } else {
        Err(faults)
    }
}

fn copper_layers_text(layers: &BTreeSet<i32>) -> String {
    layers
        .iter()
        .filter_map(|id| BoardLayer::try_from(*id).ok())
        .filter(|layer| is_copper(*layer))
        .map(layer_text)
        .collect::<Vec<_>>()
        .join(", ")
}

/// What a dry run promises an apply: this plan, on the document KiCad named
/// for the board, with this net table and these enabled layers. A renamed,
/// added or removed net, or a changed layer set, makes it stale.
fn plan_revision(
    document: &DocumentSpecifier,
    nets: &[IpcNet],
    enabled: &IpcEnabledLayers,
    plan: &Plan,
) -> String {
    fn field(digest: &mut Sha256, bytes: &[u8]) {
        digest.update((bytes.len() as u64).to_le_bytes());
        digest.update(bytes);
    }
    let mut digest = Sha256::new();
    field(&mut digest, b"apply_copper/1");
    field(&mut digest, &document.encode_to_vec());
    let mut table: Vec<(&str, i32)> = nets
        .iter()
        .map(|net| (net.name.as_str(), net.netcode))
        .collect();
    table.sort_unstable();
    digest.update((table.len() as u64).to_le_bytes());
    for (name, code) in table {
        field(&mut digest, name.as_bytes());
        digest.update(code.to_le_bytes());
    }
    let layers: BTreeSet<i32> = enabled.layers.iter().map(|layer| layer.id).collect();
    digest.update((layers.len() as u64).to_le_bytes());
    for id in layers {
        digest.update(id.to_le_bytes());
    }
    digest.update(enabled.copper_layer_count.to_le_bytes());
    digest.update((plan.tracks.len() as u64).to_le_bytes());
    for (track, code) in &plan.tracks {
        for value in [
            track.start.0,
            track.start.1,
            track.end.0,
            track.end.1,
            track.width,
        ] {
            digest.update(value.to_le_bytes());
        }
        digest.update((track.layer as i32).to_le_bytes());
        field(&mut digest, track.net.as_bytes());
        digest.update(code.to_le_bytes());
    }
    digest.update((plan.vias.len() as u64).to_le_bytes());
    for (via, code) in &plan.vias {
        for value in [via.position.0, via.position.1, via.drill, via.size] {
            digest.update(value.to_le_bytes());
        }
        let (start, end) = via.span();
        for value in [via.via_type as i32, start as i32, end as i32] {
            digest.update(value.to_le_bytes());
        }
        field(&mut digest, via.net.as_bytes());
        digest.update(code.to_le_bytes());
    }
    format!("sha256:{}", encode_hex(&digest.finalize()))
}

/// The dry run's answer: the plan as it will be sent.
fn plan_json(plan: &Plan, revision: &str) -> Value {
    json!({
        "dry_run": true,
        "plan_revision": revision,
        "track_count": plan.tracks.len(),
        "via_count": plan.vias.len(),
        "tracks": plan.tracks.iter().map(|(track, code)| json!({
            "start": point_json(track.start),
            "end": point_json(track.end),
            "width": nm_to_mm(track.width),
            "layer": layer_text(track.layer),
            "net": track.net,
            "net_code": code,
        })).collect::<Vec<_>>(),
        "vias": plan.vias.iter().map(|(via, code)| json!({
            "position": point_json(via.position),
            "drill": nm_to_mm(via.drill),
            "size": nm_to_mm(via.size),
            "net": via.net,
            "net_code": code,
            "via_type": via_type_name(via.via_type),
            "start_layer": layer_text(via.span().0),
            "end_layer": layer_text(via.span().1),
        })).collect::<Vec<_>>(),
        "source": "ipc",
    })
}

/// One item of the batch as the responses name it.
struct Requested {
    label: String,
    kind: Kind,
    net: String,
    place: String,
}

impl Plan {
    /// The items in the order they are sent: tracks, then vias.
    fn requested(&self) -> Vec<Requested> {
        let tracks = self
            .tracks
            .iter()
            .enumerate()
            .map(|(index, (track, _))| Requested {
                label: format!("tracks[{index}]"),
                kind: Kind::Track,
                net: track.net.clone(),
                place: format!(
                    "{}-{} on {}",
                    point_text(track.start),
                    point_text(track.end),
                    layer_text(track.layer)
                ),
            });
        let vias = self
            .vias
            .iter()
            .enumerate()
            .map(|(index, (via, _))| Requested {
                label: format!("vias[{index}]"),
                kind: Kind::Via,
                net: via.net.clone(),
                place: format!("at {}", point_text(via.position)),
            });
        tracks.chain(vias).collect()
    }

    /// The `CreateItems` payload, in exact nanometres: the builders take
    /// millimetres, so the plan's nanometre geometry is set after them rather
    /// than converted back and forth.
    fn items(&self) -> Vec<prost_types::Any> {
        let vector = |(x_nm, y_nm): Point| Some(Vector2 { x_nm, y_nm });
        let square = |nm: i64| Some(Vector2 { x_nm: nm, y_nm: nm });
        let tracks = self.tracks.iter().map(|(track, code)| {
            let mut item =
                builders::build_track(&track.net, *code, "F.Cu", 0.0, 0.0, 0.0, 0.0, 0.0);
            item.start = vector(track.start);
            item.end = vector(track.end);
            item.width = Some(kiapi::common::types::Distance {
                value_nm: track.width,
            });
            item.layer = track.layer as i32;
            pack_any(&item, TRACK)
        });
        let vias = self.vias.iter().map(|(via, code)| {
            let mut item = builders::build_via(&via.net, *code, 0.0, 0.0, 0.0, 0.0);
            item.position = vector(via.position);
            item.r#type = via.via_type as i32;
            let (start, end) = via.span();
            if let Some(stack) = item.pad_stack.as_mut() {
                // KiCad takes a via's layers from its drill; the stack lists
                // the two ends, as KiCad's own blind vias do.
                stack.layers = vec![start as i32, end as i32];
                if let Some(drill) = stack.drill.as_mut() {
                    drill.diameter = square(via.drill);
                    drill.start_layer = start as i32;
                    drill.end_layer = end as i32;
                }
                for layer in &mut stack.copper_layers {
                    layer.size = square(via.size);
                }
            }
            pack_any(&item, VIA)
        });
        tracks.chain(vias).collect()
    }
}

// ─── What KiCad made of it ────────────────────────────────────────────────────

/// The KIID of an item KiCad handed back, when it is the kind requested.
fn created_kiid(item: &prost_types::Any, kind: Kind) -> Option<String> {
    let id = match kind {
        Kind::Track if any_is(item, TRACK) => {
            kiapi::board::types::Track::decode(item.value.as_slice())
                .ok()?
                .id
        }
        Kind::Via if any_is(item, VIA) => {
            kiapi::board::types::Via::decode(item.value.as_slice())
                .ok()?
                .id
        }
        _ => None,
    };
    id.map(|id| id.value).filter(|value| !value.is_empty())
}

/// A `CreateItems` answer that did not create the batch as planned.
#[derive(Debug, Default)]
struct Unapplied {
    /// KIIDs of the items KiCad did create, staged in the commit.
    staged: Vec<String>,
    /// Items KiCad refused, with its status and reason.
    refused: Vec<String>,
    /// Answers that cannot be read: a created item without a usable KIID.
    unreadable: Vec<String>,
}

/// The KIIDs KiCad assigned, in request order, or what went wrong.
fn created_kiids(
    requested: &[Requested],
    results: &[IpcItemCreation],
) -> Result<Vec<String>, Unapplied> {
    let mut unapplied = Unapplied::default();
    let mut kiids = Vec::with_capacity(results.len());
    let mut seen = HashSet::new();
    for (request, result) in requested.iter().zip(results) {
        if !result.created {
            unapplied.refused.push(format!(
                "{} ({} {}): KiCad refused it with {}{}",
                request.label,
                request.kind.noun(),
                request.place,
                result.status,
                if result.error_message.is_empty() {
                    String::new()
                } else {
                    format!(" ({})", result.error_message)
                }
            ));
            continue;
        }
        match result
            .item
            .as_ref()
            .and_then(|item| created_kiid(item, request.kind))
        {
            Some(kiid) if seen.insert(kiid.clone()) => {
                unapplied.staged.push(kiid.clone());
                kiids.push(kiid);
            }
            Some(kiid) => unapplied.unreadable.push(format!(
                "{}: KiCad gave it the KIID {kiid} it had already given another item",
                request.label
            )),
            None => unapplied.unreadable.push(format!(
                "{}: KiCad reported it created but sent back no {} with a KIID, so it could not \
                 be read back",
                request.label,
                request.kind.noun()
            )),
        }
    }
    if unapplied.refused.is_empty() && unapplied.unreadable.is_empty() {
        Ok(kiids)
    } else {
        Err(unapplied)
    }
}

/// One created item as KiCad holds it after the commit.
struct Stored {
    kiid: String,
    kind: Kind,
    net: String,
    report: Value,
}

fn stored_item(item: &prost_types::Any) -> Option<Stored> {
    if any_is(item, TRACK) {
        let track = kiapi::board::types::Track::decode(item.value.as_slice()).ok()?;
        let kiid = track.id?.value;
        let net = track.net.map(|net| net.name).unwrap_or_default();
        let layer = BoardLayer::try_from(track.layer)
            .ok()
            .and_then(builders::layer_name);
        let point =
            |vector: Option<Vector2>| vector.map_or(Value::Null, |v| point_json((v.x_nm, v.y_nm)));
        let report = json!({
            "uuid": kiid,
            "start": point(track.start),
            "end": point(track.end),
            "width": track.width.map(|width| nm_to_mm(width.value_nm)),
            "layer": layer,
            "net": net,
        });
        Some(Stored {
            kiid,
            kind: Kind::Track,
            net,
            report,
        })
    } else if any_is(item, VIA) {
        let via = kiapi::board::types::Via::decode(item.value.as_slice()).ok()?;
        let kiid = via.id?.value;
        let net = via.net.map(|net| net.name).unwrap_or_default();
        let stack = via.pad_stack.as_ref();
        let drill = stack.and_then(|stack| stack.drill.as_ref());
        let layer = |id: i32| BoardLayer::try_from(id).ok().and_then(builders::layer_name);
        let report = json!({
            "uuid": kiid,
            "position": via.position.map_or(Value::Null, |v| point_json((v.x_nm, v.y_nm))),
            "drill": drill
                .and_then(|drill| drill.diameter)
                .map(|diameter| nm_to_mm(diameter.x_nm)),
            "size": stack
                .and_then(|stack| stack.copper_layers.first())
                .and_then(|layer| layer.size)
                .map(|size| nm_to_mm(size.x_nm)),
            "net": net,
            "via_type": ViaType::try_from(via.r#type).ok().and_then(via_type_name),
            "start_layer": drill.and_then(|drill| layer(drill.start_layer)),
            "end_layer": drill.and_then(|drill| layer(drill.end_layer)),
        });
        Some(Stored {
            kiid,
            kind: Kind::Via,
            net,
            report,
        })
    } else {
        None
    }
}

/// The read-back of every created item, in request order, or why it does
/// not account for the batch.
fn read_back(
    requested: &[Requested],
    kiids: &[String],
    items: &[prost_types::Any],
) -> Result<Vec<Stored>, String> {
    let created: HashSet<&str> = kiids.iter().map(String::as_str).collect();
    let mut by_kiid: HashMap<String, Stored> = HashMap::new();
    for item in items {
        let stored = stored_item(item).ok_or_else(|| {
            format!(
                "KiCad read back an item that is not a readable track or via ({})",
                item.type_url
            )
        })?;
        if !created.contains(stored.kiid.as_str()) {
            return Err(format!(
                "KiCad read back {}, which this call did not create",
                stored.kiid
            ));
        }
        let kiid = stored.kiid.clone();
        if by_kiid.insert(kiid.clone(), stored).is_some() {
            return Err(format!("KiCad read back {kiid} twice"));
        }
    }
    requested
        .iter()
        .zip(kiids)
        .map(|(request, kiid)| {
            let stored = by_kiid.remove(kiid).ok_or_else(|| {
                format!(
                    "{} ({kiid}) is not on the board after the commit, though KiCad reported \
                     creating it",
                    request.label
                )
            })?;
            if stored.kind != request.kind {
                return Err(format!(
                    "{} ({kiid}) reads back as a {}, not a {}",
                    request.label,
                    stored.kind.noun(),
                    request.kind.noun()
                ));
            }
            Ok(stored)
        })
        .collect()
}

/// The KIIDs of the items a lookup still found.
fn found_kiids(items: &[prost_types::Any]) -> Vec<String> {
    items
        .iter()
        .map(|item| {
            stored_item(item).map_or_else(|| format!("an unreadable {}", item.type_url), |s| s.kiid)
        })
        .collect()
}

fn net_text(net: &str) -> String {
    if net.is_empty() {
        "no net".to_string()
    } else {
        format!("net '{net}'")
    }
}

// ─── The refusals ─────────────────────────────────────────────────────────────

fn stale_plan(board_path: &Path) -> CallToolResult {
    CallToolResult::error_kind(
        ToolErrorKind::StaleTarget {
            target: board_path.display().to_string(),
            reason: "expected_plan_revision does not match the current plan".to_string(),
        },
        "The plan revision is stale: the plan, the board's nets or layers, or the board KiCad \
         holds changed since the dry run. Nothing was written; run the dry run again and apply \
         its plan_revision.",
    )
}

/// Nothing from this call remains on the board, and here is why.
fn blocked(summary: &str, reasons: Vec<String>, restored: &str) -> CallToolResult {
    CallToolResult::error_kind(
        ToolErrorKind::PlanBlocked {
            operation: "apply_copper".to_string(),
            reasons: reasons.clone(),
        },
        format!(
            "apply_copper refused: {summary}: {}. {restored}",
            reasons.join("; ")
        ),
    )
}

/// The board may hold some of this call's items: the caller inspects these
/// KIIDs before doing anything else.
fn uncertain(board_path: &Path, reason: String, kiids: &[String]) -> CallToolResult {
    let reason = format!("{reason}; inspect these items: {}", kiids.join(", "));
    CallToolResult::error_kind(
        ToolErrorKind::MutationOutcomeUncertain {
            operation: "apply_copper".to_string(),
            path: board_path.display().to_string(),
            reason: reason.clone(),
        },
        format!(
            "apply_copper could not confirm what the board KiCad holds now contains: {reason}. \
             Do not repeat the call: inspect those UUIDs on the live board (query_traces, \
             query_vias), remove or keep them, then dry-run again."
        ),
    )
}

// ─── The handler ──────────────────────────────────────────────────────────────

async fn handle_apply_copper(args: &Value, ctx: &ToolContext) -> anyhow::Result<CallToolResult> {
    let board_path = get_path(args, "board")?;
    let dry_run = match &args["dry_run"] {
        Value::Null => true,
        Value::Bool(dry_run) => *dry_run,
        _ => return Ok(invalid_arg("dry_run", "must be a boolean")),
    };
    let expected = match &args["expected_plan_revision"] {
        Value::Null => None,
        Value::String(revision) => Some(revision.clone()),
        _ => return Ok(invalid_arg("expected_plan_revision", "must be a string")),
    };
    if !dry_run && expected.is_none() {
        return Ok(CallToolResult::error_kind(
            ToolErrorKind::InvalidArgument {
                field: "expected_plan_revision".to_string(),
                reason: "required when dry_run is false".to_string(),
            },
            "Apply requires the plan_revision returned by a current dry run. Nothing was sent \
             to KiCad.",
        ));
    }
    let request = match parse_request(args) {
        Ok(request) => request,
        Err(faults) => return Ok(refusal(&faults)),
    };

    let live_path = board_path.clone();
    let outcome = with_bound_board_ipc_classified(ctx, &board_path, move |client, document| {
        apply_live(
            client,
            document,
            &live_path,
            &request,
            expected.as_deref().filter(|_| !dry_run),
            dry_run,
        )
    })
    .await?;
    Ok(match outcome {
        Ok(BoardBinding::Bound(result)) => result,
        Ok(BoardBinding::Unserved { message, .. }) => CallToolResult::error(format!(
            "KiCAD must be running with the board loaded (IPC error: {message})"
        )),
        Err(failure) => ipc_failure_result(failure),
    })
}

/// The plan on the board KiCad holds, or, given the revision a dry run
/// returned, the batch in one commit, read back and guarded.
fn apply_live(
    client: &KiCadIpcClient,
    document: DocumentSpecifier,
    board_path: &Path,
    request: &Request,
    expected: Option<&str>,
    dry_run: bool,
) -> anyhow::Result<CallToolResult> {
    let nets = client.get_nets_in(document.clone())?;
    let enabled = client.get_enabled_layers_in(document.clone())?;
    let plan = match resolve(request, &nets, &enabled) {
        Ok(plan) => plan,
        Err(faults) => return Ok(refusal(&faults)),
    };
    let revision = plan_revision(&document, &nets, &enabled, &plan);
    if dry_run {
        return Ok(CallToolResult::json(&plan_json(&plan, &revision)));
    }
    if expected != Some(revision.as_str()) {
        return Ok(stale_plan(board_path));
    }

    let requested = plan.requested();
    let items = plan.items();
    // A create-only batch is invisible until EndCommit, so a CreateItems
    // answer that arrives too late can still be dropped and the board's
    // snapshot compared, instead of leaving an open commit of unknown
    // outcome; the snapshot read is the price.
    let mut unapplied = None;
    let committed = client.run_commit_recovering_in(document.clone(), UNDO_DESCRIPTION, |client| {
        let results = client.create_items_in_reporting(document.clone(), items)?;
        created_kiids(&requested, &results).map_err(|answer| {
            let summary = format!(
                "KiCad did not create the batch as planned ({} refused, {} unreadable)",
                answer.refused.len(),
                answer.unreadable.len()
            );
            unapplied = Some(answer);
            anyhow::anyhow!(summary)
        })
    });
    let kiids = match (committed, unapplied) {
        (Ok(kiids), _) => kiids,
        (Err(error), Some(answer)) => {
            return Ok(dropped(client, document, board_path, error, answer))
        }
        (Err(error), None) => return Err(error),
    };

    let items = match client.get_items_by_id_in(document.clone(), &kiids) {
        Ok(items) => items,
        Err(error) => {
            return Ok(uncertain(
                board_path,
                format!("the batch was committed but could not be read back ({error:#})"),
                &kiids,
            ))
        }
    };
    let stored = match read_back(&requested, &kiids, &items) {
        Ok(stored) => stored,
        Err(reason) => {
            let reason = format!(
                "the batch was committed but its read-back does not account for it: \
                 {reason}"
            );
            return Ok(uncertain(board_path, reason, &kiids));
        }
    };

    let moved: Vec<String> = requested
        .iter()
        .zip(&stored)
        .filter(|(request, stored)| stored.net != request.net)
        .map(|(request, stored)| {
            format!(
                "{} ({} {}, {}): KiCad stored it on {}, not '{}'",
                request.label,
                request.kind.noun(),
                request.place,
                stored.kiid,
                net_text(&stored.net),
                request.net
            )
        })
        .collect();
    if !moved.is_empty() {
        return Ok(remove_batch(client, document, board_path, &kiids, moved));
    }

    let (tracks, vias): (Vec<&Stored>, Vec<&Stored>) =
        stored.iter().partition(|stored| stored.kind == Kind::Track);
    Ok(CallToolResult::json(&json!({
        "dry_run": false,
        "plan_revision": revision,
        "track_count": tracks.len(),
        "via_count": vias.len(),
        "tracks": tracks.iter().map(|stored| &stored.report).collect::<Vec<_>>(),
        "vias": vias.iter().map(|stored| &stored.report).collect::<Vec<_>>(),
        "source": "ipc_readback",
        "undo": format!(
            "One KiCad undo step ('{UNDO_DESCRIPTION}') removes the whole batch. The board \
             was not saved."
        ),
    })))
}

/// KiCad created only part of the batch: the commit was dropped, so check
/// that what it did create is gone.
fn dropped(
    client: &KiCadIpcClient,
    document: DocumentSpecifier,
    board_path: &Path,
    error: anyhow::Error,
    answer: Unapplied,
) -> CallToolResult {
    let mut reasons = answer.refused;
    reasons.extend(answer.unreadable);
    if error.downcast_ref::<IpcOutcomeUnknown>().is_some() {
        return uncertain(
            board_path,
            format!(
                "KiCad did not create the batch as planned ({}), and dropping its commit could \
                 not be confirmed ({error:#})",
                reasons.join("; ")
            ),
            &answer.staged,
        );
    }
    match client.get_items_by_id_in(document, &answer.staged) {
        Ok(left) if left.is_empty() => blocked(
            "KiCad did not create the batch as planned",
            reasons,
            &format!(
                "The commit was dropped, and a lookup of the {} item(s) KiCad did create finds \
                 none of them, so the board holds nothing from this call. Fix or remove those \
                 items, then dry-run again.",
                answer.staged.len()
            ),
        ),
        Ok(left) => uncertain(
            board_path,
            format!(
                "KiCad did not create the batch as planned ({}); its commit was dropped, but a \
                 lookup still finds {} of the {} item(s) it did create",
                reasons.join("; "),
                left.len(),
                answer.staged.len()
            ),
            &found_kiids(&left),
        ),
        Err(lookup) => uncertain(
            board_path,
            format!(
                "KiCad did not create the batch as planned ({}); its commit was dropped, but \
                 whether the items it created are gone could not be read ({lookup:#})",
                reasons.join("; ")
            ),
            &answer.staged,
        ),
    }
}

/// KiCad stored some of the batch on other nets: remove all of it in a second
/// commit, then check it is gone.
fn remove_batch(
    client: &KiCadIpcClient,
    document: DocumentSpecifier,
    board_path: &Path,
    kiids: &[String],
    moved: Vec<String>,
) -> CallToolResult {
    let removal = client.run_commit(ROLLBACK_DESCRIPTION, |client| {
        client.delete_items_in(document.clone(), kiids.to_vec())
    });
    let left = removal.and_then(|()| client.get_items_by_id_in(document, kiids));
    let summary = format!(
        "KiCad stored {} of the {} created items on another net than requested (an end or via \
         touches copper of that net)",
        moved.len(),
        kiids.len()
    );
    match left {
        Ok(left) if left.is_empty() => blocked(
            &summary,
            moved,
            "The whole batch was removed again in a second commit, and a lookup finds none of \
             its items, so the board holds nothing from this call (KiCad's undo history holds \
             both commits). Move those items clear of the other nets' copper, or give them the \
             net KiCad chose, then dry-run again.",
        ),
        Ok(left) => uncertain(
            board_path,
            format!(
                "{summary}: {}. Removing the batch again was answered, but a lookup still finds \
                 {} of its {} items",
                moved.join("; "),
                left.len(),
                kiids.len()
            ),
            &found_kiids(&left),
        ),
        Err(error) => uncertain(
            board_path,
            format!(
                "{summary}: {}. Removing the batch again could not be confirmed ({error:#})",
                moved.join("; ")
            ),
            kiids,
        ),
    }
}

/// The checks that need no KiCad, and the plan revision, against KiCad
/// 10.0.8's recorded net table and layers for the repro board
/// (`tests/fixtures/issue_95_ipc.README.md`).
#[cfg(test)]
mod plan_tests {
    use super::*;
    use copperpour_ipc::IpcLayer;

    const NETS: &[u8] = include_bytes!("../../tests/fixtures/issue_95_nets.ipc.bin");
    const ENABLED_LAYERS: &[u8] =
        include_bytes!("../../tests/fixtures/issue_95_enabled_layers.ipc.bin");
    const READ_BACK: &[u8] = include_bytes!("../../tests/fixtures/issue_95_readback.ipc.bin");
    /// The four-layer board's layers and its two blind vias, F.Cu-In1.Cu at
    /// (146, 90) and In2.Cu-B.Cu at (140, 90), as KiCad 10.0.8 answered for
    /// them (`tests/fixtures/issue_96_layers_ipc.README.md`).
    const FOUR_LAYERS: &[u8] =
        include_bytes!("../../tests/fixtures/issue_96_layers_enabled_layers.ipc.bin");
    const BLIND_VIAS: &[u8] = include_bytes!("../../tests/fixtures/issue_96_layers_vias.ipc.bin");

    /// KiCad's net table, read as `get_nets_in` reads it.
    fn nets() -> Vec<IpcNet> {
        kiapi::board::commands::NetsResponse::decode(NETS)
            .unwrap()
            .nets
            .into_iter()
            .map(|net| IpcNet {
                name: net.name,
                netcode: net.code.map_or(0, |code| code.value),
            })
            .collect()
    }

    /// KiCad's enabled layers, read as `get_enabled_layers_in` reads them.
    fn enabled() -> IpcEnabledLayers {
        enabled_layers(ENABLED_LAYERS)
    }

    fn four_layers() -> IpcEnabledLayers {
        enabled_layers(FOUR_LAYERS)
    }

    fn enabled_layers(recorded: &[u8]) -> IpcEnabledLayers {
        let answer = kiapi::board::commands::BoardEnabledLayersResponse::decode(recorded).unwrap();
        IpcEnabledLayers {
            copper_layer_count: answer.copper_layer_count,
            layers: answer
                .layers
                .iter()
                .map(|&id| IpcLayer {
                    name: String::new(),
                    id,
                    kind: String::new(),
                    display_name: None,
                })
                .collect(),
        }
    }

    fn document(path: &str) -> DocumentSpecifier {
        crate::tools::pcb_board::board_mock::board_document(path)
    }

    fn track(start: (f64, f64), end: (f64, f64), width: f64, layer: &str, net: &str) -> Value {
        json!({
            "start": { "x": start.0, "y": start.1 },
            "end": { "x": end.0, "y": end.1 },
            "width": width, "layer": layer, "net": net
        })
    }

    fn via(at: (f64, f64), drill: f64, size: f64, net: &str) -> Value {
        json!({ "position": { "x": at.0, "y": at.1 }, "drill": drill, "size": size, "net": net })
    }

    /// A via of `via_type` with the layers given, at (146, 90).
    fn spanned(via_type: &str, start: Option<&str>, end: Option<&str>) -> Value {
        let mut item = via((146.0, 90.0), 0.3, 0.6, "NET_C");
        item["via_type"] = json!(via_type);
        if let Some(start) = start {
            item["start_layer"] = json!(start);
        }
        if let Some(end) = end {
            item["end_layer"] = json!(end);
        }
        item
    }

    /// The recorded batch: T1 and T2 as tracks, V1 and V2 as vias.
    fn batch() -> Value {
        json!({
            "tracks": [
                track((130.0, 90.0), (135.0, 90.0), 0.25, "F.Cu", "NET_D"),
                track((95.0, 110.0), (99.175, 110.0), 0.25, "F.Cu", "NET_D"),
            ],
            "vias": [
                via((135.0, 90.0), 0.3, 0.6, "NET_D"),
                via((109.175, 95.0), 0.3, 0.6, "NET_D"),
            ]
        })
    }

    fn fields(faults: &[Fault]) -> Vec<&str> {
        faults.iter().map(|fault| fault.field.as_str()).collect()
    }

    fn plan_of(args: &Value) -> Plan {
        resolve(&parse_request(args).unwrap(), &nets(), &enabled()).unwrap()
    }

    fn revision_of(args: &Value, nets: &[IpcNet], enabled: &IpcEnabledLayers) -> String {
        let plan = resolve(&parse_request(args).unwrap(), nets, enabled).unwrap();
        plan_revision(&document("/boards/issue96.kicad_pcb"), nets, enabled, &plan)
    }

    #[test]
    fn each_fault_is_refused_under_the_field_that_has_it() {
        let ok_track = || track((1.0, 2.0), (3.0, 2.0), 0.25, "F.Cu", "NET_D");
        let ok_via = || via((1.0, 2.0), 0.3, 0.6, "NET_D");
        let with = |mut item: Value, key: &str, value: Value| {
            item[key] = value;
            item
        };
        let cases = [
            (
                json!({ "tracks": [with(ok_track(), "width", json!(0))] }),
                "tracks[0].width",
            ),
            (
                json!({ "tracks": [with(ok_track(), "width", json!(-0.1))] }),
                "tracks[0].width",
            ),
            // Positive, but 0.4 nm rounds to nothing.
            (
                json!({ "tracks": [with(ok_track(), "width", json!(0.0000004))] }),
                "tracks[0].width",
            ),
            (
                json!({ "vias": [with(ok_via(), "drill", json!(0))] }),
                "vias[0].drill",
            ),
            (
                json!({ "vias": [with(ok_via(), "size", json!(-0.6))] }),
                "vias[0].size",
            ),
            (
                json!({ "vias": [with(ok_via(), "drill", json!(0.6))] }),
                "vias[0].drill",
            ),
            (
                json!({ "vias": [with(ok_via(), "drill", json!(0.8))] }),
                "vias[0].drill",
            ),
            (
                json!({ "tracks": [track((1.0, 2.0), (1.0, 2.0), 0.25, "F.Cu", "NET_D")] }),
                "tracks[0].end",
            ),
            // Zero length once both ends are rounded to the nanometre.
            (
                json!({ "tracks": [track((1.0000001, 2.0), (1.0, 2.0), 0.25, "F.Cu", "NET_D")] }),
                "tracks[0].end",
            ),
            (
                json!({ "tracks": [with(ok_track(), "layer", json!("F.Cuu"))] }),
                "tracks[0].layer",
            ),
            (
                json!({ "tracks": [with(ok_track(), "layer", json!("F.SilkS"))] }),
                "tracks[0].layer",
            ),
            (
                json!({ "tracks": [with(ok_track(), "layer", json!("Edge.Cuts"))] }),
                "tracks[0].layer",
            ),
            (
                json!({ "tracks": [with(ok_track(), "net", json!(""))] }),
                "tracks[0].net",
            ),
            (
                json!({ "vias": [with(ok_via(), "net", json!(""))] }),
                "vias[0].net",
            ),
            (
                json!({ "tracks": [track((3000.0, 2.0), (3.0, 2.0), 0.25, "F.Cu", "NET_D")] }),
                "tracks[0].start.x",
            ),
            (
                json!({ "vias": [with(ok_via(), "position", json!(5))] }),
                "vias[0].position",
            ),
            (json!({ "vias": [ok_via(), 7] }), "vias[1]"),
        ];
        for (args, field) in cases {
            let faults = parse_request(&args).expect_err(&args.to_string());
            assert_eq!(fields(&faults), [field], "{args}");
        }
        // A good item next to them is not refused.
        assert!(parse_request(&json!({ "tracks": [ok_track()], "vias": [ok_via()] })).is_ok());
    }

    #[test]
    fn a_layer_name_is_checked_before_its_kind() {
        let reason = |layer: &str| {
            let args = json!({ "tracks": [track((1.0, 2.0), (3.0, 2.0), 0.25, layer, "NET_D")] });
            parse_request(&args).unwrap_err()[0].reason.clone()
        };
        // A name KiCad has no layer for never reaches KiCad as BL_UNDEFINED.
        assert!(
            reason("F.Cuu").contains("no KiCAD board layer"),
            "{}",
            reason("F.Cuu")
        );
        assert!(
            reason("F.SilkS").contains("not a copper layer"),
            "{}",
            reason("F.SilkS")
        );
    }

    #[test]
    fn an_empty_plan_is_refused() {
        for args in [
            json!({}),
            json!({ "tracks": [], "vias": [] }),
            json!({ "vias": null }),
        ] {
            assert_eq!(
                fields(&parse_request(&args).unwrap_err()),
                ["tracks"],
                "{args}"
            );
        }
    }

    #[test]
    fn every_fault_is_reported_at_once() {
        let faults = parse_request(&json!({
            "tracks": [
                track((1.0, 2.0), (3.0, 2.0), 0.0, "F.Cu", "NET_D"),
                track((1.0, 2.0), (3.0, 2.0), 0.25, "Top", "NET_D"),
            ],
            "vias": [via((1.0, 2.0), 0.6, 0.3, "NET_D")]
        }))
        .unwrap_err();
        assert_eq!(
            fields(&faults),
            ["tracks[0].width", "tracks[1].layer", "vias[0].drill"]
        );
        let result = refusal(&faults);
        assert_eq!(
            crate::mcp::error::extract_error_kind(&result).as_deref(),
            Some("invalid_argument")
        );
        let text = format!("{:?}", result.content);
        for field in fields(&faults) {
            assert!(text.contains(field), "{field} missing from {text}");
        }
    }

    #[test]
    fn lengths_are_rounded_to_the_nanometre_not_truncated() {
        let plan = plan_of(&json!({
            "tracks": [track((264.65, 99.175), (99.1750000004, 0.0), 0.2499999996, "B.Cu", "NET_A")],
            "vias": [via((-0.0000006, 10.0), 0.30000000001, 0.6, "NET_B")]
        }));
        let (track, code) = &plan.tracks[0];
        assert_eq!(track.start, (264_650_000, 99_175_000));
        assert_eq!(track.end, (99_175_000, 0));
        assert_eq!(track.width, 250_000);
        assert_eq!(track.layer, BoardLayer::BlBCu);
        assert_eq!(*code, 1);
        let (via, code) = &plan.vias[0];
        assert_eq!(via.position, (-1, 10_000_000));
        assert_eq!((via.drill, via.size), (300_000, 600_000));
        assert_eq!(*code, 2);

        // The items sent to KiCad carry those nanometres, not the builders'.
        let items = plan.items();
        let sent = kiapi::board::types::Track::decode(items[0].value.as_slice()).unwrap();
        assert_eq!(
            (sent.start, sent.end),
            (
                Some(Vector2 {
                    x_nm: 264_650_000,
                    y_nm: 99_175_000
                }),
                Some(Vector2 {
                    x_nm: 99_175_000,
                    y_nm: 0
                })
            )
        );
        assert_eq!(sent.width.unwrap().value_nm, 250_000);
        assert_eq!(sent.layer, BoardLayer::BlBCu as i32);
        let sent = kiapi::board::types::Via::decode(items[1].value.as_slice()).unwrap();
        assert_eq!(
            sent.position,
            Some(Vector2 {
                x_nm: -1,
                y_nm: 10_000_000
            })
        );
        let stack = sent.pad_stack.unwrap();
        assert_eq!(
            stack.drill.unwrap().diameter,
            Some(Vector2 {
                x_nm: 300_000,
                y_nm: 300_000
            })
        );
        assert_eq!(
            stack.copper_layers[0].size,
            Some(Vector2 {
                x_nm: 600_000,
                y_nm: 600_000
            })
        );
    }

    #[test]
    fn the_dry_run_plan_is_the_resolved_plan() {
        let plan = plan_of(&batch());
        let answer = plan_json(&plan, "sha256:x");
        assert_eq!(answer["track_count"], 2);
        assert_eq!(answer["via_count"], 2);
        assert_eq!(
            answer["tracks"][1]["start"],
            json!({ "x": 95.0, "y": 110.0 })
        );
        assert_eq!(
            answer["tracks"][1]["end"],
            json!({ "x": 99.175, "y": 110.0 })
        );
        assert_eq!(answer["tracks"][1]["width"], json!(0.25));
        assert_eq!(answer["tracks"][1]["layer"], "F.Cu");
        // NET_D is net 4 in KiCad's recorded table.
        assert_eq!(answer["tracks"][1]["net_code"], 4);
        assert_eq!(
            answer["vias"][1]["position"],
            json!({ "x": 109.175, "y": 95.0 })
        );
        assert_eq!(answer["vias"][1]["drill"], json!(0.3));
        assert_eq!(answer["vias"][1]["size"], json!(0.6));
        assert_eq!(answer["vias"][1]["net_code"], 4);
        // A via given no type is a through via.
        assert_eq!(answer["vias"][1]["via_type"], "through");
        assert_eq!(answer["vias"][1]["start_layer"], "F.Cu");
        assert_eq!(answer["vias"][1]["end_layer"], "B.Cu");
    }

    #[test]
    fn a_via_span_must_be_its_types() {
        // (via_type, start_layer, end_layer, the field refused)
        let refused = [
            ("laser", Some("F.Cu"), Some("In1.Cu"), "via_type"),
            ("blind", None, None, "start_layer"),
            ("micro", Some("F.Cu"), None, "end_layer"),
            ("micro", Some("F.SilkS"), Some("In1.Cu"), "start_layer"),
            ("buried", Some("In1.Cu"), Some("In1.Cu"), "end_layer"),
            // KiCad reads a span with one end on F.Cu or B.Cu as blind and
            // one with neither as buried, and keeps a through via F.Cu to B.Cu.
            ("blind", Some("In1.Cu"), Some("In2.Cu"), "via_type"),
            ("blind", Some("F.Cu"), Some("B.Cu"), "via_type"),
            ("buried", Some("F.Cu"), Some("In2.Cu"), "via_type"),
            ("through", Some("F.Cu"), Some("In1.Cu"), "via_type"),
            ("through", Some("F.Cu"), None, "end_layer"),
        ];
        for (via_type, start, end, field) in refused {
            let args = json!({ "vias": [spanned(via_type, start, end)] });
            let faults = parse_request(&args).expect_err(&args.to_string());
            assert_eq!(fields(&faults), [format!("vias[0].{field}")], "{args}");
        }
        let args = json!({ "vias": [spanned("blind", Some("In1.Cu"), Some("In2.Cu"))] });
        let reason = &parse_request(&args).unwrap_err()[0].reason;
        assert!(
            reason.contains("In1.Cu to In2.Cu is not its span: a blind via has exactly one end"),
            "{reason}"
        );
    }

    #[test]
    fn a_via_span_is_taken_front_first() {
        use ViaType::{VtBlind, VtBuried, VtMicro, VtThrough};
        // (via_type, start_layer, end_layer, the type and span sent, front first)
        let accepted = [
            (None, None, None, VtThrough, "F.Cu", "B.Cu"),
            (None, Some("B.Cu"), Some("F.Cu"), VtThrough, "F.Cu", "B.Cu"),
            (
                Some("blind"),
                Some("In1.Cu"),
                Some("F.Cu"),
                VtBlind,
                "F.Cu",
                "In1.Cu",
            ),
            (
                Some("blind"),
                Some("B.Cu"),
                Some("In2.Cu"),
                VtBlind,
                "In2.Cu",
                "B.Cu",
            ),
            (
                Some("buried"),
                Some("In2.Cu"),
                Some("In1.Cu"),
                VtBuried,
                "In1.Cu",
                "In2.Cu",
            ),
            (
                Some("micro"),
                Some("F.Cu"),
                Some("In1.Cu"),
                VtMicro,
                "F.Cu",
                "In1.Cu",
            ),
            (
                Some("micro"),
                Some("B.Cu"),
                Some("F.Cu"),
                VtMicro,
                "F.Cu",
                "B.Cu",
            ),
        ];
        for (via_type, start, end, sent_type, front, back) in accepted {
            let mut item = via((146.0, 90.0), 0.3, 0.6, "NET_C");
            for (key, value) in [
                ("via_type", via_type),
                ("start_layer", start),
                ("end_layer", end),
            ] {
                if let Some(value) = value {
                    item[key] = json!(value);
                }
            }
            let args = json!({ "vias": [item] });
            let request =
                parse_request(&args).unwrap_or_else(|faults| panic!("{args}: {faults:?}"));
            let via = &request.vias[0];
            let (start, end) = via.span();
            assert_eq!(
                (via.via_type, layer_text(start), layer_text(end)),
                (sent_type, front, back),
                "{args}"
            );
        }
    }

    #[test]
    fn a_via_span_on_a_layer_the_board_lacks_is_refused() {
        let blind =
            parse_request(&json!({ "vias": [spanned("blind", Some("F.Cu"), Some("In1.Cu"))] }))
                .unwrap();
        let faults = resolve(&blind, &nets(), &enabled()).unwrap_err();
        assert_eq!(fields(&faults), ["vias[0].end_layer"]);
        assert!(
            faults[0].reason.contains("F.Cu, B.Cu"),
            "{}",
            faults[0].reason
        );
        assert!(resolve(&blind, &nets(), &four_layers()).is_ok());
        // The fault is under the key the request gave the layer.
        let reversed =
            parse_request(&json!({ "vias": [spanned("micro", Some("In1.Cu"), Some("F.Cu"))] }))
                .unwrap();
        assert_eq!(
            fields(&resolve(&reversed, &nets(), &enabled()).unwrap_err()),
            ["vias[0].start_layer"]
        );

        let buried =
            parse_request(&json!({ "vias": [spanned("buried", Some("In2.Cu"), Some("In3.Cu"))] }))
                .unwrap();
        let faults = resolve(&buried, &nets(), &four_layers()).unwrap_err();
        assert_eq!(fields(&faults), ["vias[0].end_layer"]);
        assert!(
            faults[0].reason.contains("F.Cu, In1.Cu, In2.Cu, B.Cu"),
            "{}",
            faults[0].reason
        );
    }

    /// What is sent for a blind via is what KiCad 10.0.8 itself holds for
    /// the two on the four-layer board: the type, the drill's ends, front
    /// first, and the one copper entry on F.Cu.
    #[test]
    fn a_blind_via_is_sent_as_kicad_holds_its_own() {
        let mut front = spanned("blind", Some("In1.Cu"), Some("F.Cu"));
        front["position"] = json!({ "x": 146.0, "y": 90.0 });
        let mut back = spanned("blind", Some("B.Cu"), Some("In2.Cu"));
        back["position"] = json!({ "x": 140.0, "y": 90.0 });
        let request = parse_request(&json!({ "vias": [front, back] })).unwrap();
        let plan = resolve(&request, &nets(), &four_layers()).unwrap();

        let answer = plan_json(&plan, "sha256:x");
        assert_eq!(answer["vias"][0]["via_type"], "blind");
        assert_eq!(answer["vias"][0]["start_layer"], "F.Cu");
        assert_eq!(answer["vias"][0]["end_layer"], "In1.Cu");
        assert_eq!(answer["vias"][1]["start_layer"], "In2.Cu");
        assert_eq!(answer["vias"][1]["end_layer"], "B.Cu");

        let kicad = kiapi::common::commands::GetItemsResponse::decode(BLIND_VIAS).unwrap();
        let held: Vec<kiapi::board::types::Via> = kicad
            .items
            .iter()
            .map(|item| kiapi::board::types::Via::decode(item.value.as_slice()).unwrap())
            .collect();
        for item in plan.items() {
            let sent = kiapi::board::types::Via::decode(item.value.as_slice()).unwrap();
            let own = held
                .iter()
                .find(|via| via.position == sent.position)
                .expect("KiCad holds a via there");
            assert_eq!(sent.r#type, own.r#type);
            assert_eq!(sent.r#type, ViaType::VtBlind as i32);
            let (stack, kicad_stack) = (sent.pad_stack.unwrap(), own.pad_stack.clone().unwrap());
            assert_eq!(stack.r#type, kicad_stack.r#type);
            let (mut layers, mut kicad_layers) = (stack.layers.clone(), kicad_stack.layers.clone());
            layers.sort_unstable();
            kicad_layers.sort_unstable();
            assert_eq!(layers, kicad_layers);
            let (drill, kicad_drill) = (stack.drill.unwrap(), kicad_stack.drill.unwrap());
            assert_eq!(
                (
                    drill.start_layer,
                    drill.end_layer,
                    drill.diameter,
                    drill.shape
                ),
                (
                    kicad_drill.start_layer,
                    kicad_drill.end_layer,
                    kicad_drill.diameter,
                    kicad_drill.shape
                )
            );
            assert_eq!(stack.copper_layers.len(), 1);
            assert_eq!(kicad_stack.copper_layers.len(), 1);
            let (copper, kicad_copper) = (&stack.copper_layers[0], &kicad_stack.copper_layers[0]);
            assert_eq!(
                (copper.layer, copper.shape, copper.size),
                (kicad_copper.layer, kicad_copper.shape, kicad_copper.size)
            );
        }
    }

    /// The response names a created via's type and span from what KiCad
    /// reads back: the recorded through via, and the recorded blind ones.
    #[test]
    fn the_read_back_reports_kicads_via_type_and_span() {
        let report = |recorded: &[u8], kiid: &str| {
            kiapi::common::commands::GetItemsResponse::decode(recorded)
                .unwrap()
                .items
                .iter()
                .filter_map(stored_item)
                .find(|stored| stored.kiid == kiid)
                .unwrap()
                .report
        };
        for (recorded, kiid, via_type, start, end) in [
            (
                READ_BACK,
                "97fb9c73-15ec-4507-bd0d-27f94c7eb709",
                "through",
                "F.Cu",
                "B.Cu",
            ),
            (
                BLIND_VIAS,
                "ab6f8834-1aac-4fb7-b536-bb2b9e1d69cf",
                "blind",
                "F.Cu",
                "In1.Cu",
            ),
            (
                BLIND_VIAS,
                "17b4d6e4-982d-4d3d-aa69-1ad59d44a1c3",
                "blind",
                "In2.Cu",
                "B.Cu",
            ),
        ] {
            let report = report(recorded, kiid);
            assert_eq!(report["via_type"], via_type, "{report}");
            assert_eq!(report["start_layer"], start, "{report}");
            assert_eq!(report["end_layer"], end, "{report}");
        }
    }

    #[test]
    fn the_plan_revision_covers_each_vias_type_and_span() {
        let revision = |item: Value| {
            let args = json!({ "vias": [item] });
            revision_of(&args, &nets(), &four_layers())
        };
        let blind = revision(spanned("blind", Some("F.Cu"), Some("In1.Cu")));
        assert_eq!(
            blind,
            revision(spanned("blind", Some("In1.Cu"), Some("F.Cu")))
        );
        assert_ne!(
            blind,
            revision(spanned("micro", Some("F.Cu"), Some("In1.Cu")))
        );
        assert_ne!(
            blind,
            revision(spanned("blind", Some("F.Cu"), Some("In2.Cu")))
        );
        assert_ne!(
            blind,
            revision(spanned("buried", Some("In1.Cu"), Some("In2.Cu")))
        );
        assert_ne!(blind, revision(via((146.0, 90.0), 0.3, 0.6, "NET_C")));
    }

    #[test]
    fn a_net_or_layer_the_board_lacks_is_refused() {
        let request = parse_request(&json!({
            "tracks": [
                track((1.0, 2.0), (3.0, 2.0), 0.25, "F.Cu", "NET_Z"),
                track((1.0, 2.0), (3.0, 2.0), 0.25, "In1.Cu", "NET_D"),
            ],
            "vias": [via((1.0, 2.0), 0.3, 0.6, "net_d")]
        }))
        .unwrap();
        let faults = resolve(&request, &nets(), &enabled()).unwrap_err();
        assert_eq!(
            fields(&faults),
            ["tracks[0].net", "tracks[1].layer", "vias[0].net"]
        );
        assert!(
            faults[1].reason.contains("F.Cu, B.Cu"),
            "{}",
            faults[1].reason
        );
    }

    #[test]
    fn net_zero_is_never_a_target_whatever_it_is_called() {
        let mut table = nets();
        table.push(IpcNet {
            name: "GHOST".to_string(),
            netcode: 0,
        });
        let request =
            parse_request(&json!({ "vias": [via((1.0, 2.0), 0.3, 0.6, "GHOST")] })).unwrap();
        assert_eq!(
            fields(&resolve(&request, &table, &enabled()).unwrap_err()),
            ["vias[0].net"]
        );
    }

    #[test]
    fn the_plan_revision_is_stable_and_bound_to_the_plan_and_the_board() {
        let base = revision_of(&batch(), &nets(), &enabled());
        assert!(base.starts_with("sha256:"));
        assert_eq!(base, revision_of(&batch(), &nets(), &enabled()));
        let mut reordered = nets();
        reordered.reverse();
        assert_eq!(base, revision_of(&batch(), &reordered, &enabled()));

        // A net the plan does not use, renamed.
        let mut renamed = nets();
        renamed
            .iter_mut()
            .find(|net| net.name == "NET_B")
            .unwrap()
            .name = "NET_B2".into();
        assert_ne!(base, revision_of(&batch(), &renamed, &enabled()));
        let mut added = nets();
        added.push(IpcNet {
            name: "NET_E".into(),
            netcode: 5,
        });
        assert_ne!(base, revision_of(&batch(), &added, &enabled()));
        let mut renumbered = nets();
        renumbered
            .iter_mut()
            .find(|net| net.name == "NET_D")
            .unwrap()
            .netcode = 9;
        assert_ne!(base, revision_of(&batch(), &renumbered, &enabled()));

        let mut four_layers = enabled();
        four_layers.copper_layer_count = 4;
        for layer in [BoardLayer::BlIn1Cu, BoardLayer::BlIn2Cu] {
            four_layers.layers.push(IpcLayer {
                name: String::new(),
                id: layer as i32,
                kind: String::new(),
                display_name: None,
            });
        }
        assert_ne!(base, revision_of(&batch(), &nets(), &four_layers));
        // As many layers, one of them another.
        let mut swapped = enabled();
        swapped
            .layers
            .iter_mut()
            .find(|layer| layer.id == BoardLayer::BlUser4 as i32)
            .unwrap()
            .id = BoardLayer::BlUser5 as i32;
        assert_ne!(base, revision_of(&batch(), &nets(), &swapped));

        let mut moved = batch();
        moved["tracks"][1]["end"]["x"] = json!(99.175001);
        assert_ne!(base, revision_of(&moved, &nets(), &enabled()));
        let mut wider = batch();
        wider["tracks"][0]["width"] = json!(0.3);
        assert_ne!(base, revision_of(&wider, &nets(), &enabled()));
        let mut renetted = batch();
        renetted["vias"][1]["net"] = json!("NET_A");
        assert_ne!(base, revision_of(&renetted, &nets(), &enabled()));
        let mut larger = batch();
        larger["vias"][0]["size"] = json!(0.8);
        assert_ne!(base, revision_of(&larger, &nets(), &enabled()));

        let plan = plan_of(&batch());
        assert_ne!(
            base,
            plan_revision(
                &document("/boards/other.kicad_pcb"),
                &nets(),
                &enabled(),
                &plan
            )
        );
    }
}

/// `apply_copper` through served `tools/call`, against a KiCad double that
/// replays KiCad 10.0.8's recorded answers for the batch in
/// `tests/fixtures/issue_95_ipc.README.md`: its net table and layers, its
/// `CreateItems` answer, its read-back after the commit (T2 on `NET_C`, V2 on
/// `NET_A`) and its `DeleteItems` answer, each cut down to the items asked for.
#[cfg(test)]
mod served_tests {
    use crate::mcp::handler::McpHandler;
    use crate::test_support::MockIpcServer;
    use crate::tools::pcb_board::board_mock::board_document;
    use copperpour_ipc::builders::{any_is, any_type_name, pack_any};
    use copperpour_ipc::client::GET_ITEMS_BY_ID_NONE_FOUND;
    use copperpour_ipc::gen::kiapi;
    use kiapi::common::commands as command;
    use kiapi::common::{ApiRequest, ApiResponse, ApiResponseStatus, ApiStatusCode};
    use prost::Message;
    use serde_json::{json, Value};
    use std::collections::BTreeSet;
    use std::sync::{Arc, Mutex};

    const NETS: &[u8] = include_bytes!("../../tests/fixtures/issue_95_nets.ipc.bin");
    const ENABLED_LAYERS: &[u8] =
        include_bytes!("../../tests/fixtures/issue_95_enabled_layers.ipc.bin");
    const CREATED: &[u8] = include_bytes!("../../tests/fixtures/issue_95_create_response.ipc.bin");
    const READ_BACK: &[u8] = include_bytes!("../../tests/fixtures/issue_95_readback.ipc.bin");
    const DELETED: &[u8] = include_bytes!("../../tests/fixtures/issue_95_delete_response.ipc.bin");

    // The KIIDs KiCad gave the recorded batch.
    const T1: &str = "a9120183-d94c-4e22-98e1-75ae01dacf05";
    const V1: &str = "97fb9c73-15ec-4507-bd0d-27f94c7eb709";
    const T2: &str = "e0932d42-fcee-4f81-893e-ef4682d0809b";
    const V2: &str = "e13233db-0e62-41e4-848f-0368fd3948ee";

    /// Departures from the recorded KiCad, one per failure path.
    #[derive(Clone, Copy, Default)]
    struct Quirks {
        /// `CreateItems` refuses the first via (`ISC_INVALID_DATA`, no item).
        refuse_first_via: bool,
        /// `CreateItems` reports the first item created but sends no item back.
        first_item_without_kiid: bool,
        /// `CreateItems` answers one result fewer than it was sent items.
        one_result_short: bool,
        /// Dropping a commit leaves its items on the board.
        drop_keeps_items: bool,
        /// `EndCommit` refuses to drop, so the commit stays open.
        drop_refused: bool,
        /// `DeleteItems` answers as recorded but removes nothing.
        delete_keeps_items: bool,
        /// `GetItemsById` answers this refusal once something was deleted.
        lookup_after_delete: Option<(ApiStatusCode, &'static str)>,
        /// `GetItemsById` never lists this KIID.
        omit_from_read_back: Option<&'static str>,
    }

    #[derive(Default)]
    struct Board {
        /// Every command received, `EndCommit` with its action.
        sent: Vec<String>,
        staged: Vec<String>,
        on_board: BTreeSet<String>,
        created: Vec<command::CreateItems>,
        deleted: Vec<Vec<String>>,
        /// `GetNets` answers with NET_B renamed NET_B2.
        net_b_renamed: bool,
    }

    fn reply(code: ApiStatusCode, error: &str, message: Option<prost_types::Any>) -> ApiResponse {
        ApiResponse {
            status: Some(ApiResponseStatus {
                status: code as i32,
                error_message: error.to_string(),
            }),
            header: None,
            message,
        }
    }

    fn ok(message: prost_types::Any) -> ApiResponse {
        reply(ApiStatusCode::AsOk, "", Some(message))
    }

    fn recorded(type_name: &str, bytes: &[u8]) -> prost_types::Any {
        prost_types::Any {
            type_url: format!("type.googleapis.com/{type_name}"),
            value: bytes.to_vec(),
        }
    }

    /// A track's ends or a via's position, in nm: how the double matches a
    /// requested item to the one KiCad created for it.
    fn place(item: &prost_types::Any) -> Option<Vec<i64>> {
        if any_is(item, "kiapi.board.types.Track") {
            let track = kiapi::board::types::Track::decode(item.value.as_slice()).ok()?;
            let (start, end) = (track.start?, track.end?);
            Some(vec![start.x_nm, start.y_nm, end.x_nm, end.y_nm])
        } else if any_is(item, "kiapi.board.types.Via") {
            let position = kiapi::board::types::Via::decode(item.value.as_slice())
                .ok()?
                .position?;
            Some(vec![position.x_nm, position.y_nm])
        } else {
            None
        }
    }

    fn kiid(item: &prost_types::Any) -> String {
        let id = if any_is(item, "kiapi.board.types.Track") {
            kiapi::board::types::Track::decode(item.value.as_slice())
                .unwrap()
                .id
        } else {
            kiapi::board::types::Via::decode(item.value.as_slice())
                .unwrap()
                .id
        };
        id.unwrap().value
    }

    /// The document a board command names, for the commands that name one.
    fn named_document(
        name: &str,
        command: &prost_types::Any,
    ) -> Option<Option<kiapi::common::types::DocumentSpecifier>> {
        let bytes = command.value.as_slice();
        Some(match name {
            "GetNets" => {
                kiapi::board::commands::GetNets::decode(bytes)
                    .unwrap()
                    .board
            }
            "GetBoardEnabledLayers" => {
                kiapi::board::commands::GetBoardEnabledLayers::decode(bytes)
                    .unwrap()
                    .board
            }
            "SaveDocumentToString" => {
                command::SaveDocumentToString::decode(bytes)
                    .unwrap()
                    .document
            }
            "CreateItems" => command::CreateItems::decode(bytes)
                .unwrap()
                .header
                .and_then(|header| header.document),
            "GetItemsById" => command::GetItemsById::decode(bytes)
                .unwrap()
                .header
                .and_then(|header| header.document),
            "DeleteItems" => command::DeleteItems::decode(bytes)
                .unwrap()
                .header
                .and_then(|header| header.document),
            _ => return None,
        })
    }

    fn respond(
        path: &str,
        quirks: Quirks,
        board: &Mutex<Board>,
        request: ApiRequest,
    ) -> ApiResponse {
        let command = request.message.expect("a command");
        let name = any_type_name(&command)
            .rsplit('.')
            .next()
            .unwrap()
            .to_string();
        let mut board = board.lock().unwrap();
        if name != "EndCommit" {
            board.sent.push(name.clone());
        }
        // Path-bearing IPC never substitutes another board (DEV.md,
        // `wrong_document`), so the double answers only commands that name
        // the board it holds, as KiCad answers only for documents it has open.
        if let Some(document) = named_document(&name, &command) {
            if document.as_ref() != Some(&board_document(path)) {
                board.sent.push(format!("{name}:wrong_document"));
                return reply(
                    ApiStatusCode::AsBadRequest,
                    "the double holds no such document",
                    None,
                );
            }
        }
        match name.as_str() {
            "GetOpenDocuments" => ok(pack_any(
                &command::GetOpenDocumentsResponse {
                    documents: vec![board_document(path)],
                },
                "kiapi.common.commands.GetOpenDocumentsResponse",
            )),
            "GetNets" if board.net_b_renamed => {
                let mut nets = kiapi::board::commands::NetsResponse::decode(NETS).unwrap();
                for net in &mut nets.nets {
                    if net.name == "NET_B" {
                        net.name = "NET_B2".to_string();
                    }
                }
                ok(pack_any(&nets, "kiapi.board.commands.NetsResponse"))
            }
            "GetNets" => ok(recorded("kiapi.board.commands.NetsResponse", NETS)),
            "GetBoardEnabledLayers" => ok(recorded(
                "kiapi.board.commands.BoardEnabledLayersResponse",
                ENABLED_LAYERS,
            )),
            // The recovery snapshot is only ever compared with another one.
            "SaveDocumentToString" => {
                let asked =
                    command::SaveDocumentToString::decode(command.value.as_slice()).unwrap();
                ok(pack_any(
                    &command::SavedDocumentResponse {
                        document: asked.document,
                        contents: "(kicad_pcb (snapshot))".to_string(),
                    },
                    "kiapi.common.commands.SavedDocumentResponse",
                ))
            }
            "BeginCommit" => ok(pack_any(
                &command::BeginCommitResponse {
                    id: Some(kiapi::common::types::Kiid {
                        value: "commit-95".to_string(),
                    }),
                },
                "kiapi.common.commands.BeginCommitResponse",
            )),
            "CreateItems" => {
                let asked = command::CreateItems::decode(command.value.as_slice()).unwrap();
                let kicad = command::CreateItemsResponse::decode(CREATED).unwrap();
                let mut results = Vec::new();
                let mut via_seen = false;
                for item in &asked.items {
                    let is_via = any_is(item, "kiapi.board.types.Via");
                    if quirks.refuse_first_via && is_via && !via_seen {
                        via_seen = true;
                        results.push(command::ItemCreationResult {
                            status: Some(command::ItemStatus {
                                code: command::ItemStatusCode::IscInvalidData as i32,
                                error_message: "refused by the test double".to_string(),
                            }),
                            item: None,
                        });
                        continue;
                    }
                    via_seen |= is_via;
                    let Some(result) = kicad.created_items.iter().find(|result| {
                        result.item.as_ref().is_some_and(|created| {
                            created.type_url == item.type_url && place(created) == place(item)
                        })
                    }) else {
                        return reply(
                            ApiStatusCode::AsBadRequest,
                            "the double has no recorded answer for this item",
                            None,
                        );
                    };
                    results.push(result.clone());
                }
                if quirks.first_item_without_kiid {
                    results[0].item = None;
                }
                if quirks.one_result_short {
                    results.pop();
                }
                for result in &results {
                    if let Some(item) = &result.item {
                        board.staged.push(kiid(item));
                    }
                }
                board.created.push(asked);
                ok(pack_any(
                    &command::CreateItemsResponse {
                        header: None,
                        status: kiapi::common::types::ItemRequestStatus::IrsOk as i32,
                        created_items: results,
                    },
                    "kiapi.common.commands.CreateItemsResponse",
                ))
            }
            "EndCommit" => {
                let end = command::EndCommit::decode(command.value.as_slice()).unwrap();
                if quirks.drop_refused && end.action == command::CommitAction::CmaDrop as i32 {
                    board.sent.push("EndCommit:drop".to_string());
                    return reply(
                        ApiStatusCode::AsBadRequest,
                        "the double refuses to drop",
                        None,
                    );
                }
                let staged = std::mem::take(&mut board.staged);
                if end.action == command::CommitAction::CmaCommit as i32 {
                    board.sent.push("EndCommit:commit".to_string());
                    board.on_board.extend(staged);
                } else {
                    board.sent.push("EndCommit:drop".to_string());
                    if quirks.drop_keeps_items {
                        board.on_board.extend(staged);
                    }
                }
                ok(pack_any(
                    &command::EndCommitResponse {},
                    "kiapi.common.commands.EndCommitResponse",
                ))
            }
            "GetItemsById" => {
                if let Some((code, error)) = quirks.lookup_after_delete {
                    if !board.deleted.is_empty() {
                        return reply(code, error, None);
                    }
                }
                let asked = command::GetItemsById::decode(command.value.as_slice()).unwrap();
                let kicad = command::GetItemsResponse::decode(READ_BACK).unwrap();
                let items: Vec<_> = asked
                    .items
                    .iter()
                    .filter(|id| board.on_board.contains(&id.value))
                    .filter(|id| quirks.omit_from_read_back != Some(id.value.as_str()))
                    .filter_map(|id| kicad.items.iter().find(|item| kiid(item) == id.value))
                    .cloned()
                    .collect();
                if items.is_empty() {
                    return reply(
                        ApiStatusCode::AsBadRequest,
                        GET_ITEMS_BY_ID_NONE_FOUND,
                        None,
                    );
                }
                ok(pack_any(
                    &command::GetItemsResponse {
                        header: None,
                        status: kiapi::common::types::ItemRequestStatus::IrsOk as i32,
                        items,
                    },
                    "kiapi.common.commands.GetItemsResponse",
                ))
            }
            "DeleteItems" => {
                let asked = command::DeleteItems::decode(command.value.as_slice()).unwrap();
                let ids: Vec<String> = asked.item_ids.into_iter().map(|id| id.value).collect();
                let mut kicad = command::DeleteItemsResponse::decode(DELETED).unwrap();
                kicad
                    .deleted_items
                    .retain(|result| ids.contains(&result.id.as_ref().unwrap().value));
                if !quirks.delete_keeps_items {
                    for id in &ids {
                        board.on_board.remove(id);
                    }
                }
                board.deleted.push(ids);
                ok(pack_any(
                    &kicad,
                    "kiapi.common.commands.DeleteItemsResponse",
                ))
            }
            _ => reply(ApiStatusCode::AsUnhandled, "", None),
        }
    }

    struct Kicad {
        _dir: tempfile::TempDir,
        board_path: std::path::PathBuf,
        board: Arc<Mutex<Board>>,
        handler: McpHandler,
        _server: MockIpcServer,
    }

    async fn kicad_with(quirks: Quirks) -> Kicad {
        let dir = tempfile::tempdir().unwrap();
        let board_path = dir.path().join("issue96.kicad_pcb");
        std::fs::write(&board_path, "(kicad_pcb (version 20260206))").unwrap();
        let board = Arc::new(Mutex::new(Board::default()));
        let shared = board.clone();
        let path = board_path.to_string_lossy().to_string();
        let server = MockIpcServer::spawn("apply-copper", move |request| {
            respond(&path, quirks, &shared, request)
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
            board_path,
            board,
            handler,
            _server: server,
        }
    }

    async fn kicad() -> Kicad {
        kicad_with(Quirks::default()).await
    }

    impl Kicad {
        async fn call(&self, mut arguments: Value) -> (bool, Value) {
            arguments["board"] = json!(self.board_path.to_string_lossy());
            let response = self
                .handler
                .handle_message(json!({
                    "jsonrpc": "2.0",
                    "id": 95,
                    "method": "tools/call",
                    "params": { "name": "apply_copper", "arguments": arguments }
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

        /// A dry run that must pass; its plan_revision.
        async fn plan(&self, batch: &Value) -> String {
            let (is_error, answer) = self.call(batch.clone()).await;
            assert!(!is_error, "{answer}");
            answer["plan_revision"].as_str().unwrap().to_string()
        }

        /// A dry run, then the apply of its revision, logged from the apply.
        async fn apply(&self, batch: &Value) -> (bool, Value) {
            let revision = self.plan(batch).await;
            self.board.lock().unwrap().sent.clear();
            let mut arguments = batch.clone();
            arguments["dry_run"] = json!(false);
            arguments["expected_plan_revision"] = json!(revision);
            self.call(arguments).await
        }

        fn sent(&self) -> Vec<String> {
            self.board.lock().unwrap().sent.clone()
        }

        fn count(&self, command: &str) -> usize {
            self.sent().iter().filter(|sent| *sent == command).count()
        }

        fn on_board(&self) -> BTreeSet<String> {
            self.board.lock().unwrap().on_board.clone()
        }

        /// Nothing that could change the board reached KiCad.
        fn assert_untouched(&self) {
            let sent = self.sent();
            for command in [
                "SaveDocumentToString",
                "BeginCommit",
                "CreateItems",
                "EndCommit:commit",
                "EndCommit:drop",
                "DeleteItems",
            ] {
                assert!(
                    !sent.iter().any(|s| s == command),
                    "{command} sent: {sent:?}"
                );
            }
            assert!(self.on_board().is_empty());
        }
    }

    fn t1() -> Value {
        json!({ "start": { "x": 130, "y": 90 }, "end": { "x": 135, "y": 90 }, "width": 0.25, "layer": "F.Cu", "net": "NET_D" })
    }

    fn t2() -> Value {
        json!({ "start": { "x": 95, "y": 110 }, "end": { "x": 99.175, "y": 110 }, "width": 0.25, "layer": "F.Cu", "net": "NET_D" })
    }

    fn v1() -> Value {
        json!({ "position": { "x": 135, "y": 90 }, "drill": 0.3, "size": 0.6, "net": "NET_D" })
    }

    fn v2() -> Value {
        json!({ "position": { "x": 109.175, "y": 95 }, "drill": 0.3, "size": 0.6, "net": "NET_D" })
    }

    /// T1 and V1: clear of other nets' copper.
    fn clear_batch() -> Value {
        json!({ "tracks": [t1()], "vias": [v1()] })
    }

    /// All four: T2 ends on a NET_C pad and V2 sits on a NET_A pad.
    fn full_batch() -> Value {
        json!({ "tracks": [t1(), t2()], "vias": [v1(), v2()] })
    }

    fn kind(answer: &Value) -> &str {
        answer["error"]["kind"].as_str().unwrap_or("")
    }

    #[tokio::test]
    async fn a_dry_run_returns_the_normalized_plan_and_writes_nothing() {
        let kicad = kicad().await;
        let (is_error, answer) = kicad.call(full_batch()).await;
        assert!(!is_error, "{answer}");
        assert_eq!(answer["dry_run"], true);
        assert_eq!(
            (answer["track_count"].clone(), answer["via_count"].clone()),
            (json!(2), json!(2))
        );
        assert_eq!(
            answer["tracks"][1]["end"],
            json!({ "x": 99.175, "y": 110.0 })
        );
        assert_eq!(answer["tracks"][1]["layer"], "F.Cu");
        assert_eq!(
            answer["vias"][1]["position"],
            json!({ "x": 109.175, "y": 95.0 })
        );
        assert_eq!(answer["vias"][1]["net"], "NET_D");
        assert_eq!(answer["vias"][1]["net_code"], 4);
        assert!(answer["plan_revision"]
            .as_str()
            .unwrap()
            .starts_with("sha256:"));
        kicad.assert_untouched();
    }

    /// A via's type and layers reach the plan through `tools/call`; on this
    /// two-layer board a blind via has no inner layer to end on.
    #[tokio::test]
    async fn a_via_span_is_planned_and_checked_against_the_board() {
        let kicad = kicad().await;
        let mut through = v1();
        through["via_type"] = json!("through");
        through["start_layer"] = json!("B.Cu");
        through["end_layer"] = json!("F.Cu");
        let (is_error, answer) = kicad.call(json!({ "vias": [through] })).await;
        assert!(!is_error, "{answer}");
        assert_eq!(answer["vias"][0]["via_type"], "through");
        assert_eq!(answer["vias"][0]["start_layer"], "F.Cu");
        assert_eq!(answer["vias"][0]["end_layer"], "B.Cu");

        let mut blind = v1();
        blind["via_type"] = json!("blind");
        blind["start_layer"] = json!("F.Cu");
        blind["end_layer"] = json!("In1.Cu");
        let (is_error, answer) = kicad.call(json!({ "vias": [blind] })).await;
        assert!(is_error, "{answer}");
        assert_eq!(kind(&answer), "invalid_argument");
        assert_eq!(answer["error"]["field"], "vias[0].end_layer");

        let mut unknown = v1();
        unknown["via_type"] = json!("laser");
        let (is_error, answer) = kicad.call(json!({ "vias": [unknown] })).await;
        assert!(is_error, "{answer}");
        assert_eq!(kind(&answer), "invalid_argument");
        kicad.assert_untouched();
    }

    #[tokio::test]
    async fn an_apply_without_a_revision_sends_nothing() {
        let kicad = kicad().await;
        let mut arguments = clear_batch();
        arguments["dry_run"] = json!(false);
        let (is_error, answer) = kicad.call(arguments).await;
        assert!(is_error);
        assert_eq!(kind(&answer), "invalid_argument");
        assert_eq!(answer["error"]["field"], "expected_plan_revision");
        assert!(kicad.sent().is_empty(), "{:?}", kicad.sent());
    }

    #[tokio::test]
    async fn an_apply_with_a_stale_revision_writes_nothing() {
        let kicad = kicad().await;
        kicad.plan(&clear_batch()).await;
        let mut arguments = clear_batch();
        arguments["dry_run"] = json!(false);
        arguments["expected_plan_revision"] = json!("sha256:stale");
        let (is_error, answer) = kicad.call(arguments).await;
        assert!(is_error);
        assert_eq!(kind(&answer), "stale_target");
        kicad.assert_untouched();
    }

    #[tokio::test]
    async fn a_net_renamed_after_the_dry_run_makes_the_apply_stale() {
        let kicad = kicad().await;
        let revision = kicad.plan(&clear_batch()).await;
        kicad.board.lock().unwrap().net_b_renamed = true;
        let mut arguments = clear_batch();
        arguments["dry_run"] = json!(false);
        arguments["expected_plan_revision"] = json!(revision);
        let (is_error, answer) = kicad.call(arguments).await;
        assert!(is_error, "{answer}");
        assert_eq!(kind(&answer), "stale_target");
        kicad.assert_untouched();
    }

    #[tokio::test]
    async fn a_clear_batch_is_one_commit_reported_from_the_read_back() {
        let kicad = kicad().await;
        let (is_error, answer) = kicad.apply(&clear_batch()).await;
        assert!(!is_error, "{answer}");
        assert_eq!(answer["dry_run"], false);
        assert_eq!(
            (answer["track_count"].clone(), answer["via_count"].clone()),
            (json!(1), json!(1))
        );
        assert_eq!(answer["tracks"][0]["uuid"], T1);
        assert_eq!(answer["tracks"][0]["net"], "NET_D");
        assert_eq!(answer["tracks"][0]["end"], json!({ "x": 135.0, "y": 90.0 }));
        assert_eq!(answer["vias"][0]["uuid"], V1);
        assert_eq!(answer["vias"][0]["net"], "NET_D");
        assert_eq!(answer["vias"][0]["drill"], json!(0.3));
        assert_eq!(answer["vias"][0]["via_type"], "through");
        assert_eq!(answer["vias"][0]["start_layer"], "F.Cu");
        assert_eq!(answer["vias"][0]["end_layer"], "B.Cu");
        assert_eq!(answer["source"], "ipc_readback");
        for (command, times) in [
            ("BeginCommit", 1),
            ("CreateItems", 1),
            ("EndCommit:commit", 1),
            ("EndCommit:drop", 0),
            ("GetItemsById", 1),
            ("DeleteItems", 0),
        ] {
            assert_eq!(kicad.count(command), times, "{command}: {:?}", kicad.sent());
        }
        assert_eq!(
            kicad.on_board(),
            BTreeSet::from([T1.to_string(), V1.to_string()])
        );

        // The one CreateItems carried the request in nanometres.
        let created = kicad.board.lock().unwrap().created[0].clone();
        let track = kiapi::board::types::Track::decode(created.items[0].value.as_slice()).unwrap();
        let nm = |x_nm, y_nm| Some(kiapi::common::types::Vector2 { x_nm, y_nm });
        assert_eq!(track.start, nm(130_000_000, 90_000_000));
        assert_eq!(track.end, nm(135_000_000, 90_000_000));
        assert_eq!(track.width.unwrap().value_nm, 250_000);
        assert_eq!(track.layer, kiapi::board::types::BoardLayer::BlFCu as i32);
        assert_eq!(track.net.unwrap().name, "NET_D");
        let via = kiapi::board::types::Via::decode(created.items[1].value.as_slice()).unwrap();
        assert_eq!(via.position, nm(135_000_000, 90_000_000));
        assert_eq!(via.r#type, kiapi::board::types::ViaType::VtThrough as i32);
        let stack = via.pad_stack.unwrap();
        assert_eq!(stack.drill.unwrap().diameter, nm(300_000, 300_000));
        assert_eq!(stack.copper_layers[0].size, nm(600_000, 600_000));
    }

    #[tokio::test]
    async fn a_batch_kicad_stores_on_other_nets_is_removed_and_refused() {
        let kicad = kicad().await;
        let (is_error, answer) = kicad.apply(&full_batch()).await;
        assert!(is_error, "{answer}");
        assert_eq!(kind(&answer), "plan_blocked");
        let reasons: Vec<&str> = answer["error"]["reasons"]
            .as_array()
            .unwrap()
            .iter()
            .map(|reason| reason.as_str().unwrap())
            .collect();
        assert_eq!(reasons.len(), 2, "{reasons:?}");
        assert!(reasons[0].starts_with("tracks[1] "), "{}", reasons[0]);
        assert!(
            reasons[0].contains(T2) && reasons[0].contains("net 'NET_C'"),
            "{}",
            reasons[0]
        );
        assert!(reasons[1].starts_with("vias[1] "), "{}", reasons[1]);
        assert!(
            reasons[1].contains(V2) && reasons[1].contains("net 'NET_A'"),
            "{}",
            reasons[1]
        );
        // The whole batch went, in a second commit, and the board holds none of it.
        assert_eq!(
            kicad.board.lock().unwrap().deleted,
            [[T1, T2, V1, V2].map(String::from).to_vec()]
        );
        assert!(kicad.on_board().is_empty());
        for (command, times) in [
            ("BeginCommit", 2),
            ("CreateItems", 1),
            ("EndCommit:commit", 2),
            ("DeleteItems", 1),
            ("GetItemsById", 2),
        ] {
            assert_eq!(kicad.count(command), times, "{command}: {:?}", kicad.sent());
        }
    }

    #[tokio::test]
    async fn a_removal_the_board_does_not_confirm_is_uncertain_with_the_kiids() {
        let kicad = kicad_with(Quirks {
            delete_keeps_items: true,
            ..Default::default()
        })
        .await;
        let (is_error, answer) = kicad.apply(&full_batch()).await;
        assert!(is_error, "{answer}");
        assert_eq!(kind(&answer), "mutation_outcome_uncertain");
        let reason = answer["error"]["reason"].as_str().unwrap();
        for kiid in [T1, T2, V1, V2] {
            assert!(reason.contains(kiid), "{kiid} missing from {reason}");
        }
        assert!(reason.contains("still finds 4 of its 4 items"), "{reason}");
    }

    #[tokio::test]
    async fn only_kicads_none_found_answer_proves_the_items_gone() {
        for refusal in [
            (ApiStatusCode::AsBadRequest, "the board is being rebuilt"),
            (ApiStatusCode::AsBusy, GET_ITEMS_BY_ID_NONE_FOUND),
        ] {
            let kicad = kicad_with(Quirks {
                lookup_after_delete: Some(refusal),
                ..Default::default()
            })
            .await;
            let (is_error, answer) = kicad.apply(&full_batch()).await;
            assert!(is_error, "{answer}");
            assert_eq!(
                kind(&answer),
                "mutation_outcome_uncertain",
                "{refusal:?}: {answer}"
            );
            assert!(answer["error"]["reason"].as_str().unwrap().contains(T2));
        }
    }

    #[tokio::test]
    async fn a_partial_create_is_dropped_and_refused() {
        let kicad = kicad_with(Quirks {
            refuse_first_via: true,
            ..Default::default()
        })
        .await;
        let (is_error, answer) = kicad.apply(&clear_batch()).await;
        assert!(is_error, "{answer}");
        assert_eq!(kind(&answer), "plan_blocked");
        let reasons = answer["error"]["reasons"].as_array().unwrap();
        assert_eq!(reasons.len(), 1, "{reasons:?}");
        let reason = reasons[0].as_str().unwrap();
        assert!(
            reason.starts_with("vias[0] ") && reason.contains("ISC_INVALID_DATA"),
            "{reason}"
        );
        for (command, times) in [
            ("CreateItems", 1),
            ("EndCommit:drop", 1),
            ("EndCommit:commit", 0),
            ("DeleteItems", 0),
            // The lookup of T1, the one item KiCad did create.
            ("GetItemsById", 1),
        ] {
            assert_eq!(kicad.count(command), times, "{command}: {:?}", kicad.sent());
        }
        assert!(kicad.on_board().is_empty());
    }

    #[tokio::test]
    async fn a_partial_create_whose_items_remain_is_uncertain() {
        let kicad = kicad_with(Quirks {
            refuse_first_via: true,
            drop_keeps_items: true,
            ..Default::default()
        })
        .await;
        let (is_error, answer) = kicad.apply(&clear_batch()).await;
        assert!(is_error, "{answer}");
        assert_eq!(kind(&answer), "mutation_outcome_uncertain");
        assert!(answer["error"]["reason"].as_str().unwrap().contains(T1));
    }

    #[tokio::test]
    async fn a_partial_create_whose_drop_kicad_refuses_is_uncertain() {
        let kicad = kicad_with(Quirks {
            refuse_first_via: true,
            drop_refused: true,
            ..Default::default()
        })
        .await;
        let (is_error, answer) = kicad.apply(&clear_batch()).await;
        assert!(is_error, "{answer}");
        // T1 is still staged in an open commit, where no lookup can see it.
        assert_eq!(kind(&answer), "mutation_outcome_uncertain", "{answer}");
        assert!(answer["error"]["reason"].as_str().unwrap().contains(T1));
        assert_eq!(kicad.count("EndCommit:commit"), 0);
    }

    #[tokio::test]
    async fn a_created_item_without_a_kiid_is_dropped_not_committed() {
        let kicad = kicad_with(Quirks {
            first_item_without_kiid: true,
            ..Default::default()
        })
        .await;
        let (is_error, answer) = kicad.apply(&clear_batch()).await;
        assert!(is_error, "{answer}");
        assert_eq!(kind(&answer), "plan_blocked");
        assert!(answer["error"]["reasons"][0]
            .as_str()
            .unwrap()
            .starts_with("tracks[0]: "));
        assert_eq!(kicad.count("EndCommit:drop"), 1);
        assert_eq!(kicad.count("EndCommit:commit"), 0);
        assert!(kicad.on_board().is_empty());
    }

    #[tokio::test]
    async fn an_answer_that_does_not_match_the_request_is_dropped() {
        let kicad = kicad_with(Quirks {
            one_result_short: true,
            ..Default::default()
        })
        .await;
        let (is_error, answer) = kicad.apply(&clear_batch()).await;
        assert!(is_error, "{answer}");
        assert!(answer.to_string().contains("per-item results"), "{answer}");
        assert_eq!(kicad.count("EndCommit:drop"), 1);
        assert_eq!(kicad.count("EndCommit:commit"), 0);
        assert!(kicad.on_board().is_empty());
    }

    #[tokio::test]
    async fn a_read_back_missing_an_item_is_uncertain() {
        let kicad = kicad_with(Quirks {
            omit_from_read_back: Some(V1),
            ..Default::default()
        })
        .await;
        let (is_error, answer) = kicad.apply(&clear_batch()).await;
        assert!(is_error, "{answer}");
        assert_eq!(kind(&answer), "mutation_outcome_uncertain");
        let reason = answer["error"]["reason"].as_str().unwrap();
        assert!(
            reason.contains("vias[0]") && reason.contains(V1),
            "{reason}"
        );
        assert_eq!(kicad.count("DeleteItems"), 0);
    }

    #[tokio::test]
    async fn refusals_name_the_field_and_write_nothing() {
        let bad = |key: &str, index: usize, field: &str, value: Value| {
            let mut batch = full_batch();
            batch[key][index][field] = value;
            batch
        };
        let cases = [
            // The handler: a schema cannot compare drill with size.
            (bad("vias", 0, "drill", json!(0.6)), "vias[0].drill"),
            // The schema gate, before the handler.
            (bad("tracks", 0, "width", json!(0)), "tracks[0].width"),
            // The board.
            (bad("tracks", 1, "net", json!("NET_Z")), "tracks[1].net"),
            (
                bad("tracks", 0, "layer", json!("In1.Cu")),
                "tracks[0].layer",
            ),
            (json!({ "tracks": [], "vias": [] }), "tracks"),
        ];
        for (mut arguments, field) in cases {
            let kicad = kicad().await;
            arguments["dry_run"] = json!(false);
            arguments["expected_plan_revision"] = json!("sha256:any");
            let (is_error, answer) = kicad.call(arguments).await;
            assert!(is_error, "{answer}");
            assert_eq!(kind(&answer), "invalid_argument", "{field}: {answer}");
            assert_eq!(answer["error"]["field"], field, "{answer}");
            kicad.assert_untouched();
        }
    }

    #[tokio::test]
    async fn direct_calls_refuse_options_of_the_wrong_type() {
        let ctx = crate::tools::pcb_board::board_mock::ctx_talking_to(String::new());
        for (option, value) in [
            ("dry_run", json!("false")),
            ("expected_plan_revision", json!(7)),
        ] {
            let mut arguments = clear_batch();
            arguments["board"] = json!("/boards/issue96.kicad_pcb");
            arguments[option] = value;
            let result = super::handle_apply_copper(&arguments, &ctx).await.unwrap();
            assert_eq!(
                crate::mcp::error::extract_error_kind(&result).as_deref(),
                Some("invalid_argument"),
                "{option}"
            );
            assert!(format!("{:?}", result.content).contains(option), "{option}");
        }
    }
}
