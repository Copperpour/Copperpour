//! Revision-bound rigid moves and turns of board items (#652). KiCad source
//! lines are cited at 10.0.6.

use super::pcb_board::{preflight_refusal_kind, preflight_refusal_result};
use super::pcb_copper_check::{self, footprint_reference, MovedCopper, PoseRequest};
use super::pcb_via_positions::{blocked, envelope, point, snapshot, stale, uncertain, with_key};
use crate::hex::encode_hex;
use crate::mcp::protocol::CallToolResult;
use crate::tool;
use crate::tools::{
    get_path, invalid_arg, ipc_failure_result, opt_bool_flag, require_array, require_f64,
    with_bound_board_ipc_classified, BoardAccess, BoardBinding, ToolContext, ToolDef,
};
use copperpour_ipc::builders::{any_is, any_type_name, nm_to_mm, pack_any};
use copperpour_ipc::client::{diff_nodes, IpcBatchRecovered, IpcOutcomeUnknown, KiCadIpcClient};
use copperpour_ipc::gen::kiapi::{
    board::types::{
        Arc, BoardGraphicShape, BoardText, BoardTextBox, Field, FootprintInstance, Group, Net, Pad,
        ReferencePoint, Track, Via, Zone, ZoneType,
    },
    common::types::{
        graphic_shape::Geometry, poly_line_node, Angle, DocumentSpecifier, KiCadObjectType as Kot,
        Kiid, LockedState, PolySet, TextAttributes, Vector2,
    },
};
use copperpour_ipc::transform::{
    keep_footprint_text_upright, normalize_deg, rotate_point, transform_board_graphic,
    transform_board_text_box, transform_footprint_children, transform_zone, within_a_turn, Xform,
};
use copperpour_ipc::types::IpcFootprintPlacement;
use copperpour_ipc::{IpcFailure, PreflightRefusal};
use copperpour_sexp::SexpNode;
use prost::Message;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::path::Path;

const TOOL: &str = "transform_pcb_items";
const UNDO: &str = "Transform PCB items";
const FOOTPRINT: &str = "kiapi.board.types.FootprintInstance";
const VIA: &str = "kiapi.board.types.Via";
const TRACK: &str = "kiapi.board.types.Track";
const ARC: &str = "kiapi.board.types.Arc";
const TEXT: &str = "kiapi.board.types.BoardText";
const TEXT_BOX: &str = "kiapi.board.types.BoardTextBox";
const GRAPHIC: &str = "kiapi.board.types.BoardGraphicShape";
const ZONE: &str = "kiapi.board.types.Zone";
const GROUP: &str = "kiapi.board.types.Group";
const PAD: &str = "kiapi.board.types.Pad";
const FIELD: &str = "kiapi.board.types.Field";
const REFERENCE_POINT: &str = "kiapi.board.types.ReferencePoint";
const BOARD: [Kot; 8] = [
    Kot::KotPcbFootprint,
    Kot::KotPcbTrace,
    Kot::KotPcbVia,
    Kot::KotPcbArc,
    Kot::KotPcbText,
    Kot::KotPcbTextbox,
    Kot::KotPcbShape,
    Kot::KotPcbZone,
];
const COPPER: [Kot; 3] = [Kot::KotPcbTrace, Kot::KotPcbVia, Kot::KotPcbArc];
const LIMIT: i64 = super::pcb_board_outline::COORDINATE_LIMIT_NM;
const OUT_OF_RANGE: &str = "a destination is outside KiCad's board coordinate range";
const SHOWN_BOUNDARY_CONNECTIONS: usize = 100;

type Refusal<T> = Result<T, CallToolResult>;

/// Board items by UUID, as KiCad sent them.
type Items = BTreeMap<String, Item>;

/// Board groups by UUID, as KiCad sent them.
type Groups = BTreeMap<String, Group>;

pub(crate) fn tool() -> ToolDef {
    tool!(
        "transform_pcb_items",
        "Move or turn footprints, vias, straight and arc tracks, board texts, text boxes, \
         graphics, zones and rule areas as one rigid block in one KiCad commit and undo step; a \
         group's UUID moves the whole group (KiCad 10.0.7 or newer). Footprints by references, \
         any of these by uuids; offset {x,y} mm, or rotation degrees (counterclockwise) about \
         origin {x,y}. Text boxes and rounded rectangles turn by multiples of 90° only. Nothing \
         is rerouted: unselected copper ending on a selected via, track or arc end, or pad is \
         listed in boundary_connections; no pad may land on another net's via, track or arc. \
         Refused before any write: other types, a group moved in part, teardrop zones, tuning \
         patterns and their tracks, a hatch fill or hatch smoothing KiCad's API would drop, and \
         locked, missing or duplicate targets. Dry run by default; apply needs its \
         expected_plan_revision. UUIDs and nets are kept; targets and groups are read back and \
         the board text is compared after the commit; a KiCad net change is undone by a second \
         commit. Unconfirmed outcomes are mutation_outcome_uncertain with UUIDs. No save or \
         refill; a zone's fill is carried, stale until refilled.",
        json!({
            "type": "object",
            "properties": {
                "board": {"type": "string"},
                "references": {"type": "array", "minItems": 1,
                    "items": {"type": "string", "minLength": 1},
                    "description": "Footprints by reference designator"},
                "uuids": {"type": "array", "minItems": 1,
                    "items": {"type": "string", "minLength": 1},
                    "description": "Footprints, vias, straight and arc tracks, board texts, text \
                        boxes, graphics, zones, rule areas and groups by UUID; a group's UUID \
                        stands for every member"},
                "offset": {"type": "object", "properties": {
                    "x": {"type": "number", "description": "X offset in mm"},
                    "y": {"type": "number", "description": "Y offset in mm"}
                }, "required": ["x", "y"]},
                "rotation": {"type": "number", "description":
                    "Turn in degrees, counterclockwise as KiCad shows it (the sense of a \
                     footprint's rotation)"},
                "origin": {"type": "object", "description": "The point turned about", "properties": {
                    "x": {"type": "number", "description": "X in mm"},
                    "y": {"type": "number", "description": "Y in mm"}
                }, "required": ["x", "y"]},
                "dry_run": {"type": "boolean", "default": true},
                "expected_plan_revision": {"type": "string"}
            },
            "required": ["board"],
            "anyOf": [{"required": ["references"]}, {"required": ["uuids"]}],
            "oneOf": [
                {"required": ["offset"], "not": {"anyOf": [
                    {"required": ["rotation"]}, {"required": ["origin"]}
                ]}},
                {"required": ["rotation", "origin"], "not": {"required": ["offset"]}}
            ]
        }),
        |args, ctx| async move { handle_transform_pcb_items(args, ctx).await }
    )
    .with_board_access(BoardAccess::LiveOnly)
}

/// A rigid transform: a translation, or a turn about a point.
#[derive(Clone, Copy, Debug)]
enum Transform {
    Offset(Vector2),
    Turn { degrees: f64, origin: Vector2 },
}

impl Transform {
    /// The turn in degrees, in (-180, 180]: zero for an offset.
    fn degrees(self) -> f64 {
        match self {
            Self::Turn { degrees, .. } => degrees,
            Self::Offset(_) => 0.0,
        }
    }

    /// The argument a destination out of range is refused on.
    fn field(self) -> &'static str {
        match self {
            Self::Offset(_) => "offset",
            Self::Turn { .. } => "rotation",
        }
    }

    /// Where `p` lands: a checked add, or KiCad's turn about a centre, which
    /// subtracts, turns and adds in integers (`libs/kimath/src/trigo.cpp:267-278`).
    fn point(self, p: Vector2) -> Refusal<Vector2> {
        let moved = match self {
            Self::Offset(by) => p.x_nm.checked_add(by.x_nm).zip(p.y_nm.checked_add(by.y_nm)),
            Self::Turn { origin, .. } => {
                let (x, y) =
                    rotate_point(p.x_nm - origin.x_nm, p.y_nm - origin.y_nm, self.degrees());
                Some((x + origin.x_nm, y + origin.y_nm))
            }
        };
        moved
            .map(|(x_nm, y_nm)| Vector2 { x_nm, y_nm })
            .ok_or_else(|| invalid_arg(self.field(), OUT_OF_RANGE))
    }

    /// The same transform for `transform.rs`, which turns by KiCad's rule too.
    fn xform(self) -> Xform {
        match self {
            Self::Offset(by) => Xform::Translate {
                dx_nm: by.x_nm,
                dy_nm: by.y_nm,
            },
            Self::Turn { degrees, origin } => Xform::Rotate {
                cx_nm: origin.x_nm,
                cy_nm: origin.y_nm,
                delta_deg: degrees,
            },
        }
    }
}

fn in_range(p: Vector2) -> bool {
    p.x_nm.unsigned_abs() <= LIMIT as u64 && p.y_nm.unsigned_abs() <= LIMIT as u64
}

#[derive(Debug)]
struct Request {
    references: Vec<String>,
    uuids: Vec<String>,
    transform: Transform,
}

/// An optional list of nonempty, distinct strings.
fn names(args: &Value, field: &str) -> Refusal<Vec<String>> {
    if args.get(field).is_none() {
        return Ok(Vec::new());
    }
    let items = require_array(args, field)?;
    if items.is_empty() {
        return Err(invalid_arg(field, "must not be empty"));
    }
    let mut seen = HashSet::new();
    let mut names = Vec::with_capacity(items.len());
    for (index, item) in items.iter().enumerate() {
        let name = item
            .as_str()
            .ok_or_else(|| invalid_arg(&format!("{field}[{index}]"), "must be a string"))?;
        if name.trim().is_empty() || !seen.insert(name) {
            return Err(invalid_arg(field, "entries must be nonempty and unique"));
        }
        names.push(name.to_string());
    }
    Ok(names)
}

fn parse(args: &Value) -> Refusal<Request> {
    let references = names(args, "references")?;
    let uuids = names(args, "uuids")?;
    if references.is_empty() && uuids.is_empty() {
        return Err(invalid_arg("references", "give references, uuids or both"));
    }
    // Reduced once, so the anchor, the copper and the children turn by one angle.
    let transform = match (args.get("offset"), args.get("rotation"), args.get("origin")) {
        (Some(offset), None, None) => Transform::Offset(point(offset)?),
        (None, Some(_), Some(origin)) => Transform::Turn {
            degrees: normalize_deg(require_f64(args, "rotation")?),
            origin: point(origin)?,
        },
        (Some(_), _, _) => return Err(invalid_arg("offset", "excludes rotation and origin")),
        (None, Some(_), None) => return Err(invalid_arg("origin", "required with rotation")),
        (None, None, Some(_)) => return Err(invalid_arg("rotation", "required with origin")),
        (None, None, None) => {
            return Err(invalid_arg("offset", "required, or rotation with origin"))
        }
    };
    Ok(Request {
        references,
        uuids,
        transform,
    })
}

/// A board item as KiCad sent it. Each kind is a variant here and an arm in
/// each method; the rest of the tool goes through these.
#[derive(Clone, Debug, PartialEq)]
enum Item {
    Footprint(Box<FootprintInstance>),
    Via(Box<Via>),
    Track(Track),
    Arc(Arc),
    Text(Box<BoardText>),
    TextBox(Box<BoardTextBox>),
    Graphic(Box<BoardGraphicShape>),
    Zone(Box<Zone>),
}

impl Item {
    /// Every kind, as [`Item::kind`] names it, and the key a response lists
    /// its rows under.
    const KINDS: [(&'static str, &'static str); 8] = [
        ("footprint", "footprints"),
        ("via", "vias"),
        ("track", "tracks"),
        ("arc", "arcs"),
        ("text", "texts"),
        ("text_box", "text_boxes"),
        ("graphic", "graphics"),
        ("zone", "zones"),
    ];

    /// By declared type, never by trial decode; `None` for any other type.
    fn decode(any: &prost_types::Any) -> anyhow::Result<Option<Self>> {
        let bytes = any.value.as_slice();
        Ok(Some(match any_type_name(any) {
            FOOTPRINT => Self::Footprint(Box::new(FootprintInstance::decode(bytes)?)),
            VIA => Self::Via(Box::new(Via::decode(bytes)?)),
            TRACK => Self::Track(Track::decode(bytes)?),
            ARC => Self::Arc(Arc::decode(bytes)?),
            TEXT => Self::Text(Box::new(BoardText::decode(bytes)?)),
            TEXT_BOX => Self::TextBox(Box::new(BoardTextBox::decode(bytes)?)),
            GRAPHIC => Self::Graphic(Box::new(BoardGraphicShape::decode(bytes)?)),
            ZONE => Self::Zone(Box::new(Zone::decode(bytes)?)),
            _ => return Ok(None),
        }))
    }

    fn pack(&self) -> prost_types::Any {
        match self {
            Self::Footprint(footprint) => pack_any(footprint.as_ref(), FOOTPRINT),
            Self::Via(via) => pack_any(via.as_ref(), VIA),
            Self::Track(track) => pack_any(track, TRACK),
            Self::Arc(arc) => pack_any(arc, ARC),
            Self::Text(text) => pack_any(text.as_ref(), TEXT),
            Self::TextBox(text_box) => pack_any(text_box.as_ref(), TEXT_BOX),
            Self::Graphic(graphic) => pack_any(graphic.as_ref(), GRAPHIC),
            Self::Zone(zone) => pack_any(zone.as_ref(), ZONE),
        }
    }

    fn kind(&self) -> &'static str {
        match self {
            Self::Footprint(_) => "footprint",
            Self::Via(_) => "via",
            Self::Track(_) => "track",
            Self::Arc(_) => "arc",
            Self::Text(_) => "text",
            Self::TextBox(_) => "text_box",
            Self::Graphic(_) => "graphic",
            Self::Zone(_) => "zone",
        }
    }

    fn id(&self) -> String {
        let id = match self {
            Self::Footprint(footprint) => &footprint.id,
            Self::Via(via) => &via.id,
            Self::Track(track) => &track.id,
            Self::Arc(arc) => &arc.id,
            Self::Text(text) => &text.id,
            Self::TextBox(text_box) => &text_box.id,
            Self::Graphic(graphic) => &graphic.id,
            Self::Zone(zone) => &zone.id,
        };
        id.as_ref().map(|id| id.value.clone()).unwrap_or_default()
    }

    /// How a reason names it: a footprint by reference and UUID.
    fn label(&self, id: &str) -> String {
        match self {
            Self::Footprint(footprint) => {
                format!("footprint {} ({id})", footprint_reference(footprint))
            }
            Self::Zone(zone) if zone.r#type == ZoneType::ZtRuleArea as i32 => {
                format!("rule area '{id}'")
            }
            other => format!("{} '{id}'", other.kind().replace('_', " ")),
        }
    }

    /// The lock state KiCad's message gives. A zone's is not in it: see
    /// [`zone_locked`].
    fn locked(&self) -> i32 {
        match self {
            Self::Footprint(footprint) => footprint.locked,
            Self::Via(via) => via.locked,
            Self::Track(track) => track.locked,
            Self::Arc(arc) => arc.locked,
            Self::Text(text) => text.locked,
            Self::TextBox(text_box) => text_box.locked,
            Self::Graphic(graphic) => graphic.locked,
            Self::Zone(zone) => zone.locked,
        }
    }

    /// Copper KiCad re-nets across the board when a commit is pushed.
    fn is_copper(&self) -> bool {
        matches!(self, Self::Via(_) | Self::Track(_) | Self::Arc(_))
    }

    /// The net KiCad may change, on copper and on a selected graphic. A
    /// zone's net KiCad does not change (`CN_ZONE_LAYER` is built with
    /// `aCanChangeNet` false, `pcbnew/connectivity/connectivity_items.h:132,
    /// 288`): it is compared with the rest of the zone.
    fn net(&self) -> Option<&Net> {
        match self {
            Self::Footprint(_) | Self::Text(_) | Self::TextBox(_) | Self::Zone(_) => None,
            Self::Via(via) => via.net.as_ref(),
            Self::Track(track) => track.net.as_ref(),
            Self::Arc(arc) => arc.net.as_ref(),
            Self::Graphic(graphic) => graphic.net.as_ref(),
        }
    }

    fn net_name(&self) -> &str {
        self.net()
            .map(|net| net.name.as_str())
            .filter(|name| !name.is_empty())
            .unwrap_or("no net")
    }

    /// A copy without its net, to compare everything else.
    fn without_net(&self) -> Self {
        let mut item = self.clone();
        match &mut item {
            Self::Via(via) => via.net = None,
            Self::Track(track) => track.net = None,
            Self::Arc(arc) => arc.net = None,
            Self::Graphic(graphic) => graphic.net = None,
            Self::Footprint(_) | Self::Text(_) | Self::TextBox(_) | Self::Zone(_) => {}
        }
        item
    }

    /// A copy without a zone's fill, which KiCad may rebuild when it pushes a
    /// commit with "Automatically refill zones" on (`pcbnew/board_commit.cpp:228-235`).
    fn without_fill(&self) -> Self {
        let mut item = self.clone();
        if let Self::Zone(zone) = &mut item {
            zone.filled = false;
            zone.filled_polygons.clear();
        }
        item
    }

    /// What KiCad must hold of a target after the push: all but the net of
    /// copper and of a graphic, which [`copper_changes`] compares, and a
    /// zone's fill.
    fn comparable(&self) -> Self {
        self.without_net().without_fill()
    }

    /// Carry it where `transform` puts it. `Some` is why it cannot follow;
    /// `Err` is a point it would put out of range. Copper and a text's
    /// position go by the exact point function: `PCB_VIA` has no `Rotate` of
    /// its own, `PCB_TRACK::Rotate` turns start and end, `PCB_ARC::Rotate`
    /// start, end and mid (`pcbnew/pcb_track.cpp:1104-1116`), and
    /// `PCB_TEXT::Rotate` the position, adding the turn to the angle in
    /// [0, 360) (`pcbnew/pcb_text.cpp:450-458`); KiCad always sends a text's
    /// angle (`common/eda_text.cpp:183-192`). A text box, a graphic and a zone
    /// go by `transform.rs`, as a footprint's children do; a text box's angle
    /// is then put in [0, 360), as `PCB_TEXTBOX::Rotate` leaves it. A text box
    /// or a rounded rectangle, or a footprint holding one, turns by an exact
    /// multiple of 90° only: KiCad 10.0.8 crashes on a text box at 45°, and a
    /// rounded rectangle has no exact form off its axes.
    fn carry(&mut self, transform: Transform, label: &str) -> Refusal<Option<String>> {
        let xform = transform.xform();
        let unfit = |result: anyhow::Result<()>| result.err().map(|e| format!("{label}: {e:#}"));
        if transform.degrees().rem_euclid(90.0) != 0.0 {
            if let Some(why) = self.turns_by_right_angles_only() {
                return Ok(Some(format!("{label}: {why}")));
            }
        }
        let refused = match self {
            Self::Footprint(footprint) => place(footprint, transform, label)?,
            Self::Via(via) => carry_points(vec![&mut via.position], transform, label)?,
            Self::Track(track) => {
                carry_points(vec![&mut track.start, &mut track.end], transform, label)?
            }
            Self::Arc(arc) => {
                let points = vec![&mut arc.start, &mut arc.mid, &mut arc.end];
                carry_points(points, transform, label)?
            }
            Self::Text(text) => match text.text.as_mut() {
                None => Some(format!("{label} has no position")),
                Some(text) => {
                    let refused = carry_points(vec![&mut text.position], transform, label)?;
                    if transform.degrees() != 0.0 {
                        let attrs = text.attributes.get_or_insert_with(Default::default);
                        let held = attrs.angle.map_or(0.0, |angle| angle.value_degrees);
                        attrs.angle = Some(Angle {
                            value_degrees: within_a_turn(held + transform.degrees()),
                        });
                    }
                    refused
                }
            },
            Self::TextBox(text_box) => {
                let refused = unfit(transform_board_text_box(text_box, &xform));
                if transform.degrees() != 0.0 {
                    let textbox = text_box.textbox.as_mut();
                    text_angle_within_a_turn(textbox.and_then(|tb| tb.attributes.as_mut()));
                }
                refused
            }
            Self::Graphic(graphic) => unfit(transform_board_graphic(graphic, &xform)),
            Self::Zone(zone) => {
                transform_zone(zone, &xform);
                None
            }
        };
        if refused.is_none() && !self.points().into_iter().all(in_range) {
            return Err(invalid_arg(transform.field(), OUT_OF_RANGE));
        }
        Ok(refused)
    }

    /// Why it turns by right angles only, if it does: a text box, a rounded
    /// rectangle, or a footprint holding either.
    fn turns_by_right_angles_only(&self) -> Option<String> {
        const TEXT_BOX_WHY: &str =
            "KiCad cannot hold a text box turned by an angle that is not a multiple of 90°";
        let rounded = |graphic: &BoardGraphicShape| {
            let Some(Geometry::Rectangle(rect)) = geometry(graphic) else {
                return None;
            };
            let layer = graphic.layer();
            rect.corner_radius.is_some_and(|r| r.value_nm > 0).then(|| {
                let name = copperpour_ipc::builders::layer_name(layer);
                format!(
                    "its rounded rectangle on {} has no exact form at a turn that is not a \
                     multiple of 90°",
                    name.unwrap_or(layer.as_str_name())
                )
            })
        };
        match self {
            Self::TextBox(_) => Some(TEXT_BOX_WHY.into()),
            Self::Graphic(graphic) => rounded(graphic),
            Self::Footprint(footprint) => children(footprint).find_map(|child| match child {
                Item::TextBox(_) => Some(format!("it holds a text box; {TEXT_BOX_WHY}")),
                Item::Graphic(graphic) => rounded(&graphic),
                _ => None,
            }),
            _ => None,
        }
    }

    /// Every point it has, to range-check after a move: a footprint's anchor
    /// and every point of its fields and children.
    fn points(&self) -> Vec<Vector2> {
        match self {
            Self::Footprint(footprint) => {
                let mut points: Vec<_> = footprint.position.into_iter().collect();
                for text in fields_of(footprint).filter_map(|field| field.text.as_ref()) {
                    points.extend(text.text.as_ref().and_then(|text| text.position));
                }
                for item in footprint.definition.iter().flat_map(|def| &def.items) {
                    let bytes = item.value.as_slice();
                    match any_type_name(item) {
                        PAD => points.extend(Pad::decode(bytes).ok().and_then(|pad| pad.position)),
                        FIELD => points.extend(Field::decode(bytes).ok().and_then(|field| {
                            field
                                .text
                                .and_then(|text| text.text)
                                .and_then(|text| text.position)
                        })),
                        REFERENCE_POINT => points.extend(
                            ReferencePoint::decode(bytes)
                                .ok()
                                .and_then(|point| point.position),
                        ),
                        _ => {}
                    }
                }
                points.extend(children(footprint).flat_map(|child| child.points()));
                points
            }
            Self::Via(via) => via.position.into_iter().collect(),
            Self::Track(track) => [track.start, track.end].into_iter().flatten().collect(),
            Self::Arc(arc) => [arc.start, arc.mid, arc.end]
                .into_iter()
                .flatten()
                .collect(),
            Self::Text(text) => text
                .text
                .as_ref()
                .and_then(|text| text.position)
                .into_iter()
                .collect(),
            Self::TextBox(text_box) => text_box
                .textbox
                .iter()
                .flat_map(|textbox| [textbox.top_left, textbox.bottom_right])
                .flatten()
                .collect(),
            Self::Graphic(graphic) => graphic_points(graphic),
            Self::Zone(zone) => zone
                .outline
                .iter()
                .chain(
                    zone.filled_polygons
                        .iter()
                        .filter_map(|fill| fill.shapes.as_ref()),
                )
                .flat_map(polyset_points)
                .collect(),
        }
    }

    /// Its row in a response, in mm and degrees.
    fn row(&self, id: &str) -> Value {
        let mut row = match self {
            Self::Footprint(footprint) => {
                let mut row = xy(footprint.position);
                row["reference"] = json!(footprint_reference(footprint));
                row["rotation"] = json!(orientation(footprint));
                row
            }
            Self::Via(via) => xy(via.position),
            Self::Track(track) => json!({"start": xy(track.start), "end": xy(track.end)}),
            Self::Arc(arc) => {
                json!({"start": xy(arc.start), "mid": xy(arc.mid), "end": xy(arc.end)})
            }
            Self::Text(text) => {
                let text = text.text.clone().unwrap_or_default();
                let mut row = xy(text.position);
                row["rotation"] = json!(angle_of(text.attributes.as_ref()));
                row
            }
            Self::TextBox(text_box) => {
                let textbox = text_box.textbox.clone().unwrap_or_default();
                json!({"top_left": xy(textbox.top_left), "bottom_right": xy(textbox.bottom_right),
                    "rotation": angle_of(textbox.attributes.as_ref())})
            }
            Self::Graphic(graphic) => graphic_row(graphic),
            Self::Zone(zone) => json!({
                "name": zone.name,
                "filled": zone.filled,
                "outline_point_count": zone.outline.iter()
                    .flat_map(|set| &set.polygons)
                    .filter_map(|polygon| polygon.outline.as_ref())
                    .map(|line| line.nodes.len())
                    .sum::<usize>(),
                "hole_count": zone.outline.iter()
                    .flat_map(|set| &set.polygons)
                    .map(|polygon| polygon.holes.len())
                    .sum::<usize>(),
            }),
        };
        row["uuid"] = json!(id);
        row
    }

    /// Where other copper can join it: a via's centre, a track's or arc's ends.
    fn ends(&self) -> Vec<Vector2> {
        match self {
            Self::Via(via) => via.position.into_iter().collect(),
            Self::Track(track) => track.start.into_iter().chain(track.end).collect(),
            Self::Arc(arc) => arc.start.into_iter().chain(arc.end).collect(),
            Self::Footprint(_)
            | Self::Text(_)
            | Self::TextBox(_)
            | Self::Graphic(_)
            | Self::Zone(_) => Vec::new(),
        }
    }
}

/// `points` moved by `transform`'s exact point function; `Some` when one is
/// absent.
fn carry_points(
    points: Vec<&mut Option<Vector2>>,
    transform: Transform,
    label: &str,
) -> Refusal<Option<String>> {
    for point in points {
        match point {
            Some(p) => *p = transform.point(*p)?,
            None => return Ok(Some(format!("{label} has no position"))),
        }
    }
    Ok(None)
}

/// A text's angle put in [0, 360), where `PCB_TEXT::Rotate` and
/// `PCB_TEXTBOX::Rotate` leave it.
fn text_angle_within_a_turn(attributes: Option<&mut TextAttributes>) {
    if let Some(angle) = attributes.and_then(|attrs| attrs.angle.as_mut()) {
        angle.value_degrees = within_a_turn(angle.value_degrees);
    }
}

fn angle_of(attributes: Option<&TextAttributes>) -> f64 {
    attributes
        .and_then(|attrs| attrs.angle)
        .map_or(0.0, |angle| angle.value_degrees)
}

/// Every point of a polygon set: its outlines' and holes' nodes, an arc node
/// by its start, mid and end.
fn polyset_points(set: &PolySet) -> Vec<Vector2> {
    set.polygons
        .iter()
        .flat_map(|polygon| polygon.outline.iter().chain(&polygon.holes))
        .flat_map(|line| &line.nodes)
        .flat_map(|node| match node.geometry {
            Some(poly_line_node::Geometry::Point(p)) => vec![p],
            Some(poly_line_node::Geometry::Arc(arc)) => [arc.start, arc.mid, arc.end]
                .into_iter()
                .flatten()
                .collect(),
            None => Vec::new(),
        })
        .collect()
}

/// The geometry of a graphic, as it names its defining points.
fn geometry(graphic: &BoardGraphicShape) -> Option<&Geometry> {
    graphic
        .shape
        .as_ref()
        .and_then(|shape| shape.geometry.as_ref())
}

fn graphic_points(graphic: &BoardGraphicShape) -> Vec<Vector2> {
    let points = match geometry(graphic) {
        Some(Geometry::Segment(s)) => vec![s.start, s.end],
        Some(Geometry::Rectangle(r)) => vec![r.top_left, r.bottom_right],
        Some(Geometry::Arc(a)) => vec![a.start, a.mid, a.end],
        Some(Geometry::Circle(c)) => vec![c.center, c.radius_point],
        Some(Geometry::Polygon(set)) => return polyset_points(set),
        Some(Geometry::Bezier(b)) => vec![b.start, b.control1, b.control2, b.end],
        None => Vec::new(),
    };
    points.into_iter().flatten().collect()
}

/// A graphic's row: its shape and defining points, a polygon by its count of
/// outline and hole nodes.
fn graphic_row(graphic: &BoardGraphicShape) -> Value {
    match geometry(graphic) {
        Some(Geometry::Segment(s)) => {
            json!({"shape": "segment", "start": xy(s.start), "end": xy(s.end)})
        }
        Some(Geometry::Rectangle(r)) => json!({"shape": "rectangle",
            "top_left": xy(r.top_left), "bottom_right": xy(r.bottom_right)}),
        Some(Geometry::Arc(a)) => json!({"shape": "arc",
            "start": xy(a.start), "mid": xy(a.mid), "end": xy(a.end)}),
        Some(Geometry::Circle(c)) => json!({"shape": "circle",
            "center": xy(c.center), "radius_point": xy(c.radius_point)}),
        Some(Geometry::Polygon(set)) => {
            let lines = set
                .polygons
                .iter()
                .flat_map(|p| p.outline.iter().chain(&p.holes));
            json!({"shape": "polygon", "point_count": lines.map(|l| l.nodes.len()).sum::<usize>()})
        }
        Some(Geometry::Bezier(b)) => json!({"shape": "bezier", "start": xy(b.start),
            "control1": xy(b.control1), "control2": xy(b.control2), "end": xy(b.end)}),
        None => json!({"shape": null}),
    }
}

fn pads_of(footprint: &FootprintInstance) -> anyhow::Result<Vec<Pad>> {
    footprint
        .definition
        .iter()
        .flat_map(|definition| &definition.items)
        .filter(|item| any_is(item, PAD))
        .map(|item| Ok(Pad::decode(item.value.as_slice())?))
        .collect()
}

/// A footprint's four mandatory fields.
fn fields_of(footprint: &FootprintInstance) -> impl Iterator<Item = &Field> {
    [
        &footprint.reference_field,
        &footprint.value_field,
        &footprint.datasheet_field,
        &footprint.description_field,
    ]
    .into_iter()
    .flatten()
}

/// A footprint's texts, text boxes, graphics and zones, as [`Item`]s.
fn children(footprint: &FootprintInstance) -> impl Iterator<Item = Item> + '_ {
    let items = footprint.definition.iter().flat_map(|def| &def.items);
    items.filter_map(|item| Item::decode(item).ok().flatten())
}

/// The UUID of every child of `footprint` `BOARD::ResolveItem` finds by its
/// own: its pads, fields, texts, text boxes, graphics, zones and points
/// (`pcbnew/board.cpp:1899-1937`).
fn child_ids(footprint: &FootprintInstance) -> Vec<String> {
    let field_id = |field: &Field| field.text.as_ref().and_then(|text| text.id.clone());
    let mut ids: Vec<_> = fields_of(footprint).filter_map(field_id).collect();
    for item in footprint.definition.iter().flat_map(|def| &def.items) {
        let bytes = item.value.as_slice();
        ids.extend(match any_type_name(item) {
            PAD => Pad::decode(bytes).ok().and_then(|pad| pad.id),
            FIELD => Field::decode(bytes).ok().as_ref().and_then(field_id),
            REFERENCE_POINT => ReferencePoint::decode(bytes)
                .ok()
                .and_then(|point| point.id),
            _ => Item::decode(item)
                .ok()
                .flatten()
                .map(|child| Kiid { value: child.id() }),
        });
    }
    ids.into_iter().map(|id| id.value).collect()
}

fn orientation(footprint: &FootprintInstance) -> f64 {
    footprint
        .orientation
        .as_ref()
        .map_or(0.0, |angle| angle.value_degrees)
}

/// The items of `kinds` KiCad holds, in one request.
fn read_board(
    client: &KiCadIpcClient,
    document: &DocumentSpecifier,
    kinds: &[Kot],
) -> anyhow::Result<Items> {
    let mut board = Items::new();
    for any in client.get_items_of_types_in(document.clone(), kinds)? {
        let item = Item::decode(&any)?.ok_or_else(|| {
            anyhow::anyhow!("KiCad listed an unrequested {}", any_type_name(&any))
        })?;
        let id = item.id();
        anyhow::ensure!(!id.is_empty(), "a listed {} has no UUID", item.kind());
        anyhow::ensure!(
            board.insert(id.clone(), item).is_none(),
            "UUID '{id}' listed twice"
        );
    }
    Ok(board)
}

/// Every requested UUID exactly once, each an [`Item`].
fn read_targets(
    client: &KiCadIpcClient,
    document: &DocumentSpecifier,
    ids: &[String],
) -> anyhow::Result<Items> {
    let wanted: BTreeSet<_> = ids.iter().map(String::as_str).collect();
    let mut targets = Items::new();
    for any in client.get_items_by_id_in(document.clone(), ids)? {
        let Some(item) = Item::decode(&any)? else {
            anyhow::bail!("a requested item is a {}", any_type_name(&any))
        };
        let id = item.id();
        anyhow::ensure!(wanted.contains(id.as_str()), "unrequested item '{id}' read");
        anyhow::ensure!(
            targets.insert(id, item).is_none(),
            "an item UUID read twice"
        );
    }
    anyhow::ensure!(
        targets.len() == wanted.len(),
        "requested item UUIDs are missing"
    );
    Ok(targets)
}

/// What the reads say about the selection.
struct Resolved {
    /// The targets as the board holds them.
    targets: Items,
    /// Every reason to refuse them.
    reasons: Vec<String>,
    /// The groups the targets touch, as KiCad sent them, each before the
    /// groups inside it.
    touched: Vec<Group>,
    /// The UUID of every member of every board group.
    grouped: HashSet<String>,
}

/// The targets as `board` holds them, a group's UUID standing for every
/// member through its member groups; every reason the reads give to refuse
/// them; the groups they touch; and the board's group members. KiCad's
/// update path checks no lock (`pcbnew/api/api_handler_pcb.cpp:359-557`);
/// its delete path says locks are not supported (781-782). A footprint is
/// rebuilt from a message that carries none of its own groups (#701). A
/// footprint's own child, a pad say, travels with its footprint, so it is
/// no target of its own.
fn resolve(
    client: &KiCadIpcClient,
    document: &DocumentSpecifier,
    contents: &str,
    request: &Request,
    board: &Items,
) -> anyhow::Result<Resolved> {
    let mut targets = Items::new();
    let mut reasons = Vec::new();
    let groups = read_groups(client, document).unwrap_or_else(|error| {
        reasons.push(format!("group membership could not be read: {error:#}"));
        Groups::new()
    });
    let mut by_reference = HashSet::new();
    for reference in &request.references {
        match holders(board, reference)[..] {
            [id] => {
                targets.insert(id.clone(), board[id].clone());
                by_reference.insert(id.clone());
            }
            [] => reasons.push(format!("footprint {reference} not found on the board")),
            _ => reasons.push(format!(
                "footprint reference '{reference}' is held more than once on the board"
            )),
        }
    }
    let (mut leftovers, mut named) = (Vec::new(), Vec::new());
    for id in &request.uuids {
        if let Some(item) = board.get(id) {
            if by_reference.contains(id) {
                reasons.push(format!(
                    "{} is selected both by reference and by UUID",
                    item.label(id)
                ));
            }
            targets.insert(id.clone(), item.clone());
        } else if groups.contains_key(id) {
            named.push(id.clone());
            // An item reached twice, by its UUID or through two groups, is one target.
            for member in members_within(&groups, id) {
                if let Some(item) = board.get(&member) {
                    targets.entry(member).or_insert_with(|| item.clone());
                }
            }
        } else {
            leftovers.push(id);
        }
    }
    for (id, item) in &targets {
        let Item::Footprint(footprint) = item else {
            continue;
        };
        let reference = footprint_reference(footprint);
        if !by_reference.contains(id)
            && (reference.is_empty() || holders(board, reference).len() > 1)
        {
            reasons.push(format!(
                "{} has an empty or shared reference; the copper check addresses footprints by \
                 reference",
                item.label(id)
            ));
        }
    }
    // The board text is read once: for a footprint's groups, a zone's lock,
    // a hatch the API would drop and the generated blocks, a tuning pattern
    // among them.
    let needed = contents.contains("(generated")
        || targets
            .values()
            .any(|item| matches!(item, Item::Footprint(_) | Item::Zone(_) | Item::Graphic(_)));
    let tree = match needed.then(|| copperpour_sexp::parse_sexp(contents)) {
        Some(Ok(tree)) => Some(tree),
        Some(Err(error)) => {
            reasons.push(format!("the board text could not be read: {error}"));
            None
        }
        None => None,
    };
    let generated = tree.as_ref().map(generated_blocks).unwrap_or_default();
    // Asked one at a time, so each answer names its UUID. KiCad parses a
    // UUID's text into its value (`common/kiid.cpp:135-139`), so it may find
    // an item the listing holds under another spelling.
    for id in leftovers {
        if let Some(block) = generated.iter().find(|block| block.id == *id) {
            reasons.push(block.refusal(None));
            continue;
        }
        let found = client.get_items_by_id_in(document.clone(), std::slice::from_ref(id))?;
        let held = found
            .first()
            .and_then(|item| Item::decode(item).ok().flatten());
        let held = held
            .map(|item| item.id())
            .filter(|held| board.contains_key(held));
        let owner = board.iter().find(|(_, item)| {
            matches!(item, Item::Footprint(footprint)
                if child_ids(footprint).iter().any(|child| child.eq_ignore_ascii_case(id)))
        });
        reasons.extend(match (&found[..], held, owner) {
            ([], _, _) => Some(format!("item '{id}' not found on the board")),
            ([_], Some(held), _) => Some(format!(
                "item '{id}' is {}; give its UUID as KiCad writes it",
                board[&held].label(&held)
            )),
            ([_], None, Some((owner, _))) if targets.contains_key(owner) => None,
            ([item], None, Some((owner, footprint))) => Some(format!(
                "{} '{id}' belongs to {}; select the footprint to move it",
                short_type(item),
                footprint.label(owner)
            )),
            ([item], None, None) => Some(format!(
                "item '{id}' is a {}, which this tool does not move",
                short_type(item)
            )),
            (many, _, _) => Some(format!(
                "KiCad holds {} items under UUID '{id}'",
                many.len()
            )),
        });
    }
    for (id, item) in &targets {
        let label = item.label(id);
        if let Some(block) = generated.iter().find(|block| block.members.contains(id)) {
            reasons.push(block.refusal(Some(&label)));
        }
        if let Some(block) = tree.as_ref().and_then(|tree| top_level_block(tree, id)) {
            let losses = api_losses(block).into_iter();
            reasons.extend(losses.map(|loss| format!("{label}: {loss}")));
        }
        let locked = match (item, &tree) {
            (Item::Zone(zone), _) if is_generated_teardrop(zone) => {
                reasons.push(format!(
                    "{label} is a teardrop zone KiCad generates and rebuilds itself"
                ));
                continue;
            }
            (Item::Zone(_), Some(tree)) => zone_locked(tree, id),
            // The board text could not be read, which is a reason already.
            (Item::Zone(_), None) => continue,
            (other, _) => Some(other.locked() != LockedState::LsUnlocked as i32),
        };
        match locked {
            Some(false) => {}
            Some(true) => reasons.push(format!("{label} is locked or its lock state is unknown")),
            None => reasons.push(format!(
                "{label} is absent from the board text, so its lock state is unknown"
            )),
        }
    }
    let touched = touched_groups(&groups, &targets, &named);
    for group in &touched {
        reasons.extend(group_reasons(
            client, document, group, &groups, &targets, board, &generated,
        )?);
    }
    if let Some(tree) = &tree {
        reasons.extend(owned_group_reasons(tree, &targets));
    }
    let grouped = groups
        .values()
        .flat_map(|group| group.items.iter().map(|member| member.value.clone()))
        .collect();
    Ok(Resolved {
        targets,
        reasons,
        touched: touched.into_iter().cloned().collect(),
        grouped,
    })
}

/// The board's groups by UUID, as `GetItems` lists them; KiCad does not list
/// a tuning pattern among them.
fn read_groups(client: &KiCadIpcClient, document: &DocumentSpecifier) -> anyhow::Result<Groups> {
    let mut groups = Groups::new();
    for any in client.get_items_of_types_in(document.clone(), &[Kot::KotPcbGroup])? {
        anyhow::ensure!(
            any_is(&any, GROUP),
            "KiCad listed a {} as a group",
            any_type_name(&any)
        );
        let group = Group::decode(any.value.as_slice())?;
        let id = group_id(&group);
        anyhow::ensure!(!id.is_empty(), "a listed group has no UUID");
        anyhow::ensure!(
            groups.insert(id.clone(), group).is_none(),
            "group UUID '{id}' listed twice"
        );
    }
    Ok(groups)
}

fn group_id(group: &Group) -> String {
    group
        .id
        .as_ref()
        .map(|id| id.value.clone())
        .unwrap_or_default()
}

fn group_label(group: &Group) -> String {
    format!("group '{}' ({})", group.name, group_id(group))
}

/// The last part of an item's type name: `Dimension`, say.
fn short_type(any: &prost_types::Any) -> &str {
    any_type_name(any).rsplit('.').next().unwrap_or_default()
}

/// Every member of group `id` that is not a group, through its member
/// groups.
fn members_within(groups: &Groups, id: &str) -> Vec<String> {
    let (mut open, mut seen, mut members) = (vec![id.to_string()], HashSet::new(), Vec::new());
    while let Some(id) = open.pop() {
        if !seen.insert(id.clone()) {
            continue;
        }
        for member in groups.get(&id).into_iter().flat_map(|group| &group.items) {
            if groups.contains_key(&member.value) {
                open.push(member.value.clone());
            } else {
                members.push(member.value.clone());
            }
        }
    }
    members
}

/// The groups `seed` names, and those of which `holds` is true: of every
/// member (`all`) or of some member, a target or a group found.
fn groups_where(groups: &Groups, targets: &Items, seed: &[String], all: bool) -> HashSet<String> {
    let mut found: HashSet<String> = seed.iter().cloned().collect();
    loop {
        let counts =
            |member: &Kiid| targets.contains_key(&member.value) || found.contains(&member.value);
        let more: Vec<String> = groups
            .iter()
            .filter(|(id, group)| {
                !found.contains(*id)
                    && if all {
                        group.items.iter().all(counts)
                    } else {
                        group.items.iter().any(counts)
                    }
            })
            .map(|(id, _)| id.clone())
            .collect();
        if more.is_empty() {
            return found;
        }
        found.extend(more);
    }
}

/// The groups the request touches: those it `named`, whatever they hold, and
/// those with a target or a touched group among their members, each before
/// the groups inside it.
fn touched_groups<'a>(groups: &'a Groups, targets: &Items, named: &[String]) -> Vec<&'a Group> {
    let mut parent = HashMap::new();
    for (id, group) in groups {
        for member in &group.items {
            if groups.contains_key(&member.value) {
                parent.insert(member.value.clone(), id.clone());
            }
        }
    }
    let mut order = Vec::new();
    for id in groups_where(groups, targets, named, false) {
        let (mut up, mut depth) = (&id, 0);
        while let Some(next) = parent.get(up).filter(|_| depth <= groups.len()) {
            (up, depth) = (next, depth + 1);
        }
        order.push((depth, id));
    }
    order.sort();
    order.into_iter().map(|(_, id)| &groups[&id]).collect()
}

/// Why touched `group` cannot move: it is locked, holds nothing, or a member
/// is left out. KiCad's undo of a commit that sent a group with only some of
/// its members moved leaves the group holding only those (measured on KiCad
/// 10.0.8), so every touched group must move whole.
fn group_reasons(
    client: &KiCadIpcClient,
    document: &DocumentSpecifier,
    group: &Group,
    groups: &Groups,
    targets: &Items,
    board: &Items,
    generated: &[Generated],
) -> anyhow::Result<Vec<String>> {
    const SHOWN: usize = 5;
    let label = group_label(group);
    let mut reasons = Vec::new();
    if group.locked != LockedState::LsUnlocked as i32 {
        reasons.push(format!("{label} is locked or its lock state is unknown"));
    }
    if group.items.is_empty() {
        reasons.push(format!("{label} holds nothing"));
    }
    let whole = groups_where(groups, targets, &[], true);
    let mut left_out = Vec::new();
    for member in group.items.iter().map(|member| &member.value) {
        if targets.contains_key(member) || whole.contains(member) {
            continue;
        }
        if board.contains_key(member) || groups.contains_key(member) {
            left_out.push(member.clone());
            continue;
        }
        let kind = match generated.iter().find(|block| block.id == *member) {
            Some(block) => block.kind.clone(),
            None => {
                let found =
                    client.get_items_by_id_in(document.clone(), std::slice::from_ref(member))?;
                found
                    .first()
                    .map_or("item KiCad does not list".into(), |any| {
                        short_type(any).to_string()
                    })
            }
        };
        reasons.push(format!(
            "{label} holds '{member}', a {kind}, which this tool does not move"
        ));
    }
    if !left_out.is_empty() {
        let more = left_out.len().saturating_sub(SHOWN);
        let mut shown = left_out[..left_out.len().min(SHOWN)].join(", ");
        if more > 0 {
            shown.push_str(&format!(" and {more} more"));
        }
        reasons.push(format!(
            "{label} would move only in part, leaving out {} of its {} members ({shown}); give \
             the group's UUID to move it whole",
            left_out.len(),
            group.items.len()
        ));
    }
    Ok(reasons)
}

/// A `(generated …)` block of the board text: a tuning pattern, say.
struct Generated {
    id: String,
    /// Its `type`, words apart: "tuning pattern".
    kind: String,
    members: HashSet<String>,
}

impl Generated {
    /// The refusal of a target in it (`member`, by its label), or of the
    /// block itself. Moving a pattern's member in place leaves the block, its
    /// base line and origin included, as it was (measured on KiCad 10.0.8).
    fn refusal(&self, member: Option<&str>) -> String {
        let what = match member {
            Some(label) => format!("{label} is a member of {} '{}'", self.kind, self.id),
            None => format!("'{}' is a {}", self.id, self.kind),
        };
        format!("{what}; KiCad's API moves the pattern's tracks but not its base line")
    }
}

/// The generated blocks of the board text. `GetItemsById` answers a tuning
/// pattern's UUID with a group that `GetItems` does not list (measured on
/// KiCad 10.0.8), so the board text is where they are read.
fn generated_blocks(tree: &SexpNode) -> Vec<Generated> {
    tree.find_all("generated")
        .into_iter()
        .map(|block| Generated {
            id: block.find_str("uuid").unwrap_or_default().to_string(),
            kind: block
                .find_str("type")
                .unwrap_or("generated block")
                .replace('_', " "),
            members: block
                .find("members")
                .and_then(SexpNode::children)
                .into_iter()
                .flatten()
                .skip(1)
                .filter_map(SexpNode::as_str)
                .map(str::to_string)
                .collect(),
        })
        .collect()
}

/// The UUIDs of the footprints `board` holds under `reference`.
fn holders<'a>(board: &'a Items, reference: &str) -> Vec<&'a String> {
    board
        .iter()
        .filter(|(_, item)| {
            matches!(item, Item::Footprint(footprint) if footprint_reference(footprint) == reference)
        })
        .map(|(id, _)| id)
        .collect()
}

/// The board text's top-level block of item `id`.
fn top_level_block<'a>(tree: &'a SexpNode, id: &str) -> Option<&'a SexpNode> {
    let mut blocks = tree.children().unwrap_or_default().iter();
    blocks.rfind(|block| block.find_str("uuid") == Some(id))
}

/// What KiCad's API would drop from an item whose board text is `block`, a
/// footprint's children's included (measured on KiCad 10.0.8: an item sent
/// back moved, otherwise as read, loses these and keeps every other token).
/// A graphic's hatch fill (`(fill hatch)`, `reverse_hatch` or `cross_hatch`)
/// is read back unfilled and written `(fill no)`; a zone filled with a
/// hatch loses its `hatch_smoothing_level` and `hatch_smoothing_value`, for
/// which the message has no field.
fn api_losses(block: &SexpNode) -> Vec<String> {
    const HATCHES: [&str; 3] = ["hatch", "reverse_hatch", "cross_hatch"];
    fn holds(node: &SexpNode, head: &str) -> bool {
        node.head() == Some(head)
            || node
                .children()
                .unwrap_or_default()
                .iter()
                .any(|child| holds(child, head))
    }
    let loss = |node: &SexpNode| -> Option<&'static str> {
        let head = node.head()?;
        let graphic = head.starts_with("gr_") || head.starts_with("fp_");
        if graphic
            && node
                .find_str("fill")
                .is_some_and(|fill| HATCHES.contains(&fill))
        {
            Some("KiCad's API does not carry a hatch fill; the update would drop it")
        } else if head == "zone" && holds(node, "hatch_smoothing_level") {
            Some("KiCad's API does not carry the zone's hatch smoothing; the update would drop it")
        } else {
            None
        }
    };
    let own = loss(block).map(str::to_string);
    let held = (block.head() == Some("footprint")).then(|| {
        let children = block.children().unwrap_or_default().iter();
        children.filter_map(move |child| {
            let what = loss(child)?;
            let id = child.find_str("uuid").unwrap_or_default();
            Some(format!(
                "its {} '{id}': {what}",
                child.head().unwrap_or_default()
            ))
        })
    });
    own.into_iter().chain(held.into_iter().flatten()).collect()
}

/// Whether a zone is a teardrop, which KiCad's API sends as `ZT_TEARDROP`
/// (`pcbnew/zone.cpp:260-261`).
fn is_generated_teardrop(zone: &Zone) -> bool {
    zone.r#type == ZoneType::ZtTeardrop as i32
}

/// Whether the board text holds zone `id` locked; `None` when it holds no
/// zone `id`. `ZONE::Serialize` sends no lock state
/// (`pcbnew/zone.cpp:249-354`); the board text writes `(locked yes)`
/// for a locked zone (`pcbnew/pcb_io/kicad_sexpr/pcb_io_kicad_sexpr.cpp:2877-2878`).
fn zone_locked(tree: &SexpNode, id: &str) -> Option<bool> {
    tree.find_all("zone")
        .into_iter()
        .find(|zone| zone.find_str("uuid") == Some(id))
        .map(|zone| zone.find_str("locked") == Some("yes"))
}

/// KiCad's footprint message carries none of a footprint's own groups, so an
/// update drops them (#701): refuse a footprint that owns one, or that the
/// board text does not account for.
fn owned_group_reasons(tree: &SexpNode, targets: &Items) -> Vec<String> {
    let footprints: Vec<_> = targets
        .iter()
        .filter(|(_, item)| matches!(item, Item::Footprint(_)))
        .collect();
    if footprints.is_empty() {
        return Vec::new();
    }
    let owned = match copperpour_sexp::board::footprint_owned_group_counts(tree) {
        Ok(owned) => owned,
        Err(error) => {
            return vec![format!(
                "could not verify footprint group ownership: {error}"
            )]
        }
    };
    footprints
        .into_iter()
        .filter_map(|(id, item)| match owned.get(id.as_str()) {
            None => Some(format!(
                "{} is absent from the board serialization",
                item.label(id)
            )),
            Some(0) => None,
            Some(count) => Some(format!(
                "{} owns {count} group(s), which the whole-footprint update a move needs would \
                 drop; move it in KiCad, or ungroup them first",
                item.label(id)
            )),
        })
        .collect()
}

/// Each target where `transform` puts it, on a copy of the message KiCad
/// sent, so every native property round-trips. A target that cannot follow
/// adds its reason; a destination out of range is the `Err`, once every
/// target was planned.
fn plan(targets: &Items, transform: Transform, reasons: &mut Vec<String>) -> Refusal<Items> {
    let mut after = targets.clone();
    let mut out_of_range = None;
    for (id, item) in &mut after {
        let label = item.label(id);
        match item.carry(transform, &label) {
            Ok(reason) => reasons.extend(reason),
            Err(refusal) => {
                out_of_range.get_or_insert(refusal);
            }
        }
    }
    out_of_range.map_or(Ok(after), Err)
}

/// Carry `footprint` to what KiCad's `FOOTPRINT::Rotate` gives
/// (`pcbnew/footprint.cpp:2837-2856`). The code turns the children about
/// the held anchor and then moves them with the anchor to where the turn
/// puts it; KiCad moves first and turns about the new anchor, and in
/// integers the two are one result. Then, on a turn, its keep-upright texts
/// are turned upright and every pad, field, text and text box angle is put
/// in [0, 360), where `PADSTACK::SetOrientation` (`pcbnew/padstack.h:344-348`)
/// and `PCB_TEXT::Rotate` leave them. `Some` is why one cannot follow.
fn place(
    footprint: &mut FootprintInstance,
    transform: Transform,
    label: &str,
) -> Refusal<Option<String>> {
    let Some(anchor) = footprint.position else {
        return Ok(Some(format!("{label} has no position")));
    };
    let position = transform.point(anchor)?;
    let rotation = orientation(footprint) + transform.degrees();
    // The transforms `placement_xforms` gives, with the turn exact.
    let xforms = [
        (transform.degrees() != 0.0).then_some(Xform::Rotate {
            cx_nm: anchor.x_nm,
            cy_nm: anchor.y_nm,
            delta_deg: transform.degrees(),
        }),
        (position != anchor).then_some(Xform::Translate {
            dx_nm: position.x_nm - anchor.x_nm,
            dy_nm: position.y_nm - anchor.y_nm,
        }),
    ];
    let mut carried = xforms
        .iter()
        .flatten()
        .try_for_each(|xform| transform_footprint_children(footprint, xform));
    if transform.degrees() != 0.0 {
        carried = carried
            .and_then(|()| keep_footprint_text_upright(footprint))
            .and_then(|()| angles_within_a_turn(footprint));
        footprint.orientation = Some(Angle {
            value_degrees: normalize_deg(rotation),
        });
    }
    if let Err(error) = carried {
        return Ok(Some(PreflightRefusal::from_error(&error).map_or_else(
            || format!("{label} cannot be carried: {error:#}"),
            |refusal| refusal.reason.clone(),
        )));
    }
    footprint.position = Some(position);
    Ok(None)
}

/// Every pad, field, text and text box angle of `footprint` in [0, 360).
fn angles_within_a_turn(footprint: &mut FootprintInstance) -> anyhow::Result<()> {
    let text = |text: Option<&mut BoardText>| {
        let text = text.and_then(|text| text.text.as_mut());
        text_angle_within_a_turn(text.and_then(|text| text.attributes.as_mut()));
    };
    for field in [
        &mut footprint.reference_field,
        &mut footprint.value_field,
        &mut footprint.datasheet_field,
        &mut footprint.description_field,
    ] {
        text(field.as_mut().and_then(|field| field.text.as_mut()));
    }
    let items = footprint
        .definition
        .iter_mut()
        .flat_map(|definition| &mut definition.items);
    for item in items {
        let bytes = item.value.as_slice();
        item.value = match any_type_name(item) {
            PAD => {
                let mut pad = Pad::decode(bytes)?;
                let stack = pad.pad_stack.as_mut();
                if let Some(angle) = stack.and_then(|stack| stack.angle.as_mut()) {
                    angle.value_degrees = within_a_turn(angle.value_degrees);
                }
                pad.encode_to_vec()
            }
            FIELD => {
                let mut field = Field::decode(bytes)?;
                text(field.text.as_mut());
                field.encode_to_vec()
            }
            TEXT => {
                let mut board_text = BoardText::decode(bytes)?;
                text(Some(&mut board_text));
                board_text.encode_to_vec()
            }
            TEXT_BOX => {
                let mut text_box = BoardTextBox::decode(bytes)?;
                let textbox = text_box.textbox.as_mut();
                text_angle_within_a_turn(textbox.and_then(|tb| tb.attributes.as_mut()));
                text_box.encode_to_vec()
            }
            _ => continue,
        };
    }
    Ok(())
}

/// Unselected copper and pads the rigid move leaves behind, on the current
/// board: a track end, arc end or via centre exactly on a selected via's
/// centre, track or arc end, or pad, and a pad of an unselected footprint
/// exactly on a selected track or arc end or via centre, each on the same
/// named net. Rows are sorted by kind, UUID and position.
fn boundary(board: &Items, targets: &Items) -> anyhow::Result<Vec<Value>> {
    let key = |p: Vector2, net: Option<&Net>| {
        net.map(|net| net.name.clone())
            .filter(|name| !name.is_empty())
            .map(|name| (p.x_nm, p.y_nm, name))
    };
    let (mut copper, mut pads) = (HashSet::new(), HashSet::new());
    for item in targets.values() {
        match item {
            Item::Footprint(footprint) => {
                for pad in pads_of(footprint)? {
                    pads.extend(pad.position.and_then(|p| key(p, pad.net.as_ref())));
                }
            }
            copper_item => copper.extend(
                copper_item
                    .ends()
                    .into_iter()
                    .filter_map(|p| key(p, copper_item.net())),
            ),
        }
    }
    let mut rows = BTreeMap::new();
    let row = |kind, uuid: String, (x, y, net): (i64, i64, String)| {
        let value = json!({"kind": kind, "uuid": uuid, "net": net,
            "x": nm_to_mm(x), "y": nm_to_mm(y)});
        ((kind, uuid, x, y), value)
    };
    for (id, item) in board.iter().filter(|(id, _)| !targets.contains_key(*id)) {
        if let Item::Footprint(footprint) = item {
            for pad in pads_of(footprint)? {
                let Some(touch) = pad.position.and_then(|p| key(p, pad.net.as_ref())) else {
                    continue;
                };
                if copper.contains(&touch) {
                    let pad_id = pad.id.map(|id| id.value).unwrap_or_default();
                    let (order, mut value) = row("pad", pad_id, touch);
                    value["reference"] = json!(footprint_reference(footprint));
                    value["pad"] = json!(pad.number);
                    rows.insert(order, value);
                }
            }
        }
        for touch in item.ends().into_iter().filter_map(|p| key(p, item.net())) {
            if copper.contains(&touch) || pads.contains(&touch) {
                let (order, value) = row(item.kind(), id.clone(), touch);
                rows.insert(order, value);
            }
        }
    }
    Ok(rows.into_values().collect())
}

fn revision(
    document: &DocumentSpecifier,
    contents: &str,
    (before, after): (&Items, &Items),
    touched: &[Group],
) -> String {
    let mut hash = Sha256::new();
    for bytes in std::iter::once(b"transform_pcb_items/1".to_vec())
        .chain([document.encode_to_vec(), contents.as_bytes().to_vec()])
        .chain(
            before
                .values()
                .chain(after.values())
                .map(|item| item.pack().encode_to_vec()),
        )
        .chain(touched.iter().map(Message::encode_to_vec))
    {
        hash.update((bytes.len() as u64).to_le_bytes());
        hash.update(bytes);
    }
    format!("sha256:{}", encode_hex(&hash.finalize()))
}

fn xy(p: Option<Vector2>) -> Value {
    let p = p.unwrap_or_default();
    json!({"x": nm_to_mm(p.x_nm), "y": nm_to_mm(p.y_nm)})
}

/// The targets' rows by kind: `footprints`, `vias`, ... with
/// `footprint_count`, `via_count`, ..., every kind listed; and the touched
/// `groups`, each as `seen` holds it if it holds it.
fn rows(targets: &Items, touched: &[Group], seen: Option<&Groups>) -> Value {
    let mut body = json!({});
    for (kind, plural) in Item::KINDS {
        let rows: Vec<_> = targets
            .iter()
            .filter(|(_, item)| item.kind() == kind)
            .map(|(id, item)| item.row(id))
            .collect();
        body[format!("{kind}_count")] = json!(rows.len());
        body[plural] = json!(rows);
    }
    let groups: Vec<_> = touched
        .iter()
        .map(|group| {
            let id = group_id(group);
            let group = seen.and_then(|seen| seen.get(&id)).unwrap_or(group);
            json!({"uuid": id, "name": group.name, "member_count": group.items.len()})
        })
        .collect();
    body["group_count"] = json!(groups.len());
    body["groups"] = json!(groups);
    body
}

/// The order one `UpdateItems` needs, measured on KiCad 10.0.8: every item
/// updated in place (`extra` among them), then each touched group as read, a
/// group before the groups inside it, then every footprint. An in-place
/// update detaches an item from its group in the running editor and the
/// group sent after it puts it back; a group sent before its in-place
/// members, an inner group before its outer, or a footprint before its group
/// leaves a group broken.
fn update_order(
    items: &Items,
    extra: Vec<prost_types::Any>,
    touched: &[Group],
) -> Vec<prost_types::Any> {
    let (footprints, in_place): (Vec<&Item>, Vec<&Item>) = items
        .values()
        .partition(|item| matches!(item, Item::Footprint(_)));
    in_place
        .into_iter()
        .map(Item::pack)
        .chain(extra)
        .chain(touched.iter().map(|group| pack_any(group, GROUP)))
        .chain(footprints.into_iter().map(Item::pack))
        .collect()
}

/// Whether KiCad holds every touched group as it was read: its UUID, name,
/// lock and members, in any order, since KiCad lists them in an order of its
/// own after an update (measured on KiCad 10.0.8).
fn groups_hold(seen: &Groups, touched: &[Group]) -> bool {
    let members = |group: &Group| -> BTreeSet<String> {
        group
            .items
            .iter()
            .map(|member| member.value.clone())
            .collect()
    };
    touched.iter().all(|was| {
        seen.get(&group_id(was)).is_some_and(|held| {
            held.name == was.name && held.locked == was.locked && members(held) == members(was)
        })
    })
}

/// The targets KiCad does not hold as `planned` has them, by label: each is
/// compared whole but for the net of copper and of a graphic, which
/// [`copper_changes`] compares, and a zone's fill.
fn targets_differing(seen: &Items, planned: &Items) -> Vec<String> {
    planned
        .iter()
        .filter(|(id, plan)| {
            seen.get(*id)
                .is_none_or(|held| held.comparable() != plan.comparable())
        })
        .map(|(id, plan)| plan.label(id))
        .collect()
}

/// How the copper `seen` differs from the copper `expected`: the UUIDs whose
/// net changed, and those that appeared, vanished, or, being one of
/// `targets`, changed besides their net. KiCad re-nets tracks, arcs and vias
/// across the whole board when a commit is pushed
/// (`pcbnew/connectivity/connectivity_algo.cpp:665-717`,
/// `pcbnew/board_commit.cpp:531-550`); pads keep theirs. Tracks, vias and
/// arcs, and a selected graphic, are compared here by net, and a net change
/// is restored; any other change KiCad makes, to an unselected copper
/// graphic's net say, is found in the board text ([`board_text_changes`]).
fn copper_changes(expected: &Items, seen: &Items, targets: &Items) -> (Vec<String>, Vec<String>) {
    let (mut renetted, mut changed) = (Vec::new(), Vec::new());
    for id in expected.keys().chain(seen.keys()).collect::<BTreeSet<_>>() {
        match (expected.get(id), seen.get(id)) {
            (Some(a), Some(b))
                if targets.contains_key(id) && a.without_net() != b.without_net() =>
            {
                changed.push(id.clone())
            }
            (Some(a), Some(b)) if a.net() != b.net() => renetted.push(id.clone()),
            (Some(_), Some(_)) => {}
            _ => changed.push(id.clone()),
        }
    }
    (renetted, changed)
}

/// The items of `items` that `keep` keeps.
fn select(items: &Items, keep: impl Fn(&Item) -> bool) -> Items {
    items
        .iter()
        .filter(|(_, item)| keep(item))
        .map(|(id, item)| (id.clone(), item.clone()))
        .collect()
}

/// Node heads a move rewrites in a moved item's block: its coordinates,
/// angles and justification, and a zone's fill, which moves with it. Checked
/// against the board text pcbnew writes for every kind of the kinds fixture,
/// and against KiCad 10.0.8's text after a turn of all of them.
const GEOMETRY: [&str; 11] = [
    "at",
    "start",
    "mid",
    "end",
    "center",
    "pts",
    "xy",
    "polygon",
    "filled_polygon",
    "angle",
    "justify",
];

/// `node` without the nodes whose heads `drop` names, at any depth.
fn stripped(node: &SexpNode, drop: &[&str]) -> SexpNode {
    let SexpNode::List(children) = node else {
        return node.clone();
    };
    let kept = children
        .iter()
        .filter(|child| child.head().is_none_or(|head| !drop.contains(&head)))
        .map(|child| stripped(child, drop))
        .collect();
    SexpNode::List(kept)
}

/// `now` with each rectangle's or polygon's head read as `was` has it at
/// the same place: a rectangle turned off its axes is written as a polygon.
fn shape_heads_as(was: &SexpNode, now: &SexpNode) -> SexpNode {
    const PAIRS: [[&str; 2]; 2] = [["gr_rect", "gr_poly"], ["fp_rect", "fp_poly"]];
    let (SexpNode::List(was), SexpNode::List(now)) = (was, now) else {
        return now.clone();
    };
    let mut out: Vec<_> = now
        .iter()
        .enumerate()
        .map(|(index, child)| match was.get(index) {
            Some(held) => shape_heads_as(held, child),
            None => child.clone(),
        })
        .collect();
    let heads = (
        was.first().and_then(SexpNode::as_str),
        now.first().and_then(SexpNode::as_str),
    );
    if let (Some(held), Some(head)) = heads {
        if PAIRS
            .iter()
            .any(|pair| pair.contains(&held) && pair.contains(&head))
        {
            out[0] = SexpNode::Atom(held.to_string());
        }
    }
    SexpNode::List(out)
}

/// What the board text `after` a commit holds that a move of `moved` (UUIDs)
/// from `before` does not explain: the UUIDs of the blocks that differ, and
/// what differs, as paths. Top-level blocks are matched by their `uuid`:
/// - a moved item's block must be unchanged once its geometry ([`GEOMETRY`],
///   at any depth) is left out and a polygon's head where a rectangle's was
///   is read as the rectangle's, a turn off the axes writing a rectangle as
///   a polygon;
/// - any other block must be unchanged but for a zone's fill
///   (`filled_polygon`), which KiCad may rebuild on the push; a group's
///   block among them, which KiCad writes with its members sorted.
///
/// Top-level nodes with no `uuid` are compared in order, but for zones
/// without one: KiCad's generated teardrops, written with no `uuid`, which
/// it rebuilds when their pad or via moves. KiCad's API does not carry every
/// property, and an update resets what it does not carry; the item read-back
/// cannot see that, the board text does.
fn board_text_changes(
    before: &str,
    after: &str,
    moved: &BTreeSet<String>,
) -> anyhow::Result<(Vec<String>, Vec<String>)> {
    let split = |text: &str| -> anyhow::Result<(BTreeMap<String, SexpNode>, Vec<SexpNode>)> {
        let tree = copperpour_sexp::parse_sexp(text)?;
        let (mut blocks, mut rest) = (BTreeMap::new(), Vec::new());
        for node in tree.children().unwrap_or_default().iter().skip(1) {
            match node.find_str("uuid") {
                Some(id) => {
                    blocks.insert(id.to_string(), node.clone());
                }
                None if node.head() == Some("zone") => {}
                None => rest.push(node.clone()),
            }
        }
        Ok((blocks, rest))
    };
    let ((blocks, rest), (blocks_after, rest_after)) = (split(before)?, split(after)?);
    let (mut ids, mut paths) = (Vec::new(), Vec::new());
    let ids_seen: BTreeSet<&String> = blocks.keys().chain(blocks_after.keys()).collect();
    for id in ids_seen {
        let (was, now) = match (blocks.get(id), blocks_after.get(id)) {
            (Some(was), Some(now)) if moved.contains(id) => {
                let was = stripped(was, &GEOMETRY);
                let now = shape_heads_as(&was, &stripped(now, &GEOMETRY));
                (was, now)
            }
            (Some(was), Some(now)) => (
                stripped(was, &["filled_polygon"]),
                stripped(now, &["filled_polygon"]),
            ),
            (Some(was), None) => {
                paths.push(format!(
                    "kicad_pcb/{}[{id}] is gone",
                    was.head().unwrap_or("?")
                ));
                ids.push(id.clone());
                continue;
            }
            (None, Some(now)) => {
                paths.push(format!(
                    "kicad_pcb/{}[{id}] appeared",
                    now.head().unwrap_or("?")
                ));
                ids.push(id.clone());
                continue;
            }
            (None, None) => continue,
        };
        if was != now {
            diff_nodes("kicad_pcb", &was, &now, &mut paths);
            ids.push(id.clone());
        }
    }
    if rest.len() != rest_after.len() {
        paths.push("kicad_pcb: its nodes without a uuid changed in number".into());
    } else {
        for (was, now) in rest.iter().zip(&rest_after) {
            diff_nodes("kicad_pcb", was, now, &mut paths);
        }
    }
    Ok((ids, paths))
}

async fn handle_request(args: &Value, ctx: &ToolContext) -> anyhow::Result<CallToolResult> {
    let path = get_path(args, "board")?;
    let request = match parse(args) {
        Ok(value) => value,
        Err(result) => return Ok(result),
    };
    let dry = match opt_bool_flag(args, "dry_run") {
        Ok(value) => value.unwrap_or(true),
        Err(result) => return Ok(result),
    };
    let field = "expected_plan_revision";
    let expected = match args.get(field) {
        None if dry => None,
        None => return Ok(invalid_arg(field, "required when dry_run is false")),
        Some(Value::String(value)) if !value.is_empty() => Some(value.clone()),
        _ => return Ok(invalid_arg(field, "must be a nonempty string")),
    };
    let target = path.clone();
    let result = with_bound_board_ipc_classified(ctx, &path, move |client, document| {
        apply(
            client,
            &document,
            &target,
            &request,
            dry,
            expected.as_deref(),
        )
    })
    .await?;
    Ok(match result {
        Ok(BoardBinding::Bound(result)) => result,
        Ok(BoardBinding::Unserved { absence, message }) => {
            crate::tools::ipc_unserved_result(absence, &message)
        }
        // The copper check refused, before anything was sent but reads.
        Err(IpcFailure::Preflight(refusal)) => preflight_refusal_result(
            "item transform",
            &refusal,
            Some(preflight_refusal_kind(TOOL, &refusal, |_, name| {
                name.to_string()
            })),
        ),
        Err(failure) => ipc_failure_result(failure),
    })
}

async fn handle_transform_pcb_items(
    args: &Value,
    ctx: &ToolContext,
) -> anyhow::Result<CallToolResult> {
    let path = get_path(args, "board")?;
    let count = ["references", "uuids"]
        .iter()
        .filter_map(|field| args.get(*field).and_then(Value::as_array))
        .map(Vec::len)
        .sum();
    let result = handle_request(args, ctx).await?;
    Ok(envelope(result, &path, count))
}

fn apply(
    client: &KiCadIpcClient,
    document: &DocumentSpecifier,
    path: &Path,
    request: &Request,
    dry: bool,
    expected: Option<&str>,
) -> anyhow::Result<CallToolResult> {
    let contents = snapshot(client, document)?;
    let board = read_board(client, document, &BOARD)?;
    let Resolved {
        targets: before,
        mut reasons,
        touched,
        grouped,
    } = resolve(client, document, &contents, request, &board)?;
    let planned = plan(&before, request.transform, &mut reasons);
    // Every reason about the targets comes before a destination out of range.
    if !reasons.is_empty() {
        return Ok(blocked(TOOL, reasons));
    }
    let after = match planned {
        Ok(value) => value,
        Err(result) => return Ok(result),
    };
    // The check tests the moved pads against the copper that stays, and the
    // pads that stay against the selected copper where it is planned to go.
    let poses: Vec<_> = before
        .values()
        .zip(after.values())
        .filter_map(|items| match items {
            (Item::Footprint(held), Item::Footprint(planned)) => {
                let position = planned.position.unwrap_or_default();
                Some(PoseRequest::Set(IpcFootprintPlacement {
                    reference: footprint_reference(held).to_string(),
                    x: nm_to_mm(position.x_nm),
                    y: nm_to_mm(position.y_nm),
                    rotation: orientation(held) + request.transform.degrees(),
                }))
            }
            _ => None,
        })
        .collect();
    let mut moved = MovedCopper::default();
    for item in after.values() {
        match item {
            Item::Via(via) => moved.vias.push(via.as_ref().clone()),
            Item::Track(track) => moved.tracks.push(track.clone()),
            Item::Arc(arc) => moved.arcs.push(arc.clone()),
            _ => {}
        }
    }
    let check = pcb_copper_check::check_live_with_copper(
        client,
        document.clone(),
        &poses,
        &HashSet::new(),
        Some(&moved),
    )?;
    let connections = boundary(&board, &before)?;
    let rev = revision(document, &contents, (&before, &after), &touched);
    let report = |targets: &Items, seen: Option<&Groups>, extra: Value| {
        let mut body = rows(targets, &touched, seen);
        body["copper_check"] = check.report();
        body["boundary_connection_count"] = json!(connections.len());
        body["boundary_connections"] = json!(connections
            .iter()
            .take(SHOWN_BOUNDARY_CONNECTIONS)
            .collect::<Vec<_>>());
        if connections.len() > SHOWN_BOUNDARY_CONNECTIONS {
            body["boundary_connections_truncated"] = json!(true);
        }
        for (key, value) in extra.as_object().into_iter().flatten() {
            body[key] = value.clone();
        }
        CallToolResult::json(&body)
    };
    if dry {
        return Ok(report(
            &after,
            None,
            json!({"dry_run": true, "plan_revision": rev, "source": "ipc_plan"}),
        ));
    }
    if expected != Some(rev.as_str()) {
        return Ok(stale(path, "item"));
    }
    let conflicts = check.conflict_reasons();
    if !conflicts.is_empty() {
        return Ok(with_key(
            blocked(TOOL, conflicts),
            "copper_check",
            check.report(),
        ));
    }
    if before == after {
        return Ok(report(
            &before,
            None,
            json!({"dry_run": false, "unchanged": true, "source": "ipc_readback"}),
        ));
    }
    let ids: Vec<_> = before.keys().cloned().collect();
    // The UUIDs an uncertain outcome names: the targets, then the touched groups.
    let named: Vec<_> = ids
        .iter()
        .cloned()
        .chain(touched.iter().map(group_id))
        .collect();
    let mut stale_inside = false;
    let mut update_attempted = false;
    let committed = client.run_commit_recovering_in(document.clone(), UNDO, |scoped| {
        // Recheck after BeginCommit, before any update: the board text and
        // every target's whole message.
        if snapshot(scoped, document)? != contents
            || read_targets(scoped, document, &ids)? != before
        {
            stale_inside = true;
            anyhow::bail!("live board changed before the batch update");
        }
        update_attempted = true;
        scoped.update_items_in(document.clone(), update_order(&after, Vec::new(), &touched))
    });
    let uncertain = |ids: &[String], reason: String| uncertain(TOOL, "item", path, ids, reason);
    if let Err(error) = committed {
        if error.downcast_ref::<IpcBatchRecovered>().is_some() {
            return Ok(crate::tools::ipc_recovered_result(&format!("{error:#}")));
        }
        if error.downcast_ref::<IpcOutcomeUnknown>().is_some() {
            return Ok(uncertain(
                &named,
                format!("batch outcome cannot be confirmed: {error:#}"),
            ));
        }
        if stale_inside {
            // No UpdateItems was sent and the empty commit was dropped.
            return Ok(stale(path, "item"));
        }
        if !update_attempted {
            return Ok(blocked(
                TOOL,
                vec![format!(
                    "pre-update check failed; no update was sent: {error:#}"
                )],
            ));
        }
        // The targets and the whole board text as the plan read them.
        let as_was = read_targets(client, document, &ids).is_ok_and(|held| held == before)
            && snapshot(client, document).is_ok_and(|text| {
                board_text_changes(&contents, &text, &BTreeSet::new())
                    .is_ok_and(|(_, paths)| paths.is_empty())
            });
        return Ok(match as_was {
            true => blocked(
                TOOL,
                vec![format!("commit failed; the board is as it was: {error:#}")],
            ),
            false => uncertain(
                &named,
                format!("commit failed and the board could not be shown as it was: {error:#}"),
            ),
        });
    }
    let copper = select(&board, Item::is_copper);
    // A selected graphic's net is compared and restored as copper's is.
    let is_graphic = |item: &Item| matches!(item, Item::Graphic(_));
    let mut compared = copper.clone();
    compared.extend(select(&before, is_graphic));
    let mut expected = compared.clone();
    for (id, item) in &after {
        if let Some(slot) = expected.get_mut(id) {
            *slot = item.clone();
        }
    }
    let read = (|| {
        let observed = read_targets(client, document, &ids)?;
        let held = read_board(client, document, &COPPER)?;
        anyhow::Ok((observed, held, read_groups(client, document)?))
    })();
    let (observed, held, groups) = match read {
        Ok(read) => read,
        Err(error) => {
            return Ok(uncertain(
                &named,
                format!("committed items cannot be read back: {error:#}"),
            ))
        }
    };
    let mut seen = held.clone();
    seen.extend(select(&observed, is_graphic));
    let (renetted, changed) = copper_changes(&expected, &seen, &before);
    let others: Vec<_> = renetted
        .iter()
        .chain(&changed)
        .filter(|id| !before.contains_key(*id))
        .cloned()
        .collect();
    let inspect: Vec<_> = named.iter().chain(&others).cloned().collect();
    let mut differing = targets_differing(&observed, &after);
    for group in &touched {
        if !groups_hold(&groups, std::slice::from_ref(group)) {
            differing.push(group_label(group));
        }
    }
    for id in changed.iter().filter(|id| !before.contains_key(*id)) {
        differing.push(match (copper.get(id), seen.get(id)) {
            (Some(was), _) => format!("{} vanished or changed", was.label(id)),
            (None, Some(now)) => format!("{} appeared", now.label(id)),
            (None, None) => format!("'{id}' changed"),
        });
    }
    if !differing.is_empty() {
        return Ok(uncertain(
            &inspect,
            format!("read back differs from the plan: {}", differing.join("; ")),
        ));
    }
    if renetted.is_empty() {
        let moved = ids.iter().cloned().collect();
        let changes = snapshot(client, document)
            .and_then(|text| board_text_changes(&contents, &text, &moved));
        return Ok(match changes {
            Ok((changed, paths)) if !paths.is_empty() => {
                let more = changed.into_iter().filter(|id| !named.contains(id));
                let names: Vec<_> = named.iter().cloned().chain(more).collect();
                uncertain(
                    &names,
                    format!(
                        "the board text changed besides the move: {}; one undo in KiCad \
                         reverses the commit",
                        paths.join("; ")
                    ),
                )
            }
            Ok(_) => report(
                &observed,
                Some(&groups),
                json!({"dry_run": false, "plan_revision": rev, "source": "ipc_readback",
                "undo": "One KiCad undo step; board not saved", "refill_required_before_drc": true}),
            ),
            Err(error) => uncertain(
                &named,
                format!("the board text cannot be compared after the commit: {error:#}"),
            ),
        });
    }
    let mut reasons: Vec<_> = renetted
        .iter()
        .map(|id| {
            format!(
                "{} '{id}' changed net from '{}' to '{}'",
                compared[id].kind(),
                compared[id].net_name(),
                seen[id].net_name()
            )
        })
        .collect();
    // Only a proven net-only change is restored; nothing uncertain is rolled
    // back blindly. An unselected re-netted item in a group is not sent: an
    // in-place update detaches it from its group in the running editor
    // (measured on KiCad 10.0.8), and its group cannot be sent for one
    // member, so the restore cannot verify.
    let others = others
        .iter()
        .filter(|id| !grouped.contains(*id))
        .map(|id| copper[id].pack())
        .collect();
    let items = update_order(&before, others, &touched);
    let summary = reasons.join("; ");
    let read = (&observed, &held);
    Ok(
        match restore(client, document, &before, read, &copper, items, &touched) {
            Ok(true) => {
                reasons.push(
                    "the move was undone by a second commit, and KiCad's undo history holds \
                     both steps"
                        .into(),
                );
                blocked(TOOL, reasons)
            }
            Ok(false) => uncertain(
                &inspect,
                format!("net changed; restoration not verified: {summary}"),
            ),
            Err(error) => uncertain(
                &inspect,
                format!("net changed; restoration failed: {error:#}; {summary}"),
            ),
        },
    )
}

/// Send `items` back in a second commit, once the targets still read as
/// `observed`, the board's copper as `held` (the same items on the same
/// nets) and the touched groups as they were; and whether a fresh read then
/// holds every target as `before` (a zone but for its fill, which KiCad may
/// rebuild), the board's copper as `copper` and every touched group as it
/// was.
fn restore(
    client: &KiCadIpcClient,
    document: &DocumentSpecifier,
    before: &Items,
    (observed, held): (&Items, &Items),
    copper: &Items,
    items: Vec<prost_types::Any>,
    touched: &[Group],
) -> anyhow::Result<bool> {
    let ids: Vec<_> = before.keys().cloned().collect();
    let unchanged =
        |a: &Items, b: &Items, targets: &Items| copper_changes(a, b, targets) == Default::default();
    client.run_commit_recovering_in(
        document.clone(),
        "Restore PCB items after net change",
        |scoped| {
            anyhow::ensure!(
                read_targets(scoped, document, &ids)? == *observed
                    && groups_hold(&read_groups(scoped, document)?, touched),
                "items changed before restoration"
            );
            anyhow::ensure!(
                unchanged(held, &read_board(scoped, document, &COPPER)?, &Items::new()),
                "the board's copper changed before restoration"
            );
            scoped.update_items_in(document.clone(), items)
        },
    )?;
    let as_before = |held: Items| {
        held.len() == before.len()
            && before
                .iter()
                .all(|(id, was)| held.get(id).map(Item::without_fill) == Some(was.without_fill()))
    };
    Ok(read_targets(client, document, &ids).is_ok_and(as_before)
        && read_board(client, document, &COPPER).is_ok_and(|held| unchanged(copper, &held, before))
        && read_groups(client, document).is_ok_and(|seen| groups_hold(&seen, touched)))
}

#[cfg(test)]
#[path = "pcb_item_transform_tests.rs"]
mod tests;
