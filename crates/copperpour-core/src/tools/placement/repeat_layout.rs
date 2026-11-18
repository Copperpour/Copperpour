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
//! The poses go through `set_component_placements`' writers: on a live board
//! its copper check and one KiCad commit, otherwise one guarded write of the
//! saved file. Routing, zones and other board items are not copied.

use crate::mcp::error::ToolErrorKind;
use crate::mcp::protocol::CallToolResult;
use crate::tool;
use crate::tools::pcb_board::{attempt_ipc_write_naming, preflight_refusal_kind, BoardWrite};
use crate::tools::pcb_components::{
    footprint_reference, normalize_root_angle, place_closed_board_footprints,
};
use crate::tools::pcb_copper_check::{self, CheckOptions, Checked, PoseRequest};
use crate::tools::{get_path, invalid_arg, require_str, ToolContext, ToolDef};
use anyhow::Context;
use copperpour_ipc::builders::{any_is, nm_to_mm};
use copperpour_ipc::client::KiCadIpcClient;
use copperpour_ipc::gen::kiapi;
use copperpour_ipc::types::IpcFootprintPlacement;
use copperpour_ipc::PreflightCause;
use copperpour_sexp::writer::{read_consistent, write_atomic_if_unchanged};
use prost::Message;
use serde_json::{json, Value};
use std::collections::HashMap;
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
         number, library footprint, pad count or side is refused, moving nothing. Uses one live \
         KiCad update and one undo step after set_component_placements' copper check when \
         reachable; otherwise edits a closed board file once with revision checks. The \
         closed-file path cannot run the check and reports copper_check.status not_run. Tracks, \
         vias and zones are not copied.",
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
    pads: usize,
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

/// Every target footprint's pose, or the refusal. `target_references` names the
/// anchor's counterparts to lay out; `None` is every one on the board.
fn plan(
    footprints: &[Footprint],
    anchor: &str,
    targets: Option<&[String]>,
) -> Result<Vec<IpcFootprintPlacement>, CallToolResult> {
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
            } else if to.pads != from.pads {
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
                    pads(to.pads),
                    from.reference,
                    from.pads
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
        paired.push((*target_anchor, matched));
    }
    if !reasons.is_empty() {
        return Err(mismatch(reasons));
    }

    let mut placements = Vec::new();
    for (target_anchor, matched) in paired {
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
    }
    Ok(placements)
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
fn rotate_point(x: i64, y: i64, degrees: f64) -> (i64, i64) {
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
            pads: definition.map_or(0, |definition| {
                definition
                    .items
                    .iter()
                    .filter(|item| any_is(item, "kiapi.board.types.Pad"))
                    .count()
            }),
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
                pads: node.find_all("pad").len(),
                x_nm: copperpour_ipc::builders::mm_to_nm(at.0),
                y_nm: copperpour_ipc::builders::mm_to_nm(at.1),
                rotation: at.2.unwrap_or(0.0),
                back: node.find_str("layer") == Some("B.Cu"),
                reference,
            })
        })
        .collect()
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

/// What the live write hands back.
enum Live {
    Planned(Checked<Vec<IpcFootprintPlacement>>),
    Refused(CallToolResult),
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
            let placements = match plan(&footprints, &live_anchor, live_targets.as_deref()) {
                Ok(placements) => placements,
                Err(refusal) => return Ok(Live::Refused(refusal)),
            };
            let requests: Vec<_> = placements.iter().cloned().map(PoseRequest::Set).collect();
            pcb_copper_check::check_then_apply(client, document, &requests, options, |client| {
                client.set_footprint_placements(&placements)
            })
            .map(Live::Planned)
        },
    )
    .await?
    {
        BoardWrite::Ipc(Live::Refused(refusal)) => Ok(refusal),
        BoardWrite::Ipc(Live::Planned(Checked::Applied { applied, check })) => Ok(check
            .applied_result(json!({
                "count": applied.len(),
                "placements": applied,
                "source": "ipc",
                "undo": "One KiCad undo step reverses the whole layout."
            }))),
        BoardWrite::Ipc(Live::Planned(Checked::Blocked(check))) => {
            Ok(check.blocked_result(OPERATION))
        }
        BoardWrite::Ipc(Live::Planned(Checked::DryRun(check))) => Ok(check.dry_run_result()),
        BoardWrite::Refused(result) => Ok(result),
        BoardWrite::File(reason) => {
            let evidence = reason.evidence();
            let warning = reason.warning();
            let dry_run = options.dry_run;
            match tokio::task::spawn_blocking(move || {
                repeat_in_file(&board, &anchor, targets.as_deref(), dry_run)
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
    let placements = plan(&footprints, anchor, targets).map_err(Ok)?;
    let (updated, applied) = match place_closed_board_footprints(&content, &placements) {
        Ok(placed) => placed,
        Err(error) => return refuse(error),
    };
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
        return Ok(json!({
            "dry_run": true,
            "count": placements.len(),
            "placements": placements,
            "source": "file",
            "copper_check": pcb_copper_check::not_run_on_closed_board()
        }));
    }
    if let Err(error) = write_atomic_if_unchanged(board, &content, &updated) {
        return refuse(ClosedBoardError::Io(error.into()));
    }
    Ok(json!({
        "count": applied.len(),
        "placements": applied,
        "source": "file",
        "copper_check": pcb_copper_check::not_run_on_closed_board()
    }))
}
