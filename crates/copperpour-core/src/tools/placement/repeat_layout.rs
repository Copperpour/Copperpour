//! `repeat_layout`: place the footprints of every other placement of a sheet
//! as they are placed on one, with the arithmetic of KiCad's Repeat Layout.
//!
//! A sheet placed more than once gives each of its symbols one footprint per
//! placement, and the footprints of one placement share the sheet path that
//! leads to it: their `(path …)` less its last UUID. KiCad's Repeat Layout
//! (`pcbnew/tools/multichannel_tool.cpp`) copies one placement's layout onto
//! the others by rule area, behind a dialog, so it cannot run over IPC. This
//! plans its footprint placement from an anchor footprint instead: the sheet
//! the anchor's symbol is on, with every sheet below it, is the reference,
//! and every other footprint of the anchor's symbol anchors a target. Each
//! target footprint takes its counterpart's pose, turned by the two anchors'
//! difference in rotation and moved so the anchor lands on the target's
//! anchor, in `copyRuleAreaContents`' order and with `RotatePoint`'s rounding.
//! KiCad turns the reference anchor in place and back for each target, which
//! is not exact away from right angles; this turns the unmoved anchor.
//!
//! Routing goes with them, unless `copy_routing` is false: the reference's
//! tracks, arcs and vias on the nets every pad of which is on the reference
//! placement, KiCad's nets local to a channel (`AutogenerateRuleAreas`,
//! `multichannel_tool.cpp` 2396-2414). Each is turned and moved as its
//! footprints are, and takes the net its pads' counterparts are on, as
//! `fixupNet` maps it; the copies replace the target's own items on those
//! nets. A net whose routing is copied must become one net local to the
//! target placement, or the call is refused.
//!
//! The poses go through `set_component_placements`' writers: on a live board
//! its copper check and one KiCad commit, which the routing joins, otherwise
//! one guarded write of the saved file. Zones and other board items are not
//! copied.

use crate::mcp::error::ToolErrorKind;
use crate::mcp::protocol::CallToolResult;
use crate::tool;
use crate::tools::pcb_board::{
    attempt_ipc_write_naming, block_tag, preflight_refusal_kind, BoardWrite,
};
use crate::tools::pcb_board_outline::mm_text;
use crate::tools::pcb_components::{
    escape_sexp_string, footprint_reference, normalize_root_angle, place_closed_board_footprints,
};
use crate::tools::pcb_copper_check::{self, CheckOptions, Checked, PoseRequest};
use crate::tools::{get_path, invalid_arg, require_str, ToolContext, ToolDef};
use anyhow::Context;
use copperpour_ipc::builders::{any_is, mm_to_nm, nm_to_mm, pack_any};
use copperpour_ipc::client::KiCadIpcClient;
use copperpour_ipc::gen::kiapi;
use copperpour_ipc::types::IpcFootprintPlacement;
use copperpour_ipc::PreflightCause;
use copperpour_sexp::board::exact_coordinate_pair;
use copperpour_sexp::net::net_name;
use copperpour_sexp::writer::{
    apply_edits, find_block_with_leading_whitespace, find_direct_child_blocks, new_uuid,
    read_consistent, write_atomic_if_unchanged, SexpEdit,
};
use prost::Message;
use serde_json::{json, Value};
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::path::Path;

const OPERATION: &str = "repeat_layout";

pub(super) fn tool() -> ToolDef {
    tool!(
        "repeat_layout",
        "Place the footprints of every other placement of a sheet as they are placed on one, \
         turned and moved with the arithmetic of KiCad's Repeat Layout. The anchor is a \
         footprint on the placement laid out; every other footprint of its symbol anchors a \
         target. Footprints are matched by symbol instance path over the anchor's sheet and the \
         sheets below it, and each target takes its counterpart's pose turned by the anchors' \
         rotation difference and moved onto its anchor. A target whose footprints differ in \
         number, library footprint, pad count or side is refused, moving nothing. Unless \
         copy_routing is false, the reference's tracks, arcs and vias on nets whose every pad is \
         on its placement are copied the same way onto each target, on the nets the matched \
         pads give them there, replacing the target's own routing on those nets. Uses one live \
         KiCad update and one undo step after set_component_placements' copper check when \
         reachable; otherwise edits a closed board file once with revision checks. The \
         closed-file path cannot run the check and reports copper_check.status not_run. Zones \
         are not copied.",
        json!({
            "type": "object",
            "properties": {
                "board": { "type": "string" },
                "anchor_reference": {
                    "type": "string",
                    "description": "Reference of a footprint on the placement already laid out, e.g. 'R1'"
                },
                "target_references": {
                    "type": "array",
                    "items": { "type": "string" },
                    "minItems": 1,
                    "description": "References of the anchor's counterparts on the placements to lay out, each already where its placement belongs. Omit for every other placement."
                },
                "copy_routing": {
                    "type": "boolean",
                    "default": true,
                    "description": "Copy the reference placement's routing on its own nets onto each target, replacing the target's own routing on the nets the copies take. False places footprints only."
                },
                "dry_run": {
                    "type": "boolean",
                    "default": false,
                    "description": pcb_copper_check::DRY_RUN_DESCRIPTION
                },
                "allow_copper_conflicts": {
                    "type": "boolean",
                    "default": false,
                    "description": pcb_copper_check::ALLOW_COPPER_CONFLICTS_DESCRIPTION
                }
            },
            "required": ["board", "anchor_reference"]
        }),
        |args, ctx| async move { handle_repeat_layout(args, ctx).await }
    )
    .with_board_access(crate::tools::BoardAccess::LivePreferredWithFallback)
}

// ─── The plan ─────────────────────────────────────────────────────────────────

/// A footprint as the plan reads it, from KiCad or from the saved file.
#[derive(Debug, Clone)]
struct Footprint {
    reference: String,
    /// The symbol's instance path, sheet UUIDs then the symbol's own; empty
    /// for a footprint with no symbol.
    path: Vec<String>,
    library_id: String,
    pads: Vec<Pad>,
    x_nm: i64,
    y_nm: i64,
    rotation: f64,
    back: bool,
}

impl Footprint {
    fn sheet(&self) -> &[String] {
        &self.path[..self.path.len().saturating_sub(1)]
    }

    fn side(&self) -> &'static str {
        if self.back {
            "B.Cu"
        } else {
            "F.Cu"
        }
    }
}

/// A pad's number and net, `""` for no net.
#[derive(Debug, Clone)]
struct Pad {
    number: String,
    net: String,
}

/// A track, arc or via as the plan reads it: its KIID and net, `""` for no
/// net.
#[derive(Debug, Clone)]
struct Wire {
    id: String,
    net: String,
}

/// What a call lays out: the target footprints' poses and, when routing is
/// copied, each target placement's routing.
struct Layout {
    placements: Vec<IpcFootprintPlacement>,
    routing: Vec<TargetRouting>,
}

/// One target placement's routing. `copies` are the reference items copied
/// onto it, by index into the routing read, each with its net there;
/// `removed` are its own items the copies replace.
struct TargetRouting {
    target: String,
    rotation: f64,
    offset: (i64, i64),
    nets: BTreeMap<String, String>,
    copies: Vec<(usize, String)>,
    removed: Vec<usize>,
}

impl TargetRouting {
    fn report(&self) -> Value {
        json!({
            "target_reference": self.target,
            "copied_count": self.copies.len(),
            "removed_count": self.removed.len(),
            "nets": self.nets,
        })
    }
}

/// `(x, y)` turned about the origin and moved, as `copyRuleAreaContents`
/// turns and moves a copy.
fn carry(x: i64, y: i64, rotation: f64, (dx, dy): (i64, i64)) -> (i64, i64) {
    let (x, y) = rotate_point(x, y, rotation);
    (x + dx, y + dy)
}

/// The nets every pad of which is on a footprint of `members`, as KiCad finds
/// the nets local to a channel (`AutogenerateRuleAreas`,
/// `multichannel_tool.cpp` 2396-2414 at 10.0.6).
fn local_nets<'a>(
    footprints: &'a [Footprint],
    members: &[(&[String], &Footprint)],
) -> HashSet<&'a str> {
    let on: HashSet<*const Footprint> = members.iter().map(|(_, fp)| *fp as *const _).collect();
    let (mut inside, mut outside) = (HashSet::new(), HashSet::new());
    for fp in footprints {
        let nets = if on.contains(&(fp as *const _)) {
            &mut inside
        } else {
            &mut outside
        };
        nets.extend(fp.pads.iter().map(|pad| pad.net.as_str()));
    }
    inside.retain(|net| !net.is_empty() && !outside.contains(net));
    inside
}

/// A target's nets for the reference's, its copies and its removed items, as
/// [`TargetRouting`] holds them.
type Route = (BTreeMap<String, String>, Vec<(usize, String)>, Vec<usize>);

/// The routing a target placement takes: each reference item on a net in
/// `reference_nets`, with the net the pads matched to that net's pads are on
/// (`fixupNet`), and the target's own items on the nets the copies go on. A
/// net whose routing is copied must become one net local to the target, or
/// the reason goes in `reasons`.
fn route(
    footprints: &[Footprint],
    wires: &[Wire],
    reference_nets: &HashSet<&str>,
    target_members: &[(&[String], &Footprint)],
    matched: &[(&Footprint, &Footprint)],
    target_anchor: &Footprint,
    reasons: &mut Vec<String>,
) -> Route {
    let target_nets = local_nets(footprints, target_members);
    let mut images: BTreeMap<&str, BTreeSet<&str>> = BTreeMap::new();
    for (from, to) in matched {
        for pad in &from.pads {
            if reference_nets.contains(pad.net.as_str()) {
                let image = to
                    .pads
                    .iter()
                    .find(|counterpart| counterpart.number == pad.number)
                    .map_or("", |counterpart| counterpart.net.as_str());
                images.entry(pad.net.as_str()).or_default().insert(image);
            }
        }
    }
    let routed: HashSet<&str> = wires.iter().map(|wire| wire.net.as_str()).collect();
    let placement = format!("{}'s placement", target_anchor.reference);
    let mut nets: BTreeMap<&str, &str> = BTreeMap::new();
    let mut taken: HashMap<&str, &str> = HashMap::new();
    for (net, on_target) in &images {
        if !routed.contains(net) {
            continue;
        }
        let image = match on_target.iter().collect::<Vec<_>>().as_slice() {
            [image] if !image.is_empty() => **image,
            several => {
                let names: Vec<&str> = several
                    .iter()
                    .map(|image| if image.is_empty() { "no net" } else { image })
                    .collect();
                reasons.push(format!(
                    "{net} becomes {} on {placement}",
                    names.join(" and ")
                ));
                continue;
            }
        };
        if !target_nets.contains(image) {
            reasons.push(format!(
                "{net} becomes {image}, which has pads off {placement}"
            ));
        } else if let Some(other) = taken.insert(image, net) {
            reasons.push(format!(
                "{other} and {net} both become {image} on {placement}"
            ));
        } else {
            nets.insert(net, image);
        }
    }
    let copies = wires
        .iter()
        .enumerate()
        .filter_map(|(index, wire)| {
            let net = nets.get(wire.net.as_str())?;
            Some((index, net.to_string()))
        })
        .collect();
    let removed = wires
        .iter()
        .enumerate()
        .filter(|(_, wire)| taken.contains_key(wire.net.as_str()))
        .map(|(index, _)| index)
        .collect();
    let nets = nets
        .into_iter()
        .map(|(from, to)| (from.to_string(), to.to_string()))
        .collect();
    (nets, copies, removed)
}

/// The one footprint `reference` names, or the refusal naming `field`.
fn named<'a>(
    footprints: &'a [Footprint],
    reference: &str,
    field: &str,
) -> Result<&'a Footprint, CallToolResult> {
    let mut found = footprints.iter().filter(|fp| fp.reference == reference);
    match (found.next(), found.next()) {
        (Some(footprint), None) => Ok(footprint),
        (None, _) => Err(invalid_arg(
            field,
            &format!("no footprint '{reference}' on this board"),
        )),
        (Some(_), Some(_)) => Err(invalid_arg(
            field,
            &format!("'{reference}' appears more than once on this board"),
        )),
    }
}

/// The footprints on `sheet` and every sheet below it, in board order, each
/// with its path below `sheet`. A second footprint on one path is a reason in
/// `reasons`: neither is that symbol's one footprint.
fn members<'a>(
    footprints: &'a [Footprint],
    sheet: &[String],
    reasons: &mut Vec<String>,
) -> Vec<(&'a [String], &'a Footprint)> {
    let mut seen: HashMap<&[String], &Footprint> = HashMap::new();
    let mut members = Vec::new();
    for fp in footprints
        .iter()
        .filter(|fp| fp.path.len() > sheet.len() && fp.path.starts_with(sheet))
    {
        let below = &fp.path[sheet.len()..];
        if let Some(other) = seen.get(below) {
            let mut both = [other.reference.as_str(), fp.reference.as_str()];
            both.sort_unstable();
            reasons.push(format!(
                "{} and {} are footprints of one symbol",
                both[0], both[1]
            ));
        } else {
            seen.insert(below, fp);
            members.push((below, fp));
        }
    }
    members
}

/// Every target footprint's pose and, given the board's `routing`, each
/// target placement's routing, or the refusal. `target_references` names the
/// anchor's counterparts to lay out; `None` is every one on the board.
fn plan(
    footprints: &[Footprint],
    routing: Option<&[Wire]>,
    anchor: &str,
    targets: Option<&[String]>,
) -> Result<Layout, CallToolResult> {
    let anchor = named(footprints, anchor, "anchor_reference")?;
    let reference_sheet = anchor.sheet();
    if reference_sheet.is_empty() {
        let reason = if anchor.path.is_empty() {
            format!("{} has no symbol, so it is on no sheet", anchor.reference)
        } else {
            format!(
                "{}'s symbol is on the root sheet, which is placed once",
                anchor.reference
            )
        };
        return Err(invalid_arg("anchor_reference", &reason));
    }
    let symbol = anchor.path.last();
    let is_counterpart = |fp: &Footprint| {
        fp.path.last() == symbol && fp.path.len() >= 2 && fp.sheet() != reference_sheet
    };
    let mut target_anchors: Vec<&Footprint> = match targets {
        None => footprints.iter().filter(|fp| is_counterpart(fp)).collect(),
        Some(names) => names
            .iter()
            .map(|name| {
                let target = named(footprints, name, "target_references")?;
                if is_counterpart(target) {
                    Ok(target)
                } else {
                    Err(invalid_arg(
                        "target_references",
                        &format!(
                            "{name} is not {}'s symbol on another placement of its sheet",
                            anchor.reference
                        ),
                    ))
                }
            })
            .collect::<Result<_, _>>()?,
    };
    if target_anchors.is_empty() {
        return Err(invalid_arg(
            "anchor_reference",
            &format!(
                "{}'s symbol has no footprint on another placement of its sheet",
                anchor.reference
            ),
        ));
    }
    // Two footprints of the anchor's symbol on one sheet are one placement,
    // laid out once; `members` reports the second as sharing the first's path.
    let mut sheets: Vec<&[String]> = Vec::new();
    target_anchors.retain(|fp| {
        let first = !sheets.contains(&fp.sheet());
        if first {
            sheets.push(fp.sheet());
        }
        first
    });

    let mut uses: HashMap<&str, usize> = HashMap::new();
    for fp in footprints {
        *uses.entry(fp.reference.as_str()).or_default() += 1;
    }
    let mut reasons = Vec::new();
    let reference = members(footprints, reference_sheet, &mut reasons);
    if reasons.is_empty() && reference.len() < 2 {
        return Err(invalid_arg(
            "anchor_reference",
            &format!(
                "{} is the only footprint on its sheet and the sheets below it, so there is \
                 nothing to place",
                anchor.reference
            ),
        ));
    }
    let mut paired = Vec::new();
    for target_anchor in &target_anchors {
        let target_members = members(footprints, target_anchor.sheet(), &mut reasons);
        let target: HashMap<&[String], &Footprint> = target_members.iter().copied().collect();
        let mut matched = Vec::new();
        for (below, from) in &reference {
            let Some(to) = target.get(below) else {
                reasons.push(format!(
                    "{} has no counterpart on {}'s placement",
                    from.reference, target_anchor.reference
                ));
                continue;
            };
            if uses[to.reference.as_str()] > 1 {
                reasons.push(format!(
                    "{} appears more than once on this board",
                    to.reference
                ));
            } else if to.library_id != from.library_id {
                reasons.push(format!(
                    "{} is {} where {} is {}",
                    to.reference, to.library_id, from.reference, from.library_id
                ));
            } else if to.pads.len() != from.pads.len() {
                let pads = |n: usize| {
                    if n == 1 {
                        "1 pad".to_string()
                    } else {
                        format!("{n} pads")
                    }
                };
                reasons.push(format!(
                    "{} has {} where {} has {}",
                    to.reference,
                    pads(to.pads.len()),
                    from.reference,
                    from.pads.len()
                ));
            } else if to.back != from.back {
                reasons.push(format!(
                    "{} is on {} where {} is on {}; flip it first",
                    to.reference,
                    to.side(),
                    from.reference,
                    from.side()
                ));
            }
            matched.push((*from, *to));
        }
        for (below, extra) in &target_members {
            if !reference.iter().any(|(path, _)| path == below) {
                reasons.push(format!(
                    "{} has no counterpart on {}'s placement",
                    extra.reference, anchor.reference
                ));
            }
        }
        paired.push((*target_anchor, target_members, matched));
    }
    if !reasons.is_empty() {
        return Err(mismatch(reasons));
    }
    let mut routes = Vec::new();
    if let Some(wires) = routing {
        let reference_nets = local_nets(footprints, &reference);
        for (target_anchor, target_members, matched) in &paired {
            routes.push(route(
                footprints,
                wires,
                &reference_nets,
                target_members,
                matched,
                target_anchor,
                &mut reasons,
            ));
        }
        if !reasons.is_empty() {
            return Err(mismatch(reasons));
        }
    }

    let mut placements = Vec::new();
    let mut transforms = Vec::new();
    for (target_anchor, _, matched) in paired {
        let rotation = target_anchor.rotation - anchor.rotation;
        let (ax, ay) = rotate_point(anchor.x_nm, anchor.y_nm, rotation);
        let (dx, dy) = (target_anchor.x_nm - ax, target_anchor.y_nm - ay);
        for (from, to) in matched {
            if to.reference == target_anchor.reference {
                continue;
            }
            let (x, y) = rotate_point(from.x_nm, from.y_nm, rotation);
            placements.push(IpcFootprintPlacement {
                reference: to.reference.clone(),
                x: nm_to_mm(x + dx),
                y: nm_to_mm(y + dy),
                rotation: normalize_root_angle(from.rotation + rotation),
            });
        }
        transforms.push((target_anchor.reference.clone(), rotation, (dx, dy)));
    }
    let routing = transforms
        .into_iter()
        .zip(routes)
        .map(
            |((target, rotation, offset), (nets, copies, removed))| TargetRouting {
                target,
                rotation,
                offset,
                nets,
                copies,
                removed,
            },
        )
        .collect();
    Ok(Layout {
        placements,
        routing,
    })
}

fn mismatch(reasons: Vec<String>) -> CallToolResult {
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
        "{OPERATION} refused: the placements do not match: {shown}{more}. Nothing was moved."
    );
    CallToolResult::error_kind(
        ToolErrorKind::PlanBlocked {
            operation: OPERATION.to_string(),
            reasons,
        },
        message,
    )
}

/// KiCad's `RotatePoint` on a point in nanometres, about the origin
/// (`libs/kimath/src/trigo.cpp`): the angle normalised to [0, 360), the four
/// right angles exact, and otherwise each coordinate rounded half away from
/// zero, with `EDA_ANGLE`'s `M_SQRT1_2` sine and cosine at the odd multiples
/// of 45°.
pub(super) fn rotate_point(x: i64, y: i64, degrees: f64) -> (i64, i64) {
    let mut angle = degrees.rem_euclid(360.0);
    if angle >= 360.0 {
        angle -= 360.0;
    }
    if angle == 0.0 {
        return (x, y);
    }
    if angle == 90.0 {
        return (y, -x);
    }
    if angle == 180.0 {
        return (-x, -y);
    }
    if angle == 270.0 {
        return (-y, x);
    }
    let half = std::f64::consts::FRAC_1_SQRT_2;
    let (sin, cos) = match angle {
        45.0 => (half, half),
        135.0 => (half, -half),
        225.0 => (-half, -half),
        315.0 => (-half, half),
        _ => {
            let radians = angle * (std::f64::consts::PI / 180.0);
            (radians.sin(), radians.cos())
        }
    };
    let (x, y) = (x as f64, y as f64);
    (
        (y * sin + x * cos).round() as i64,
        (y * cos - x * sin).round() as i64,
    )
}

// ─── Reading the board ────────────────────────────────────────────────────────

/// The footprints KiCad holds, as `FOOTPRINT::Serialize` sends them.
fn live_footprints(
    client: &KiCadIpcClient,
    document: kiapi::common::types::DocumentSpecifier,
) -> anyhow::Result<Vec<Footprint>> {
    use kiapi::common::types::KiCadObjectType as Kot;
    let mut footprints = Vec::new();
    for item in client.get_items_in(document, Kot::KotPcbFootprint)? {
        if !any_is(&item, "kiapi.board.types.FootprintInstance") {
            continue;
        }
        let fp = kiapi::board::types::FootprintInstance::decode(item.value.as_slice())
            .context("KiCad returned an unreadable footprint")?;
        let position = fp.position.unwrap_or_default();
        let definition = fp.definition.as_ref();
        footprints.push(Footprint {
            reference: fp
                .reference_field
                .as_ref()
                .and_then(|field| field.text.as_ref())
                .and_then(|text| text.text.as_ref())
                .map(|text| text.text.clone())
                .unwrap_or_default(),
            path: fp
                .symbol_path
                .as_ref()
                .map(|path| path.path.iter().map(|id| id.value.clone()).collect())
                .unwrap_or_default(),
            library_id: definition
                .and_then(|definition| definition.id.as_ref())
                .map(|id| format!("{}:{}", id.library_nickname, id.entry_name))
                .unwrap_or_default(),
            pads: definition
                .map_or(&[][..], |definition| &definition.items)
                .iter()
                .filter(|item| any_is(item, "kiapi.board.types.Pad"))
                .map(|item| {
                    let pad = kiapi::board::types::Pad::decode(item.value.as_slice())
                        .context("KiCad returned an unreadable pad")?;
                    Ok(Pad {
                        number: pad.number,
                        net: pad.net.map(|net| net.name).unwrap_or_default(),
                    })
                })
                .collect::<anyhow::Result<_>>()?,
            x_nm: position.x_nm,
            y_nm: position.y_nm,
            rotation: fp.orientation.map_or(0.0, |angle| angle.value_degrees),
            back: fp.layer == kiapi::board::types::BoardLayer::BlBCu as i32,
        });
    }
    Ok(footprints)
}

/// The footprints of a saved board, or why one cannot be read.
fn file_footprints(content: &str) -> Result<Vec<Footprint>, String> {
    let root = copperpour_sexp::parse_sexp(content)
        .map_err(|error| format!("the board does not parse: {error}"))?;
    root.find_all("footprint")
        .into_iter()
        .map(|node| {
            let reference = footprint_reference(node)
                .ok_or_else(|| "a footprint has no reference".to_string())?;
            let at = node
                .find("at")
                .and_then(|at| Some((at.get_f64(1)?, at.get_f64(2)?, at.get_f64(3))))
                .filter(|(x, y, angle)| {
                    x.is_finite() && y.is_finite() && angle.is_none_or(f64::is_finite)
                })
                .ok_or_else(|| format!("{reference} has no readable (at x y) placement"))?;
            Ok(Footprint {
                path: node
                    .find_str("path")
                    .map(|path| {
                        path.split('/')
                            .filter(|id| !id.is_empty())
                            .map(str::to_string)
                            .collect()
                    })
                    .unwrap_or_default(),
                library_id: node
                    .get(1)
                    .and_then(copperpour_sexp::SexpNode::as_str)
                    .unwrap_or_default()
                    .to_string(),
                pads: node
                    .find_all("pad")
                    .into_iter()
                    .map(|pad| Pad {
                        number: pad
                            .get(1)
                            .and_then(copperpour_sexp::SexpNode::as_str)
                            .unwrap_or_default()
                            .to_string(),
                        net: pad
                            .find("net")
                            .and_then(net_name)
                            .unwrap_or_default()
                            .to_string(),
                    })
                    .collect(),
                x_nm: copperpour_ipc::builders::mm_to_nm(at.0),
                y_nm: copperpour_ipc::builders::mm_to_nm(at.1),
                rotation: at.2.unwrap_or(0.0),
                back: node.find_str("layer") == Some("B.Cu"),
                reference,
            })
        })
        .collect()
}

/// A track, arc or via as `PCB_TRACK::Serialize` sends it.
enum LiveWire {
    Track(kiapi::board::types::Track),
    Arc(kiapi::board::types::Arc),
    Via(Box<kiapi::board::types::Via>),
}

impl LiveWire {
    /// The track, arc or via `item` holds, `None` for any other item.
    fn decode(item: &prost_types::Any) -> anyhow::Result<Option<Self>> {
        use kiapi::board::types as board;
        let bytes = item.value.as_slice();
        Ok(Some(if any_is(item, "kiapi.board.types.Track") {
            Self::Track(board::Track::decode(bytes).context("KiCad returned an unreadable track")?)
        } else if any_is(item, "kiapi.board.types.Arc") {
            Self::Arc(board::Arc::decode(bytes).context("KiCad returned an unreadable arc")?)
        } else if any_is(item, "kiapi.board.types.Via") {
            Self::Via(Box::new(
                board::Via::decode(bytes).context("KiCad returned an unreadable via")?,
            ))
        } else {
            return Ok(None);
        }))
    }

    fn wire(&self) -> Wire {
        let (id, net) = match self {
            Self::Track(track) => (&track.id, &track.net),
            Self::Arc(arc) => (&arc.id, &arc.net),
            Self::Via(via) => (&via.id, &via.net),
        };
        Wire {
            id: id.as_ref().map(|id| id.value.clone()).unwrap_or_default(),
            net: net.as_ref().map(|net| net.name.clone()).unwrap_or_default(),
        }
    }

    /// The copy `copyRuleAreaContents` makes: turned, moved and put on
    /// `net`, with no KIID, so KiCad gives it one.
    fn copied(&self, net: &str, rotation: f64, offset: (i64, i64)) -> prost_types::Any {
        let point = |point: Option<kiapi::common::types::Vector2>| {
            point.map(|point| {
                let (x_nm, y_nm) = carry(point.x_nm, point.y_nm, rotation, offset);
                kiapi::common::types::Vector2 { x_nm, y_nm }
            })
        };
        let net = Some(kiapi::board::types::Net {
            code: None,
            name: net.to_string(),
        });
        match self {
            Self::Track(track) => {
                let mut copy = track.clone();
                (copy.id, copy.parent, copy.net) = (None, None, net);
                (copy.start, copy.end) = (point(copy.start), point(copy.end));
                pack_any(&copy, "kiapi.board.types.Track")
            }
            Self::Arc(arc) => {
                let mut copy = arc.clone();
                (copy.id, copy.parent, copy.net) = (None, None, net);
                (copy.start, copy.mid, copy.end) =
                    (point(copy.start), point(copy.mid), point(copy.end));
                pack_any(&copy, "kiapi.board.types.Arc")
            }
            Self::Via(via) => {
                let mut copy = via.as_ref().clone();
                (copy.id, copy.parent, copy.net) = (None, None, net);
                copy.position = point(copy.position);
                pack_any(&copy, "kiapi.board.types.Via")
            }
        }
    }
}

/// The tracks, arcs and vias KiCad holds.
fn live_routing(
    client: &KiCadIpcClient,
    document: kiapi::common::types::DocumentSpecifier,
) -> anyhow::Result<Vec<LiveWire>> {
    use kiapi::common::types::KiCadObjectType as Kot;
    let mut wires = Vec::new();
    for kind in [Kot::KotPcbTrace, Kot::KotPcbArc, Kot::KotPcbVia] {
        for item in client.get_items_in(document.clone(), kind)? {
            wires.extend(LiveWire::decode(&item)?);
        }
    }
    Ok(wires)
}

/// A saved board's top-level track, arc or via: its tag, its block's span,
/// and the plan's view of it.
struct FileWire {
    kind: &'static str,
    span: (usize, usize),
    wire: Wire,
}

/// The points that place a routing item, as KiCad writes it.
fn defining_points(kind: &str) -> &'static [&'static str] {
    match kind {
        "segment" => &["start", "end"],
        "arc" => &["start", "mid", "end"],
        _ => &["at"],
    }
}

/// The tracks, arcs and vias of a saved board, or why they cannot be read.
fn file_routing(content: &str) -> Result<Vec<FileWire>, String> {
    let mut wires = Vec::new();
    for (start, end) in find_direct_child_blocks(content, "kicad_pcb") {
        let block = &content[start..end];
        let Some(kind) = ["segment", "arc", "via"]
            .into_iter()
            .find(|kind| block_tag(block) == Some(kind))
        else {
            continue;
        };
        let node = copperpour_sexp::parse_sexp(block)
            .map_err(|error| format!("a {kind} does not parse: {error}"))?;
        let id = node
            .find_str("uuid")
            .ok_or_else(|| format!("a {kind} has no uuid"))?;
        let net = match node.find("net") {
            None => "",
            Some(net) => net_name(net).ok_or_else(|| {
                format!(
                    "{kind} {id} names its net by number, as boards before KiCad 10 do; resave \
                     the board in KiCad 10, or pass copy_routing false"
                )
            })?,
        };
        wires.push(FileWire {
            kind,
            span: (start, end),
            wire: Wire {
                id: id.to_string(),
                net: net.to_string(),
            },
        });
    }
    Ok(wires)
}

/// The board text with each target's routing replaced by its copies. A copy
/// goes after the last routing item, where KiCad keeps them.
fn route_in_file(
    content: &str,
    wires: &[FileWire],
    routing: &[TargetRouting],
) -> Result<String, String> {
    let eol = if content.contains("\r\n") {
        "\r\n"
    } else {
        "\n"
    };
    let mut edits = Vec::new();
    let mut insertion = String::new();
    for target in routing {
        for &index in &target.removed {
            let (start, end) = wires[index].span;
            let (start, end) =
                find_block_with_leading_whitespace(content, start).unwrap_or((start, end));
            edits.push(SexpEdit::delete(start, end));
        }
        for (index, net) in &target.copies {
            let FileWire { kind, span, wire } = &wires[*index];
            let line = content[..span.0].rfind('\n').map_or(0, |i| i + 1);
            insertion.push_str(eol);
            insertion.push_str(&content[line..span.0]);
            insertion.push_str(&copied_block(
                &content[span.0..span.1],
                kind,
                &wire.id,
                net,
                target.rotation,
                target.offset,
            )?);
        }
    }
    if let Some(last) = wires.iter().map(|wire| wire.span.1).max() {
        edits.push(SexpEdit::insert(last, insertion));
    }
    Ok(apply_edits(content.to_string(), edits))
}

/// A routing item's block as `copyRuleAreaContents` copies it: each defining
/// point turned and moved, a new uuid, and `net`.
fn copied_block(
    block: &str,
    kind: &str,
    id: &str,
    net: &str,
    rotation: f64,
    offset: (i64, i64),
) -> Result<String, String> {
    let points = defining_points(kind);
    let mut edits = Vec::new();
    for (start, end) in find_direct_child_blocks(block, kind) {
        let child = &block[start..end];
        let replacement = match block_tag(child) {
            Some(tag) if points.contains(&tag) => {
                let (x, y) = copperpour_sexp::parse_sexp(child)
                    .ok()
                    .as_ref()
                    .and_then(exact_coordinate_pair)
                    .ok_or_else(|| format!("{kind} {id} has an unreadable ({tag} x y)"))?;
                let (x, y) = carry(mm_to_nm(x), mm_to_nm(y), rotation, offset);
                format!("({tag} {} {})", mm_text(x), mm_text(y))
            }
            Some("uuid") => format!("(uuid \"{}\")", new_uuid()),
            Some("net") => format!("(net \"{}\")", escape_sexp_string(net)),
            _ => continue,
        };
        edits.push(SexpEdit::replace(start, end, replacement));
    }
    Ok(apply_edits(block.to_string(), edits))
}

// ─── The handler ──────────────────────────────────────────────────────────────

/// A non-empty list of distinct references, or `None` when omitted.
fn parse_targets(args: &Value) -> Result<Option<Vec<String>>, CallToolResult> {
    let items = match &args["target_references"] {
        Value::Null => return Ok(None),
        Value::Array(items) if !items.is_empty() => items,
        _ => {
            return Err(invalid_arg(
                "target_references",
                "must be a non-empty array of references; omit it for every other placement",
            ))
        }
    };
    let mut targets: Vec<String> = Vec::with_capacity(items.len());
    for item in items {
        let Some(reference) = item.as_str().filter(|r| !r.trim().is_empty()) else {
            return Err(invalid_arg(
                "target_references",
                "every entry must be a reference",
            ));
        };
        if targets.iter().any(|seen| seen == reference) {
            return Err(invalid_arg(
                "target_references",
                &format!("names {reference} more than once"),
            ));
        }
        targets.push(reference.to_string());
    }
    Ok(Some(targets))
}

/// What the live write hands back: the check's outcome, with the placements
/// and the copies as KiCad holds them once applied, and the planned routing.
enum Live {
    Planned(
        Checked<(Vec<IpcFootprintPlacement>, Vec<Value>)>,
        Option<Value>,
    ),
    Refused(CallToolResult),
}

/// The `routing` object of a response: what was copied and removed, in all
/// and for each target placement.
fn routing_report(routing: &[TargetRouting]) -> Value {
    json!({
        "copied_count": routing.iter().map(|target| target.copies.len()).sum::<usize>(),
        "removed_count": routing.iter().map(|target| target.removed.len()).sum::<usize>(),
        "targets": routing.iter().map(TargetRouting::report).collect::<Vec<_>>(),
    })
}

/// Delete the routing the copies replace and create the copies, inside the
/// placements' commit, which a copy KiCad refuses drops; then, once it is
/// pushed, read the copies back. KiCad answers a create with the net it was
/// asked for, and on the push puts an item on the net of a pad it touches
/// (`issue_95_ipc.README.md`), so the read-back is the evidence. Each copy
/// not confirmed on its planned net is returned, with the net KiCad holds it
/// on, or none when KiCad gave no KIID for it or the read-back failed: the
/// layout is applied by then, so nothing after the commit is an error.
fn apply_live(
    client: &KiCadIpcClient,
    document: &kiapi::common::types::DocumentSpecifier,
    placements: &[IpcFootprintPlacement],
    removed: Vec<String>,
    copies: Vec<(prost_types::Any, String)>,
) -> anyhow::Result<(Vec<IpcFootprintPlacement>, Vec<Value>)> {
    let (items, nets): (Vec<_>, Vec<_>) = copies.into_iter().unzip();
    let (applied, created) = client.set_footprint_placements_then(placements, |client| {
        client.delete_items_in(document.clone(), removed)?;
        let created = client.create_items_in_reporting(document.clone(), items)?;
        let refused: Vec<String> = created
            .iter()
            .enumerate()
            .filter(|(_, creation)| !creation.created)
            .map(|(index, creation)| {
                format!(
                    "copy {index}: {} ({})",
                    creation.status, creation.error_message
                )
            })
            .collect();
        anyhow::ensure!(
            refused.is_empty(),
            "KiCad refused {} of the routing copies: {}",
            refused.len(),
            refused.join("; ")
        );
        Ok(created)
    })?;
    let kiids: Vec<Option<String>> = created
        .iter()
        .map(|creation| {
            let item = creation.item.as_ref()?;
            let kiid = LiveWire::decode(item).ok()??.wire().id;
            (!kiid.is_empty()).then_some(kiid)
        })
        .collect();
    let asked: Vec<String> = kiids.iter().flatten().cloned().collect();
    let mut held = HashMap::new();
    for item in client
        .get_items_by_id_in(document.clone(), &asked)
        .unwrap_or_default()
    {
        if let Ok(Some(wire)) = LiveWire::decode(&item) {
            let wire = wire.wire();
            held.insert(wire.id, wire.net);
        }
    }
    let unconfirmed = kiids
        .iter()
        .zip(&nets)
        .filter_map(|(kiid, net)| {
            let held_net = kiid.as_ref().and_then(|kiid| held.get(kiid));
            (held_net != Some(net))
                .then(|| json!({ "kiid": kiid, "net": net, "held_net": held_net }))
        })
        .collect();
    Ok((applied, unconfirmed))
}

async fn handle_repeat_layout(args: &Value, ctx: &ToolContext) -> anyhow::Result<CallToolResult> {
    let board = get_path(args, "board")?;
    let anchor = match require_str(args, "anchor_reference") {
        Ok(anchor) if !anchor.trim().is_empty() => anchor.to_string(),
        Ok(_) => return Ok(invalid_arg("anchor_reference", "must be a reference")),
        Err(error) => return Ok(error),
    };
    let targets = match parse_targets(args) {
        Ok(targets) => targets,
        Err(error) => return Ok(error),
    };
    let options = match CheckOptions::from_args(args) {
        Ok(options) => options,
        Err(error) => return Ok(error),
    };
    let copy_routing = match &args["copy_routing"] {
        Value::Null => true,
        Value::Bool(copy) => *copy,
        _ => return Ok(invalid_arg("copy_routing", "must be a boolean")),
    };

    let (live_anchor, live_targets) = (anchor.clone(), targets.clone());
    match attempt_ipc_write_naming(
        ctx,
        &board,
        "repeat layout",
        |refusal| match &refusal.cause {
            // A footprint the plan moves, refused once the plan was made: the
            // reason names it, and no argument does.
            PreflightCause::Rotation { .. }
            | PreflightCause::MissingReferences { .. }
            | PreflightCause::DuplicateReference { .. } => ToolErrorKind::PlanBlocked {
                operation: OPERATION.to_string(),
                reasons: vec![refusal.reason.clone()],
            },
            _ => preflight_refusal_kind(OPERATION, refusal, |_, name| name.to_string()),
        },
        move |client, document| {
            let footprints = live_footprints(client, document.clone())?;
            let wires = match copy_routing {
                true => live_routing(client, document.clone())?,
                false => Vec::new(),
            };
            let planned: Vec<Wire> = wires.iter().map(LiveWire::wire).collect();
            let routing = copy_routing.then_some(planned.as_slice());
            let layout = match plan(&footprints, routing, &live_anchor, live_targets.as_deref()) {
                Ok(layout) => layout,
                Err(refusal) => return Ok(Live::Refused(refusal)),
            };
            let removed: Vec<String> = layout
                .routing
                .iter()
                .flat_map(|target| {
                    target
                        .removed
                        .iter()
                        .map(|&index| planned[index].id.clone())
                })
                .collect();
            let copies: Vec<_> = layout
                .routing
                .iter()
                .flat_map(|target| {
                    target.copies.iter().map(|(index, net)| {
                        let copy = wires[*index].copied(net, target.rotation, target.offset);
                        (copy, net.clone())
                    })
                })
                .collect();
            let report = copy_routing.then(|| routing_report(&layout.routing));
            let ignored = removed.iter().cloned().collect();
            let requests: Vec<_> = layout
                .placements
                .iter()
                .cloned()
                .map(PoseRequest::Set)
                .collect();
            pcb_copper_check::check_then_apply_ignoring(
                client,
                document.clone(),
                &requests,
                &ignored,
                options,
                |client| apply_live(client, &document, &layout.placements, removed, copies),
            )
            .map(|checked| Live::Planned(checked, report))
        },
    )
    .await?
    {
        BoardWrite::Ipc(Live::Refused(refusal)) => Ok(refusal),
        BoardWrite::Ipc(Live::Planned(
            Checked::Applied {
                applied: (applied, unconfirmed),
                check,
            },
            report,
        )) => {
            let mut body = json!({
                "count": applied.len(),
                "placements": applied,
                "source": "ipc",
                "undo": "One KiCad undo step reverses the whole layout."
            });
            if let Some(report) = report {
                body["routing"] = report;
            }
            if !unconfirmed.is_empty() {
                body["warning"] = json!(format!(
                    "{} of the copied tracks, arcs and vias are not confirmed on their planned \
                     nets, listed in routing.unconfirmed: KiCad puts an item on the net of a pad \
                     it touches, and held_net is null where it gave no copy back to read. Check \
                     them, or undo the layout.",
                    unconfirmed.len()
                ));
                body["routing"]["unconfirmed"] = json!(unconfirmed);
            }
            Ok(check.applied_result(body))
        }
        BoardWrite::Ipc(Live::Planned(Checked::Blocked(check), _)) => {
            Ok(check.blocked_result(OPERATION))
        }
        BoardWrite::Ipc(Live::Planned(Checked::DryRun(check), report)) => {
            let mut body = check.dry_run_body();
            if let Some(report) = report {
                body["routing"] = report;
            }
            Ok(CallToolResult::json(&body))
        }
        BoardWrite::Refused(result) => Ok(result),
        BoardWrite::File(reason) => {
            let evidence = reason.evidence();
            let warning = reason.warning();
            let dry_run = options.dry_run;
            match tokio::task::spawn_blocking(move || {
                repeat_in_file(&board, &anchor, targets.as_deref(), copy_routing, dry_run)
            })
            .await?
            {
                Ok(mut body) => {
                    body["fallback_reason"] = evidence;
                    if !dry_run {
                        body["warning"] = json!(warning);
                    }
                    Ok(CallToolResult::json(&body))
                }
                Err(result) => result,
            }
        }
    }
}

/// The closed-board road: plan from the saved file and write it once,
/// unless it changed since it was read. The `Err` is the result to return.
fn repeat_in_file(
    board: &Path,
    anchor: &str,
    targets: Option<&[String]>,
    copy_routing: bool,
    dry_run: bool,
) -> Result<Value, anyhow::Result<CallToolResult>> {
    use crate::tools::pcb_components::ClosedBoardError;
    let refuse = |error: ClosedBoardError| Err(error.into_result());
    let content = match read_consistent(board) {
        Ok(content) => content,
        Err(error) => return refuse(ClosedBoardError::Io(error.into())),
    };
    let footprints = match file_footprints(&content) {
        Ok(footprints) => footprints,
        Err(reason) => return refuse(ClosedBoardError::Unusable(reason)),
    };
    let wires = match copy_routing.then(|| file_routing(&content)).transpose() {
        Ok(wires) => wires.unwrap_or_default(),
        Err(reason) => return refuse(ClosedBoardError::Unusable(reason)),
    };
    let planned: Vec<Wire> = wires.iter().map(|wire| wire.wire.clone()).collect();
    let routing = copy_routing.then_some(planned.as_slice());
    let layout = plan(&footprints, routing, anchor, targets).map_err(Ok)?;
    let routed = match route_in_file(&content, &wires, &layout.routing) {
        Ok(routed) => routed,
        Err(reason) => return refuse(ClosedBoardError::Unusable(reason)),
    };
    let (updated, applied) = match place_closed_board_footprints(&routed, &layout.placements) {
        Ok(placed) => placed,
        Err(error) => return refuse(error),
    };
    let report = copy_routing.then(|| routing_report(&layout.routing));
    if dry_run {
        let current: HashMap<&str, &Footprint> = footprints
            .iter()
            .map(|fp| (fp.reference.as_str(), fp))
            .collect();
        let placements: Vec<Value> = applied
            .iter()
            .map(|target| {
                let held = current[target.reference.as_str()];
                json!({
                    "reference": target.reference,
                    "current": {
                        "x": nm_to_mm(held.x_nm),
                        "y": nm_to_mm(held.y_nm),
                        "rotation": held.rotation,
                    },
                    "target": { "x": target.x, "y": target.y, "rotation": target.rotation },
                })
            })
            .collect();
        let mut body = json!({
            "dry_run": true,
            "count": placements.len(),
            "placements": placements,
            "source": "file",
            "copper_check": pcb_copper_check::not_run_on_closed_board()
        });
        if let Some(report) = report {
            body["routing"] = report;
        }
        return Ok(body);
    }
    if let Err(error) = write_atomic_if_unchanged(board, &content, &updated) {
        return refuse(ClosedBoardError::Io(error.into()));
    }
    let mut body = json!({
        "count": applied.len(),
        "placements": applied,
        "source": "file",
        "copper_check": pcb_copper_check::not_run_on_closed_board()
    });
    if let Some(report) = report {
        body["routing"] = report;
    }
    Ok(body)
}
