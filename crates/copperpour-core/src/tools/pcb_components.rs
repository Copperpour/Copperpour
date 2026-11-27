//! `pcb_components` toolset — place, move, rotate, query, and array footprints on the PCB.
//!
//! Most operations use the KiCAD IPC API so they integrate with KiCAD's undo/redo
//! system and don't require a separate file-sync step. `get_board_2d_view` uses
//! kicad-cli to render a PNG.

use crate::hex::encode_hex;
use crate::mcp::protocol::CallToolResult;
use crate::tool;
use crate::tools::library::{footprint_lib_nickname_for_dir, is_lib_id, resolve_footprint_path};
use crate::tools::pcb_board::{
    attempt_ipc_write, attempt_ipc_write_naming, BoardWrite, NoLiveBoard,
};
use crate::tools::pcb_copper_check::{self, CheckOptions, Checked, PoseRequest};
use crate::tools::{
    get_path, require_array, require_f64, require_str, require_u64, with_board_ipc_classified,
    with_bound_board_ipc_classified, BoardBinding, ToolContext, ToolDef,
};
use anyhow::Context;
use copperpour_ipc::client::KiCadIpcClient;
use copperpour_sexp::layers::{flip_layer, CopperStack, FlipLayerError};
use copperpour_sexp::writer::{
    apply_edits, find_balanced_block, find_block_starts, find_direct_child_blocks, new_uuid,
    read_consistent, write_atomic_if_unchanged,
};
use copperpour_sexp::SexpEdit;
use prost::Message;
use serde_json::json;
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};
use std::path::{Path, PathBuf};

macro_rules! ipc {
    ($ctx:expr, $args:expr, |$c:ident| $body:expr) => {
        ipc!($ctx, $args, |$c, _| $body)
    };
    // The closure also receives the document KiCad resolved for the board.
    ($ctx:expr, $args:expr, |$c:ident, $doc:pat_param| $body:expr) => {{
        let requested_board = get_path($args, "board")?;
        match with_bound_board_ipc_classified($ctx, &requested_board, move |$c, $doc| $body).await?
        {
            Ok(BoardBinding::Bound(v)) => v,
            Ok(BoardBinding::Unserved { absence, message }) => {
                return Ok(crate::tools::ipc_unserved_handler_error(absence, message));
            }
            Err(failure) => return Ok(crate::tools::ipc_failure_result(failure)),
        }
    }};
}

// ─── Footprint-library resolution ───────────────────────────────────────────

/// Read the library source of `lib_id` (`Library:Footprint`), resolving it
/// through the project's fp-lib-table (the board's directory), then the global
/// table, then the conventional KiCad library directories — the lookup that
/// `library::resolve_footprint_path` owns.
pub(crate) fn resolve_footprint_source(lib_id: &str, board: &Path) -> anyhow::Result<String> {
    let path = resolve_footprint_file(lib_id, board)?;
    std::fs::read_to_string(&path)
        .map_err(|error| anyhow::anyhow!("failed to read {}: {error}", path.display()))
}

/// The resolution half of [`resolve_footprint_source`], for a caller that
/// reports "does not resolve" and "cannot be read" as different failures.
pub(crate) fn resolve_footprint_file(lib_id: &str, board_path: &Path) -> anyhow::Result<PathBuf> {
    let (nickname, entry) = lib_id.split_once(':').ok_or_else(|| {
        anyhow::anyhow!("footprint must use Library:Footprint syntax, got '{lib_id}'")
    })?;
    if nickname.is_empty() || entry.is_empty() {
        anyhow::bail!("footprint must use a non-empty Library:Footprint identifier");
    }
    super::library::resolve_footprint_path(lib_id, board_path.parent())
        .map_err(|message| anyhow::anyhow!(message))
}

/// Resolve and read a library footprint for a tool handler, keeping the
/// existing refusal text. A bad id is `InvalidArgument` on `field` when the
/// caller supplied it; with `field` `None` the id came from the board (KiCad
/// saves footprints without a library nickname), so it is `FileNotFound` like
/// any other id that resolves to no library file.
fn read_footprint_source_or_error(
    lib_id: &str,
    board_path: &Path,
    field: Option<&str>,
) -> Result<(PathBuf, String), CallToolResult> {
    let path = match resolve_footprint_file(lib_id, board_path) {
        Ok(path) => path,
        Err(error) => {
            let message = error.to_string();
            let is_bad_syntax = lib_id
                .split_once(':')
                .is_none_or(|(nickname, entry)| nickname.is_empty() || entry.is_empty());
            let kind = match field {
                Some(field) if is_bad_syntax => crate::mcp::error::ToolErrorKind::InvalidArgument {
                    field: field.to_string(),
                    reason: message.clone(),
                },
                _ => crate::mcp::error::ToolErrorKind::FileNotFound {
                    path: lib_id.to_string(),
                    absolute_path: String::new(),
                },
            };
            return Err(CallToolResult::error_kind(kind, message));
        }
    };
    let source = std::fs::read_to_string(&path).map_err(|error| {
        CallToolResult::error_kind(
            crate::mcp::error::ToolErrorKind::file_not_found(&path),
            format!("failed to read {}: {error}", path.display()),
        )
    })?;
    Ok((path, source))
}

/// Structured rejection for any back-side (`B.*`) placement layer.
///
/// Placing on the back is not a layer rename: KiCAD's flip mirrors the
/// footprint's geometry (pad X positions negate, every front layer swaps with
/// its back counterpart per item). Until Copperpour implements that mirror,
/// Read a `[{x, y}, …]` argument into footprint-local millimetre pairs.
///
/// Each rejection names the offending index, because "each point needs a
/// numeric 'x'" on a twelve-vertex courtyard tells the caller nothing about
/// which vertex to fix.
fn parse_points(value: &serde_json::Value) -> Result<Vec<(f64, f64)>, CallToolResult> {
    let invalid = |field: &str, reason: String| {
        CallToolResult::error_kind(
            crate::mcp::error::ToolErrorKind::InvalidArgument {
                field: field.to_string(),
                reason: reason.clone(),
            },
            format!("Argument '{field}' is invalid: {reason}"),
        )
    };

    let array = value
        .as_array()
        .ok_or_else(|| invalid("points", "missing or not an array of {x, y}".to_string()))?;

    let mut points = Vec::with_capacity(array.len());
    for (i, p) in array.iter().enumerate() {
        let x = p["x"]
            .as_f64()
            .ok_or_else(|| invalid("points", format!("point {i} has no numeric 'x'")))?;
        let y = p["y"]
            .as_f64()
            .ok_or_else(|| invalid("points", format!("point {i} has no numeric 'y'")))?;
        points.push((x, y));
    }
    Ok(points)
}

/// pretending to support `B.Cu` silently produces wrong copper, so the layer
/// is refused up front — before anything is resolved, sent, or written.
fn back_side_layer_error(layer: &str) -> Option<CallToolResult> {
    if !layer.starts_with("B.") {
        return None;
    }
    Some(CallToolResult::error_kind(
        crate::mcp::error::ToolErrorKind::InvalidArgument {
            field: "layer".to_string(),
            reason: format!("back-side placement on '{layer}' is not yet supported"),
        },
        format!(
            "Cannot place on '{layer}': back-side placement is not yet supported, \
             because a correct flip must mirror the footprint geometry rather than \
             just rename its layers. Place the footprint on F.Cu and flip it to the \
             back in KiCAD (select it and press F)."
        ),
    ))
}

pub(crate) fn escape_sexp_string(value: &str) -> String {
    value.replace('\\', "\\\\").replace('"', "\\\"")
}

fn replace_quoted_after(source: &mut String, marker: &str, value: &str) -> anyhow::Result<()> {
    let start = source
        .find(marker)
        .map(|offset| offset + marker.len())
        .ok_or_else(|| anyhow::anyhow!("footprint library data is missing {marker}"))?;
    let bytes = source.as_bytes();
    let mut escaped = false;
    let end = (start..bytes.len())
        .find(|index| {
            let byte = bytes[*index];
            if escaped {
                escaped = false;
                false
            } else if byte == b'\\' {
                escaped = true;
                false
            } else {
                byte == b'"'
            }
        })
        .ok_or_else(|| anyhow::anyhow!("unterminated quoted value after {marker}"))?;
    source.replace_range(start..end, &escape_sexp_string(value));
    Ok(())
}

fn replace_reference(source: &mut String, reference: &str) -> anyhow::Result<()> {
    for marker in ["(property \"Reference\" \"", "(fp_text reference \""] {
        if source.contains(marker) {
            return replace_quoted_after(source, marker, reference);
        }
    }
    anyhow::bail!("footprint library data has no Reference property or fp_text")
}

#[allow(clippy::too_many_arguments)]
fn prepare_footprint_source(
    source: &str,
    lib_id: &str,
    reference: &str,
    value: Option<&str>,
    x: f64,
    y: f64,
    rotation: f64,
    layer: &str,
) -> anyhow::Result<String> {
    // No back-side placement: a correct F.Cu→B.Cu flip mirrors the geometry
    // (pad X positions negate, layers swap per item) the way KiCAD's own flip
    // does. A textual layer swap produces wrong copper, so it is refused
    // outright — see back_side_layer_error.
    if layer != "F.Cu" {
        anyhow::bail!(
            "footprints can only be placed on F.Cu (back-side placement is not yet \
             supported because a correct flip must mirror the geometry), got '{layer}'"
        );
    }
    let mut prepared = source.to_string();
    replace_quoted_after(&mut prepared, "(footprint \"", lib_id)?;
    replace_reference(&mut prepared, reference)?;
    if let Some(value) = value {
        replace_quoted_after(&mut prepared, "(property \"Value\" \"", value)?;
    }
    replace_quoted_after(&mut prepared, "(layer \"", layer)?;
    let layer_start = prepared
        .find("(layer \"")
        .context("footprint library data has no root layer")?;
    let layer_end = prepared[layer_start..]
        .find(')')
        .map(|offset| layer_start + offset + 1)
        .context("footprint root layer is unterminated")?;
    prepared.insert_str(layer_end, &format!("\n\t(at {x} {y} {rotation})"));
    copperpour_sexp::parse_sexp(&prepared)
        .context("prepared footprint is not valid S-expression")?;
    Ok(prepared)
}

pub(crate) fn extract_pad_definitions(
    source: &str,
) -> anyhow::Result<Vec<copperpour_ipc::IpcPadDefinition>> {
    let footprint = copperpour_sexp::parse_sexp(source)?;
    footprint
        .find_all("pad")
        .into_iter()
        .map(|pad| {
            let required = |index: usize, label: &str| {
                pad.get(index)
                    .and_then(copperpour_sexp::SexpNode::as_str)
                    .ok_or_else(|| anyhow::anyhow!("footprint pad is missing {label}"))
            };
            let shape = required(3, "shape")?.to_string();
            // A custom pad's anchor and primitives through the shared model
            // (#679); what KiCad's pad message cannot carry is refused with
            // the pad named.
            let custom = if shape == "custom" {
                let number = required(1, "number")?;
                let custom = super::pad_primitives::CustomPadShape::from_pad(pad)
                    .and_then(|custom| custom.to_ipc())
                    .map_err(|reason| anyhow::anyhow!("custom pad '{number}': {reason}"))?;
                Some(custom)
            } else {
                None
            };
            let at = pad
                .find("at")
                .context("footprint pad is missing its position")?;
            let size = pad
                .find("size")
                .context("footprint pad is missing its size")?;
            let layers = pad
                .find("layers")
                .context("footprint pad is missing its layer set")?
                .children()
                .unwrap_or_default()
                .iter()
                .skip(1)
                .filter_map(copperpour_sexp::SexpNode::as_str)
                .map(str::to_string)
                .collect();
            let (drill_x, drill_y, drill_oval) = match pad.find("drill") {
                Some(drill)
                    if drill.get(1).and_then(copperpour_sexp::SexpNode::as_str) == Some("oval") =>
                {
                    (
                        drill.get_f64(2),
                        drill.get_f64(3).or_else(|| drill.get_f64(2)),
                        true,
                    )
                }
                Some(drill) => (
                    drill.get_f64(1),
                    drill.get_f64(2).or_else(|| drill.get_f64(1)),
                    false,
                ),
                None => (None, None, false),
            };
            Ok(copperpour_ipc::IpcPadDefinition {
                number: required(1, "number")?.to_string(),
                pad_type: required(2, "type")?.to_string(),
                shape,
                x: at
                    .get_f64(1)
                    .context("footprint pad has an invalid X position")?,
                y: at
                    .get_f64(2)
                    .context("footprint pad has an invalid Y position")?,
                rotation: at.get_f64(3).unwrap_or(0.0),
                size_x: size
                    .get_f64(1)
                    .context("footprint pad has an invalid width")?,
                size_y: size
                    .get_f64(2)
                    .context("footprint pad has an invalid height")?,
                drill_x,
                drill_y,
                drill_oval,
                custom,
                layers,
                roundrect_ratio: pad.find_f64("roundrect_rratio").unwrap_or(0.0),
            })
        })
        .collect()
}

// ─── Footprint graphics extraction ───────────────────────────────────────────

/// `(start x y)`-style point child of a graphic node.
fn graphic_point(
    node: &copperpour_sexp::SexpNode,
    tag: &str,
    kind: &str,
) -> anyhow::Result<(f64, f64)> {
    let point = node
        .find(tag)
        .ok_or_else(|| anyhow::anyhow!("footprint {kind} is missing its ({tag} …)"))?;
    Ok((
        point
            .get_f64(1)
            .ok_or_else(|| anyhow::anyhow!("footprint {kind} has an invalid {tag} X"))?,
        point
            .get_f64(2)
            .ok_or_else(|| anyhow::anyhow!("footprint {kind} has an invalid {tag} Y"))?,
    ))
}

fn graphic_layer(node: &copperpour_sexp::SexpNode, kind: &str) -> anyhow::Result<String> {
    node.find_str("layer")
        .map(str::to_string)
        .ok_or_else(|| anyhow::anyhow!("footprint {kind} is missing its layer"))
}

/// Stroke width in mm: modern `(stroke (width w) …)`, legacy bare `(width w)`.
/// KiCad's default silkscreen line width stands in when neither is present.
fn graphic_stroke_width(node: &copperpour_sexp::SexpNode) -> f64 {
    node.find("stroke")
        .and_then(|stroke| stroke.find_f64("width"))
        .or_else(|| node.find_f64("width"))
        .unwrap_or(0.12)
}

/// Line style from `(stroke (type …))`. KiCad 10 loads a stroke without a
/// type, and the legacy bare `(width w)`, as solid.
fn graphic_line_style(node: &copperpour_sexp::SexpNode) -> copperpour_ipc::IpcLineStyle {
    node.find("stroke")
        .and_then(|stroke| stroke.find_str("type"))
        .and_then(copperpour_ipc::IpcLineStyle::from_kicad_name)
        .unwrap_or(copperpour_ipc::IpcLineStyle::Solid)
}

/// `(fill yes)` (KiCad 8+) or legacy `(fill solid)`.
fn graphic_filled(node: &copperpour_sexp::SexpNode) -> bool {
    matches!(node.find_str("fill"), Some("yes") | Some("solid"))
}

/// KiCad keeps footprint text upright unless it is `(unlocked yes)`, or, as
/// KiCad 7 and earlier wrote it, `(at x y [angle] unlocked)`.
fn text_keeps_upright(node: &copperpour_sexp::SexpNode) -> bool {
    let legacy = node.find("at").is_some_and(|at| {
        at.children()
            .unwrap_or_default()
            .iter()
            .any(|atom| atom.as_str() == Some("unlocked"))
    });
    !legacy && node.find_str("unlocked") != Some("yes")
}

/// `(hide yes)` (modern) or a bare `hide` atom (legacy).
fn text_hidden(node: &copperpour_sexp::SexpNode) -> bool {
    node.find_str("hide") == Some("yes")
        || node
            .children()
            .unwrap_or_default()
            .iter()
            .any(|child| child.as_str() == Some("hide"))
}

/// `(effects (font (size h w)))` glyph size, defaulting to KiCad's 1 mm.
fn text_size(node: &copperpour_sexp::SexpNode) -> f64 {
    node.find("effects")
        .and_then(|effects| effects.find("font"))
        .and_then(|font| font.find("size"))
        .and_then(|size| size.get_f64(1))
        .unwrap_or(1.0)
}

/// The rest of a text's `(effects …)`: glyph width, bold, italic, line
/// spacing and `(justify …)`. KiCad writes the font size as `(size height
/// width)`. A flag is `(bold yes)` from KiCad 8, a bare `bold` before.
fn text_style(node: &copperpour_sexp::SexpNode) -> copperpour_ipc::IpcTextStyle {
    use copperpour_ipc::{IpcTextHorizontal, IpcTextStyle, IpcTextVertical};

    let mut style = IpcTextStyle::default();
    let Some(effects) = node.find("effects") else {
        return style;
    };
    if let Some(font) = effects.find("font") {
        let flag = |name: &str| {
            font.find_str(name) == Some("yes")
                || font
                    .children()
                    .unwrap_or_default()
                    .iter()
                    .any(|child| child.as_str() == Some(name))
        };
        style.bold = flag("bold");
        style.italic = flag("italic");
        if let Some(spacing) = font.find_f64("line_spacing") {
            style.line_spacing = spacing;
        }
        if let Some(size) = font.find("size") {
            if let (Some(height), Some(width)) = (size.get_f64(1), size.get_f64(2)) {
                style.width = (width != height).then_some(width);
            }
        }
    }
    if let Some(justify) = effects.find("justify") {
        for value in justify.children().unwrap_or_default().iter().skip(1) {
            match value.as_str() {
                Some("left") => style.horizontal = IpcTextHorizontal::Left,
                Some("right") => style.horizontal = IpcTextHorizontal::Right,
                Some("top") => style.vertical = IpcTextVertical::Top,
                Some("bottom") => style.vertical = IpcTextVertical::Bottom,
                Some("mirror") => style.mirrored = true,
                _ => {}
            }
        }
    }
    style
}

/// Explicit font stroke width, falling back to KiCad's 15%-of-size default.
fn text_stroke_width(node: &copperpour_sexp::SexpNode) -> f64 {
    node.find("effects")
        .and_then(|effects| effects.find("font"))
        .and_then(|font| font.find_f64("thickness"))
        .unwrap_or_else(|| text_size(node) * 0.15)
}

/// Text position and angle from `(at x y [rot])`.
fn text_at(node: &copperpour_sexp::SexpNode, kind: &str) -> anyhow::Result<((f64, f64), f64)> {
    let at = node
        .find("at")
        .ok_or_else(|| anyhow::anyhow!("footprint {kind} is missing its position"))?;
    Ok((
        (
            at.get_f64(1)
                .ok_or_else(|| anyhow::anyhow!("footprint {kind} has an invalid X position"))?,
            at.get_f64(2)
                .ok_or_else(|| anyhow::anyhow!("footprint {kind} has an invalid Y position"))?,
        ),
        at.get_f64(3).unwrap_or(0.0),
    ))
}

/// Parse a footprint's drawable children — `fp_line`, `fp_rect`, `fp_circle`,
/// `fp_arc`, `fp_poly` and visible `fp_text`/`property` texts — into
/// footprint-local [`copperpour_ipc::IpcGraphicDefinition`]s.
///
/// The typed placement path previously shipped pads only, so a placed part had
/// no courtyard, silkscreen, or fab drawing: courtyard DRC had nothing to
/// check and KiCad's `lib_footprint_mismatch` flagged every placement.
///
/// `Reference` and `Value` properties are excluded — `build_footprint_item`
/// already carries those as first-class fields.
/// Footprint-local Reference/Value text anchors from the library source, so
/// placed parts keep the library's text layout (a synthesized offset put the
/// Reference on the part's own silkscreen — silk_overlap in live DRC).
pub(crate) fn extract_field_placement(source: &str) -> copperpour_ipc::IpcFieldPlacement {
    let mut placement = copperpour_ipc::IpcFieldPlacement::default();
    let Ok(footprint) = copperpour_sexp::parse_sexp(source) else {
        return placement;
    };
    for prop in footprint.find_all("property") {
        let Some(name) = prop.get(1).and_then(|n| n.as_str()) else {
            continue;
        };
        let Some(at) = prop.find("at") else {
            continue;
        };
        let num = |i: usize| {
            at.get(i)
                .and_then(|v| v.as_str())
                .and_then(|s| s.parse::<f64>().ok())
        };
        let (x, y) = (num(1), num(2));
        let rot = num(3).unwrap_or(0.0);
        if let (Some(x), Some(y)) = (x, y) {
            match name {
                "Reference" => placement.reference_at = Some((x, y, rot)),
                "Value" => placement.value_at = Some((x, y, rot)),
                _ => {}
            }
        }
    }
    placement
}

pub(crate) fn extract_graphic_definitions(
    source: &str,
) -> anyhow::Result<Vec<copperpour_ipc::IpcGraphicDefinition>> {
    extract_graphic_definitions_with_properties(source, true)
}

pub(crate) fn extract_graphic_definitions_without_properties(
    source: &str,
) -> anyhow::Result<Vec<copperpour_ipc::IpcGraphicDefinition>> {
    extract_graphic_definitions_with_properties(source, false)
}

fn extract_graphic_definitions_with_properties(
    source: &str,
    include_properties: bool,
) -> anyhow::Result<Vec<copperpour_ipc::IpcGraphicDefinition>> {
    use copperpour_ipc::IpcGraphicDefinition as Graphic;
    let footprint = copperpour_sexp::parse_sexp(source)?;
    let mut graphics = Vec::new();

    for line in footprint.find_all("fp_line") {
        graphics.push(Graphic::Line {
            start: graphic_point(line, "start", "fp_line")?,
            end: graphic_point(line, "end", "fp_line")?,
            layer: graphic_layer(line, "fp_line")?,
            width: graphic_stroke_width(line),
            line_style: graphic_line_style(line),
        });
    }
    for rect in footprint.find_all("fp_rect") {
        graphics.push(Graphic::Rect {
            start: graphic_point(rect, "start", "fp_rect")?,
            end: graphic_point(rect, "end", "fp_rect")?,
            layer: graphic_layer(rect, "fp_rect")?,
            width: graphic_stroke_width(rect),
            line_style: graphic_line_style(rect),
            filled: graphic_filled(rect),
        });
    }
    for circle in footprint.find_all("fp_circle") {
        graphics.push(Graphic::Circle {
            center: graphic_point(circle, "center", "fp_circle")?,
            end: graphic_point(circle, "end", "fp_circle")?,
            layer: graphic_layer(circle, "fp_circle")?,
            width: graphic_stroke_width(circle),
            line_style: graphic_line_style(circle),
            filled: graphic_filled(circle),
        });
    }
    for arc in footprint.find_all("fp_arc") {
        graphics.push(Graphic::Arc {
            start: graphic_point(arc, "start", "fp_arc")?,
            mid: graphic_point(arc, "mid", "fp_arc")?,
            end: graphic_point(arc, "end", "fp_arc")?,
            layer: graphic_layer(arc, "fp_arc")?,
            width: graphic_stroke_width(arc),
            line_style: graphic_line_style(arc),
        });
    }
    for poly in footprint.find_all("fp_poly") {
        let pts = poly
            .find("pts")
            .ok_or_else(|| anyhow::anyhow!("footprint fp_poly is missing its (pts …)"))?;
        let points = pts
            .children()
            .unwrap_or_default()
            .iter()
            .filter(|node| node.head() == Some("xy"))
            .map(|node| {
                Ok((
                    node.get_f64(1)
                        .ok_or_else(|| anyhow::anyhow!("footprint fp_poly has an invalid X"))?,
                    node.get_f64(2)
                        .ok_or_else(|| anyhow::anyhow!("footprint fp_poly has an invalid Y"))?,
                ))
            })
            .collect::<anyhow::Result<Vec<_>>>()?;
        graphics.push(Graphic::Poly {
            points,
            layer: graphic_layer(poly, "fp_poly")?,
            width: graphic_stroke_width(poly),
            line_style: graphic_line_style(poly),
            filled: graphic_filled(poly),
        });
    }
    for text in footprint.find_all("fp_text") {
        let kind = text.get(1).and_then(copperpour_sexp::SexpNode::as_str);
        if matches!(kind, Some("reference" | "value")) || text_hidden(text) {
            continue;
        }
        let content = text
            .get(2)
            .and_then(copperpour_sexp::SexpNode::as_str)
            .ok_or_else(|| anyhow::anyhow!("footprint fp_text is missing its text"))?;
        let (position, rotation) = text_at(text, "fp_text")?;
        graphics.push(Graphic::Text {
            text: content.to_string(),
            position,
            rotation,
            layer: graphic_layer(text, "fp_text")?,
            size: text_size(text),
            stroke_width_mm: text_stroke_width(text),
            keep_upright: text_keeps_upright(text),
            style: text_style(text),
        });
    }
    let properties = if include_properties {
        footprint.find_all("property")
    } else {
        Vec::new()
    };
    for property in properties {
        let name = property.get(1).and_then(copperpour_sexp::SexpNode::as_str);
        // Reference and Value travel as first-class fields; hidden built-ins
        // (Footprint, Datasheet, …) are not drawn.
        if matches!(name, Some("Reference") | Some("Value")) || text_hidden(property) {
            continue;
        }
        let Some(content) = property.get(2).and_then(copperpour_sexp::SexpNode::as_str) else {
            continue;
        };
        let Ok((position, rotation)) = text_at(property, "property") else {
            continue;
        };
        let Ok(layer) = graphic_layer(property, "property") else {
            continue;
        };
        graphics.push(Graphic::Text {
            text: content.to_string(),
            position,
            rotation,
            layer,
            size: text_size(property),
            stroke_width_mm: text_stroke_width(property),
            keep_upright: text_keeps_upright(property),
            style: text_style(property),
        });
    }
    Ok(graphics)
}

// ─── Library footprint → board footprint (file-editing fallback) ─────────────
//
// Used ONLY when the IPC transport is unreachable (unconfigured socket or
// failed dial/send): a live KiCad must never have the board file edited
// behind its back. Ported from emolitor's PR #66.

/// Build a board-ready `(footprint …)` block for `lib_id`.
///
/// A library `.kicad_mod` is a complete footprint definition sitting at the
/// origin with a `REF**` placeholder reference. Placing it on a board means
/// renaming it to the full `Library:Footprint` id, stamping in a position,
/// rotation and fresh UUID, and substituting the real reference designator.
///
/// KiCAD's own parser then handles the pads and graphics, which is why the
/// whole definition is forwarded rather than reconstructed.
///
/// `content` is the library text the caller already prepared — for a pre-6.0
/// file, the copy `kicad-cli fp upgrade` has just modernised, which must not
/// be read from disk again in its original form. `path` is where the text
/// came from, used only to name the placed part's library.
#[allow(clippy::too_many_arguments)]
fn board_footprint_sexp(
    content: &str,
    path: &Path,
    lib_id: &str,
    x: f64,
    y: f64,
    rotation: f64,
    layer: &str,
    reference: Option<&str>,
) -> Result<String, String> {
    let name_span = footprint_name_span(content).ok_or_else(|| {
        format!(
            "{} does not start with a (footprint \"NAME\" …) block",
            path.display()
        )
    })?;

    // Board footprints carry the full library id, not the bare footprint name.
    // The declared name is the span without its surrounding quotes.
    let declared = &content[name_span.start + 1..name_span.end - 1];
    let mut out = String::with_capacity(content.len() + 128);
    out.push_str(&content[..name_span.start]);
    out.push_str(&quote_sexp_string(&board_lib_id(lib_id, path, declared)));
    out.push_str(&format!(
        "\n\t(at {x} {y} {rotation})\n\t(uuid \"{}\")",
        new_uuid()
    ));
    out.push_str(&content[name_span.end..]);

    if rotation != 0.0 {
        out = apply_rotation_to_children(&out, rotation);
    }
    if let Some(reference) = reference {
        replace_reference(&mut out, reference).map_err(|error| error.to_string())?;
    }
    if layer != "F.Cu" {
        out = replace_footprint_layer(&out, layer);
    }

    Ok(out)
}

/// The name a board entry should carry for a footprint read from `path`.
///
/// `resolve_footprint_path` also accepts a bare filesystem path, which is
/// convenient for a caller holding a `.kicad_mod` directly. That path must not
/// reach the board file: `(footprint "C:\…\R_0805_2012Metric.kicad_mod")` is
/// not a library identifier, and KiCad reports the placed part as a broken
/// library link. This function is therefore total — every branch returns
/// something that is not a path.
///
/// Preference order, most authoritative first:
///
/// 1. The caller already gave a `Library:Footprint` id — use it verbatim.
/// 2. The fp-lib-table maps a nickname to the containing directory. Only the
///    table can answer this: KiCad lets any nickname point at any path, so
///    `MyParts` may well live in `vendor.pretty`, and guessing from the
///    directory would silently mislink the part.
/// 3. The conventional `<nickname>.pretty/` layout. The library is not
///    registered, so the link will be broken either way, but this is the
///    nickname the user gets when they do register it.
/// 4. Neither — fall back to a bare footprint name, which links to nothing but
///    is at least a valid name. The library file's own is used when it is not
///    itself path-like; otherwise the file stem, which cannot contain a
///    separator.
fn board_lib_id(reference: &str, path: &Path, declared: &str) -> String {
    if is_lib_id(reference) {
        return reference.to_string();
    }
    let stem = path
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();

    if let Some(dir) = path.parent() {
        if let Some(nick) = footprint_lib_nickname_for_dir(dir) {
            return format!("{nick}:{stem}");
        }
        if let Some(nick) = pretty_dir_nickname(dir) {
            return format!("{nick}:{stem}");
        }
    }

    if declared.is_empty() || declared.contains('/') || declared.contains('\\') {
        stem
    } else {
        declared.to_string()
    }
}

/// The nickname a conventional `<nickname>.pretty` directory implies.
///
/// Matched case-insensitively: KiCad's own libraries are lowercase `.pretty`,
/// but Windows and macOS filesystems are case-insensitive, so a `.Pretty` on
/// disk is the same directory to KiCad and should not change the answer.
fn pretty_dir_nickname(dir: &Path) -> Option<String> {
    let name = dir.file_name()?.to_string_lossy().into_owned();
    let cut = name.len().checked_sub(".pretty".len())?;
    name[cut..]
        .eq_ignore_ascii_case(".pretty")
        .then(|| name[..cut].to_string())
        .filter(|nick| !nick.is_empty())
}

/// Fold the footprint's placement rotation into its pads and text items.
///
/// KiCad stores each pad's and text item's *absolute* orientation while their
/// positions stay in unrotated footprint-local coordinates — a `C_0603` placed
/// at -90° keeps `(at -0.775 0 270)` on pad 1. Omitting this leaves the pad
/// shapes unrotated relative to the body and makes KiCad's
/// `lib_footprint_mismatch` check fire.
///
/// Text is additionally kept readable: KiCad flips an angle that would leave a
/// label upside down by 180°, so a -90° footprint carries `90` on its reference.
fn apply_rotation_to_children(content: &str, rotation: f64) -> String {
    let mut out = content.to_string();

    for tag in ["pad", "property", "fp_text"] {
        let readable = tag != "pad";
        // Rewrite back-to-front so earlier byte offsets stay valid.
        let starts: Vec<usize> = find_block_starts(&out, tag);
        for start in starts.into_iter().rev() {
            let Some((bstart, bend)) = find_balanced_block(&out, start) else {
                continue;
            };
            // The block's own `(at …)` is its first — nested ones (a pad's
            // `(primitives …)`, for instance) come later.
            let Some(at_start) = find_block_starts(&out[bstart..bend], "at")
                .first()
                .map(|i| bstart + i)
            else {
                continue;
            };
            let Some((astart, aend)) = find_balanced_block(&out, at_start) else {
                continue;
            };
            let Some(rewritten) = rotate_at_block(&out[astart..aend], rotation, readable) else {
                continue;
            };
            out.replace_range(astart..aend, &rewritten);
        }
    }
    out
}

/// Rewrite `(at x y [angle])`, adding `rotation` to the angle.
///
/// Returns `None` when the block does not look like a positional `at`.
fn rotate_at_block(block: &str, rotation: f64, readable: bool) -> Option<String> {
    let inner = block.strip_prefix('(')?.strip_suffix(')')?;
    let mut parts = inner.split_whitespace();
    if parts.next()? != "at" {
        return None;
    }
    let x: f64 = parts.next()?.parse().ok()?;
    let y: f64 = parts.next()?.parse().ok()?;
    let existing: f64 = parts.next().and_then(|a| a.parse().ok()).unwrap_or(0.0);
    if parts.next().is_some() {
        return None; // `(at …)` with unexpected extra tokens — leave alone.
    }

    let mut angle = (existing + rotation).rem_euclid(360.0);
    if readable && angle > 90.0 && angle <= 270.0 {
        angle -= 180.0;
    }
    Some(format_at(x, y, angle))
}

/// Normalise a footprint's *root* orientation the way KiCad's writer does,
/// to (-180, 180].
///
/// Measured, not assumed: rotating `R1` to 247.5° through the closed-board
/// path and then letting KiCad re-save the board, KiCad wrote `-112.5` for the
/// root `(at …)` and left both pad angles at `247.5` untouched. Same angle,
/// different spelling — but writing the un-normalised form means the user's
/// next save in KiCad produces a diff on a footprint nobody touched, and it
/// makes the file path disagree with the IPC path, which ends up normalised
/// because KiCad itself does it.
///
/// Deliberately not applied to children: KiCad does not normalise those.
pub(crate) fn normalize_root_angle(degrees: f64) -> f64 {
    let wrapped = degrees.rem_euclid(360.0);
    if wrapped > 180.0 {
        wrapped - 360.0
    } else {
        wrapped
    }
}

/// Render `(at x y angle)`, dropping a zero angle as KiCad's writer does and
/// trimming trailing zeros from the decimals.
fn format_at(x: f64, y: f64, angle: f64) -> String {
    let n = |v: f64| {
        let s = format!("{v:.6}");
        let s = s.trim_end_matches('0').trim_end_matches('.').to_string();
        if s == "-0" {
            "0".to_string()
        } else {
            s
        }
    };
    if angle == 0.0 {
        format!("(at {} {})", n(x), n(y))
    } else {
        format!("(at {} {} {})", n(x), n(y), n(angle))
    }
}

/// Byte range of the quoted name in the leading `(footprint "NAME"` header,
/// including the surrounding quotes.
fn footprint_name_span(content: &str) -> Option<std::ops::Range<usize>> {
    let block = *find_block_starts(content, "footprint").first()?;
    let after_tag = block + "(footprint".len();
    let rel = content[after_tag..].find('"')?;
    let start = after_tag + rel;
    let end = start + 1 + content[start + 1..].find('"')?;
    Some(start..end + 1)
}

/// Quote and escape `value` as an S-expression string literal.
fn quote_sexp_string(value: &str) -> String {
    format!("\"{}\"", escape_sexp_string(value))
}

/// Replace the footprint's own `(layer "…")` — the first `layer` block that is a
/// direct child of the footprint, not one belonging to a pad or graphic.
///
/// Note this only retargets the footprint; a true F.Cu↔B.Cu flip would also
/// have to mirror every child item, which is why back-side placement is
/// rejected before this code can run (see `back_side_layer_error`).
fn replace_footprint_layer(content: &str, layer: &str) -> String {
    let Some(name) = footprint_name_span(content) else {
        return content.to_string();
    };
    let Some(start) = find_block_starts(content, "layer")
        .into_iter()
        .find(|&i| i > name.end)
    else {
        return content.to_string();
    };
    let Some((bstart, bend)) = find_balanced_block(content, start) else {
        return content.to_string();
    };

    let mut out = String::with_capacity(content.len());
    out.push_str(&content[..bstart]);
    out.push_str(&format!("(layer {})", quote_sexp_string(layer)));
    out.push_str(&content[bend..]);
    out
}

/// Insert `blocks` just inside the board's closing paren and write it back,
/// refusing to write anything that is not one complete `(kicad_pcb …)` form.
///
/// The insert point is `rfind(')')`, which is only the right place if the file
/// really is a single closed form. Checking the result before committing it
/// means a board that was already truncated — or a footprint block that was —
/// fails loudly instead of being written back over the user's file in a state
/// KiCad can no longer open.
///
/// Like the rest of `copperpour-sexp`, this treats parens as syntax everywhere: a
/// `#`-commented paren would be miscounted. KiCad does not write comments into
/// `.kicad_pcb`, and no reader in this workspace understands them either, so
/// the assumption is at least consistent.
fn insert_into_board(board_path: &Path, blocks: &[String]) -> anyhow::Result<()> {
    let content = read_consistent(board_path)?;
    let existing_references = footprint_references(&content)?;
    let mut inserted_references = HashSet::new();
    for block in blocks {
        for reference in footprint_references(block)? {
            if existing_references.contains(&reference)
                || !inserted_references.insert(reference.clone())
            {
                anyhow::bail!(
                    "Footprint reference '{}' already exists on the board",
                    reference
                );
            }
        }
    }
    // KiCad writes these files CRLF on Windows — its bundled .kicad_mod
    // libraries are CRLF throughout — so an inserted block joined with bare LF
    // would leave the board with two conventions in it.
    let eol = if content.contains("\r\n") {
        "\r\n"
    } else {
        "\n"
    };
    let close_pos = content.rfind(')').unwrap_or(content.len());
    let joined: String = blocks
        .iter()
        .map(|b| format!("{eol}{}", indent_block(b.trim_end(), "\t", eol)))
        .collect();
    let new_content = apply_edits(content.clone(), vec![SexpEdit::insert(close_pos, joined)]);

    if let Err(why) = check_single_board_form(&new_content) {
        anyhow::bail!(
            "Refusing to write {}: {}. The board file was left untouched.",
            board_path.display(),
            why
        );
    }

    persist_board_replacement(board_path, &content, &new_content)?;
    Ok(())
}

/// One prepared footprint placement shared by the IPC and guarded file paths.
///
/// Callers choose whether the definition came from a KiCad library or from
/// deliberately supplied inline geometry, but they do not orchestrate the
/// parser, IPC extraction, board serialization, and duplicate-safe insertion
/// helpers independently.
#[derive(Clone)]
pub(crate) struct PreparedFootprintPlacement {
    pub(crate) value: String,
    pub(crate) pads: Vec<copperpour_ipc::IpcPadDefinition>,
    pub(crate) graphics: Vec<copperpour_ipc::IpcGraphicDefinition>,
    pub(crate) fields: copperpour_ipc::IpcFieldPlacement,
    /// What the library carries besides pads, drawings and fields — 3D
    /// models, mounting attributes, description and keywords. `None` for
    /// inline geometry, which has no library behind it.
    library: Option<LibraryFootprintData>,
    /// The orientation the footprint is built with on F.Cu. For a back-side
    /// placement it is the negation of the requested orientation, because
    /// KiCad's top-bottom flip negates it again.
    front_rotation: f64,
    /// Whether the part belongs on B.Cu: flipped by KiCad over IPC, mirrored
    /// by [`Self::file_block`] for the closed board.
    on_back: bool,
    /// The F.Cu board block for the closed-board path, if prepared with a
    /// project directory.
    board_sexp: Option<String>,
}

/// The library data `update_pcb_from_schematic` already puts on the footprints
/// it places (#789), read with the same readers so that `place_component`
/// gives a part the same 3D models, attributes and description.
#[derive(Clone)]
struct LibraryFootprintData {
    attributes: copperpour_ipc::gen::kiapi::board::types::FootprintAttributes,
    description_and_keywords: copperpour_ipc::gen::kiapi::board::types::FootprintAttributes,
    models: Vec<copperpour_ipc::gen::kiapi::board::types::Footprint3DModel>,
    /// The library's properties, whose Reference and Value give the placed
    /// fields their layer, visibility and text style. When they cannot be
    /// read, the part keeps the fields every footprint gets, and the response
    /// says so rather than refusing the part over its text. A field the
    /// library writes without a position, layer or effects keeps its default
    /// too, as in the sync.
    properties: Result<super::library_footprint::LibraryProperties, String>,
    pad_properties: Vec<super::library::PadFabricationProperty>,
    pad_zones: Vec<super::library_footprint::PadZoneSettings>,
    pad_margins: Vec<super::library_footprint::SolderMargins>,
    margins: super::library_footprint::SolderMargins,
    points: Vec<super::library_footprint::LibraryPoint>,
    /// The library's net-tie pad groups, which KiCad's API keeps from
    /// 10.0.1 (#85).
    net_ties: Vec<copperpour_ipc::gen::kiapi::board::types::NetTieDefinition>,
    unsupported: Vec<String>,
}

impl PreparedFootprintPlacement {
    /// Prepare `source` for placement on `layer`, which must be F.Cu or B.Cu.
    ///
    /// A back-side footprint is prepared on F.Cu at the negated rotation and
    /// then flipped: over IPC by KiCad's own `FlipItems`, and in the closed
    /// file by the same mirror `flip_component` uses. Neither path invents
    /// its own back-side geometry. The file mirror is only attempted when the
    /// file path runs, so geometry it refuses — a 3D model a flip would have
    /// to move — never stops the live path, where KiCad flips it natively.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn from_library(
        source: &str,
        lib_id: &str,
        reference: &str,
        value: Option<&str>,
        x: f64,
        y: f64,
        rotation: f64,
        layer: &str,
        project_dir: Option<&Path>,
    ) -> anyhow::Result<Self> {
        let on_back = match layer {
            "F.Cu" => false,
            "B.Cu" => true,
            other => anyhow::bail!("footprints are placed on F.Cu or B.Cu, got '{other}'"),
        };
        let front_rotation = if on_back {
            normalize_root_angle(-rotation)
        } else {
            rotation
        };
        let prepared = prepare_footprint_source(
            source,
            lib_id,
            reference,
            value,
            x,
            y,
            front_rotation,
            "F.Cu",
        )?;
        let pads = extract_pad_definitions(&prepared)?;
        let graphics = extract_graphic_definitions(&prepared)?;
        let fields = extract_field_placement(&prepared);
        let root = copperpour_sexp::parse_sexp(source).context("invalid footprint S-expression")?;
        let pad_nodes = root.find_all("pad");
        let library = LibraryFootprintData {
            attributes: super::library_footprint::attributes(&root)?,
            description_and_keywords: super::library_footprint::description_and_keywords(&root),
            models: super::library_footprint::models(&root)?,
            properties: super::library_footprint::properties(&root)
                .map_err(|error| format!("{error:#}")),
            pad_properties: super::library_footprint::pad_properties(&pad_nodes, pads.len())?,
            pad_zones: super::library_footprint::pad_zones(&pad_nodes)?,
            pad_margins: super::library_footprint::pad_margins(&root, &pad_nodes)?,
            margins: super::library_footprint::SolderMargins::of_footprint(&root)?,
            points: super::library_footprint::points(&root)?,
            net_ties: super::library_footprint::net_ties(&root)?,
            unsupported: super::library_footprint::unsupported_features(&root)?,
        };
        let board_sexp = if let Some(dir) = project_dir {
            let library_path =
                resolve_footprint_path(lib_id, Some(dir)).map_err(anyhow::Error::msg)?;
            Some(
                board_footprint_sexp(
                    source,
                    &library_path,
                    lib_id,
                    x,
                    y,
                    front_rotation,
                    "F.Cu",
                    Some(reference),
                )
                .map_err(anyhow::Error::msg)?,
            )
        } else {
            None
        };
        let value = value.map(str::to_string).unwrap_or_else(|| {
            lib_id
                .split_once(':')
                .map_or(lib_id, |(_, entry)| entry)
                .to_string()
        });
        Ok(Self {
            value,
            pads,
            graphics,
            fields,
            library: Some(library),
            front_rotation,
            on_back,
            board_sexp,
        })
    }

    /// Prepare `source` for live IPC placement without building a closed-board
    /// file block.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn from_library_for_ipc(
        source: &str,
        lib_id: &str,
        reference: &str,
        value: Option<&str>,
        x: f64,
        y: f64,
        rotation: f64,
        layer: &str,
    ) -> anyhow::Result<Self> {
        Self::from_library(
            source, lib_id, reference, value, x, y, rotation, layer, None,
        )
    }

    pub(crate) fn from_inline(
        value: String,
        pads: Vec<copperpour_ipc::IpcPadDefinition>,
        graphics: Vec<copperpour_ipc::IpcGraphicDefinition>,
        fields: copperpour_ipc::IpcFieldPlacement,
        board_sexp: String,
    ) -> Self {
        Self {
            value,
            pads,
            graphics,
            fields,
            library: None,
            front_rotation: 0.0,
            on_back: false,
            board_sexp: Some(board_sexp),
        }
    }

    /// Whether this footprint carries pad fabrication properties or reference
    /// points that require KiCad 10.0.7+.
    pub(crate) fn needs_kicad_10_0_7(&self) -> bool {
        self.library.as_ref().is_some_and(|lib| {
            super::library_footprint::needs_kicad_10_0_7(&lib.pad_properties, &lib.points)
        })
    }

    /// Whether this footprint has net-tie pad groups, which KiCad's API keeps
    /// only from 10.0.1.
    fn has_net_ties(&self) -> bool {
        self.library
            .as_ref()
            .is_some_and(|lib| !lib.net_ties.is_empty())
    }

    /// Ask the connected KiCad's version only when this footprint has what
    /// not every KiCad 10 release carries: pad fabrication properties or
    /// reference points (10.0.7+), or net-tie pad groups (10.0.1+). Returns
    /// what to send, and the KiCad when it is too old for some of what the
    /// footprint has.
    pub(crate) fn resolve_kicad_carried(
        &self,
        client: &KiCadIpcClient,
    ) -> anyhow::Result<(
        super::library_footprint::CarriedByKiCad,
        Option<copperpour_ipc::IpcKiCadVersion>,
    )> {
        let (needs_10_0_7, has_net_ties) = (self.needs_kicad_10_0_7(), self.has_net_ties());
        if !needs_10_0_7 && !has_net_ties {
            return Ok((Default::default(), None));
        }
        let kicad = client.get_kicad_version()?;
        // Only a footprint that has them sends the 10.0.7 fields, as before.
        let carried = super::library_footprint::CarriedByKiCad {
            fields_10_0_7: needs_10_0_7
                && super::library_footprint::carries_kicad_10_0_7_fields(&kicad),
            net_ties: super::library_footprint::carries_net_ties(&kicad),
        };
        let left_out =
            (needs_10_0_7 && !carried.fields_10_0_7) || (has_net_ties && !carried.net_ties);
        Ok((carried, left_out.then_some(kicad)))
    }

    /// Features of the library footprint that the typed IPC path left out.
    /// `older_kicad` is the connected KiCad when it is too old for some of
    /// them ([`Self::resolve_kicad_carried`]).
    pub(crate) fn not_carried(
        &self,
        older_kicad: Option<&copperpour_ipc::IpcKiCadVersion>,
    ) -> Vec<String> {
        let Some(library) = &self.library else {
            return Vec::new();
        };
        let mut items = library.unsupported.clone();
        if let Err(reason) = &library.properties {
            items.push(format!(
                "library properties (Reference and Value placed with defaults; Datasheet, \
                 Description and custom properties left out): {reason}"
            ));
        }
        if let Some(kicad) = older_kicad {
            items.extend(super::library_footprint::not_carried_10_0_7_reasons(
                &library.pad_properties,
                &self.pads,
                &library.points,
                kicad,
            ));
            items.extend(super::library_footprint::net_ties_not_carried_reason(
                &library.net_ties,
                kicad,
            ));
        }
        items
    }

    /// The F.Cu `FootprintInstance` the IPC path creates, with the library's
    /// 3D models, attributes, description and keywords, pad zone settings,
    /// pad and footprint solder mask and paste margins, fields (Reference,
    /// Value, Datasheet, Description and custom properties), and, as
    /// `carried` says, pad fabrication properties and reference points and
    /// net-tie pad groups on it.
    pub(crate) fn ipc_item(
        &self,
        lib_id: &str,
        reference: &str,
        x: f64,
        y: f64,
        carried: super::library_footprint::CarriedByKiCad,
    ) -> anyhow::Result<prost_types::Any> {
        let item = KiCadIpcClient::build_footprint_item(
            lib_id,
            reference,
            &self.value,
            &self.pads,
            &self.graphics,
            &self.fields,
            x,
            y,
            self.front_rotation,
            "F.Cu",
        )?;
        let Some(library) = &self.library else {
            return Ok(item);
        };
        let mut footprint = copperpour_ipc::gen::kiapi::board::types::FootprintInstance::decode(
            item.value.as_slice(),
        )?;
        footprint.attributes = Some(library.attributes.clone());
        super::library_footprint::apply_footprint_margins(&mut footprint, &library.margins);
        let position = footprint
            .position
            .context("built footprint has no position")?;
        let definition = footprint
            .definition
            .as_mut()
            .context("built footprint has no definition")?;
        definition.attributes = Some(library.description_and_keywords.clone());
        definition.items.extend(library.models.iter().map(|model| {
            copperpour_ipc::builders::pack_any(model, "kiapi.board.types.Footprint3DModel")
        }));
        super::library_footprint::apply_pad_settings(
            definition,
            &library.pad_properties,
            &library.pad_zones,
            &library.pad_margins,
            carried.fields_10_0_7,
        )?;
        if carried.fields_10_0_7 {
            super::library_footprint::apply_reference_points(
                definition,
                &library.points,
                &position,
                self.front_rotation,
                None,
            )?;
        }
        if carried.net_ties {
            definition.net_ties = library.net_ties.clone();
        }
        if let Ok(properties) = &library.properties {
            apply_library_properties(&mut footprint, properties, reference, &self.value)?;
        }
        Ok(copperpour_ipc::builders::pack_any(
            &footprint,
            "kiapi.board.types.FootprintInstance",
        ))
    }

    /// Why [`Self::ipc_item`] gave the part default Reference and Value
    /// fields and none of the library's other properties, if it did.
    fn library_fields_not_carried(&self, lib_id: &str, reference: &str) -> Option<String> {
        let reason = self.library.as_ref()?.properties.as_ref().err()?;
        Some(format!(
            "{lib_id}'s library properties could not be read, so {reference} was placed with \
             default Reference and Value fields, not the library's, and without its Datasheet, \
             Description and custom properties: {reason}"
        ))
    }

    /// The block the closed-board path inserts: the prepared F.Cu block, or
    /// that block mirrored to B.Cu by the closed-board flip.
    fn file_block(&self) -> anyhow::Result<String> {
        let board_sexp = self
            .board_sexp
            .as_deref()
            .context("closed-board placement requires a board file block")?;
        if !self.on_back {
            return Ok(board_sexp.to_string());
        }
        flip_footprint_block(board_sexp, CopperStack::new(2).unwrap()).context(
            "the footprint cannot be mirrored to B.Cu in the closed board file; place it while \
             the board is open in KiCad 10.0.6 or newer, which flips it natively",
        )
    }

    /// Insert exactly this footprint through the same atomic, duplicate-safe
    /// path used by ordinary library placement.
    pub(crate) fn insert_into_board(&self, board_path: &Path) -> anyhow::Result<()> {
        insert_into_board(board_path, &[self.file_block()?])
    }
}

/// Give a built footprint the library's fields, as the sync gives the
/// footprints it adds (`pcb_sync::apply_library_fields`, #789): Reference and
/// Value with the library's layer, visibility, font and keep-upright (#157),
/// Datasheet and Description where the library has them, and every custom
/// property, such as `KiLib_Generator`. They are placed with the library
/// refresh's own transform. The Reference and Value text is the placement's.
fn apply_library_properties(
    footprint: &mut copperpour_ipc::gen::kiapi::board::types::FootprintInstance,
    properties: &super::library_footprint::LibraryProperties,
    reference: &str,
    value: &str,
) -> anyhow::Result<()> {
    super::pcb_sync::apply_library_fields(footprint, properties)?;
    let definition = footprint
        .definition
        .as_mut()
        .context("built footprint has no definition")?;
    if properties.reference.is_some() {
        set_shown_text(&mut footprint.reference_field, reference);
        set_shown_text(&mut definition.reference_field, reference);
    }
    if properties.value.is_some() {
        set_shown_text(&mut footprint.value_field, value);
        set_shown_text(&mut definition.value_field, value);
    }
    Ok(())
}

fn set_shown_text(field: &mut Option<copperpour_ipc::gen::kiapi::board::types::Field>, text: &str) {
    if let Some(shown) = field
        .as_mut()
        .and_then(|field| field.text.as_mut())
        .and_then(|text| text.text.as_mut())
    {
        shown.text = text.to_string();
    }
}

fn footprint_references(content: &str) -> anyhow::Result<HashSet<String>> {
    let root = copperpour_sexp::parse_sexp(content)?;
    let footprints: Vec<&copperpour_sexp::SexpNode> = if root.head() == Some("footprint") {
        vec![&root]
    } else if root.head() == Some("kicad_pcb") {
        root.find_all("footprint")
    } else {
        anyhow::bail!("expected a footprint or kicad_pcb root");
    };

    Ok(footprints
        .into_iter()
        .filter_map(footprint_reference)
        .collect())
}

fn board_contains_reference(board_path: &Path, reference: &str) -> anyhow::Result<bool> {
    let content = read_consistent(board_path)?;
    if let Err(reason) = check_single_board_form(&content) {
        anyhow::bail!(
            "Refusing to read {}: {}. The board file is not balanced and was left untouched.",
            board_path.display(),
            reason
        );
    }
    Ok(footprint_references(&content)?.contains(reference))
}

pub(super) fn footprint_reference(footprint: &copperpour_sexp::SexpNode) -> Option<String> {
    footprint
        .find_all("property")
        .into_iter()
        .find(|property| {
            property.get(1).and_then(copperpour_sexp::SexpNode::as_str) == Some("Reference")
        })
        .and_then(|property| property.get(2))
        .and_then(copperpour_sexp::SexpNode::as_str)
        .or_else(|| {
            footprint
                .find_all("fp_text")
                .into_iter()
                .find(|text| {
                    text.get(1).and_then(copperpour_sexp::SexpNode::as_str) == Some("reference")
                })
                .and_then(|text| text.get(2))
                .and_then(copperpour_sexp::SexpNode::as_str)
        })
        .map(str::to_string)
}

fn persist_board_replacement(
    board_path: &Path,
    expected: &str,
    replacement: &str,
) -> Result<(), copperpour_sexp::SexpError> {
    #[cfg(test)]
    crate::tools::write_hook::run(board_path);
    write_atomic_if_unchanged(board_path, expected, replacement)
}

#[derive(Clone, Copy)]
enum FootprintPlacementUpdate {
    Move { x: f64, y: f64 },
    Rotate { rotation: f64 },
    Set { x: f64, y: f64, rotation: f64 },
}

/// Why a closed-board placement update could not be applied.
///
/// The handlers have to tell a caller's mistake — a reference that is not on
/// this board — from a board Copperpour declines to edit, from a genuine I/O
/// failure, and report each differently. Deciding that by matching on the
/// error's message text is exactly what [`with_board_ipc_classified`]'s contract
/// forbids two screens up, and it left every case except the missing
/// reference surfacing as an unstructured `handler_error` (#194's class).
#[derive(Debug)]
pub(crate) enum ClosedBoardError {
    /// No footprint on this board carries that reference.
    ReferenceNotFound(String),
    /// More than one does, so "the" footprint is ambiguous.
    ReferenceAmbiguous(String),
    /// More than one does, and the tool takes a `kiid` to choose: the
    /// footprints' UUIDs, sorted, and the reason naming each one's position.
    AmbiguousTarget {
        reference: String,
        candidates: Vec<String>,
        reason: String,
    },
    /// The caller's `kiid` names no footprint, or one with another reference.
    StaleTarget { target: String, reason: String },
    /// The board is not a shape this tool will edit, before or after.
    Unusable(String),
    /// The board's copper stack gives the footprint's inner copper no
    /// mirror on the other side.
    FlipBlocked(String),
    /// Reading or writing failed, or the file changed under us.
    Io(anyhow::Error),
}

impl ClosedBoardError {
    /// The result to hand back: something the caller can act on, with the
    /// board untouched. Read, parse, writer and I/O errors return `Err` so dispatch
    /// classifies readback mismatches, revision conflicts, KiCad editor locks,
    /// and remaining I/O failures the same way as for other board writers.
    pub(crate) fn into_result(self, board_path: &Path) -> anyhow::Result<CallToolResult> {
        Ok(match self {
            Self::ReferenceNotFound(reference) => CallToolResult::error_kind(
                crate::mcp::error::ToolErrorKind::InvalidArgument {
                    field: "reference".to_string(),
                    reason: format!("no footprint '{reference}' on this board"),
                },
                format!(
                    "Footprint '{reference}' is not on this board, so there was nothing to \
                     update. The board file was not modified."
                ),
            ),
            Self::ReferenceAmbiguous(reference) => CallToolResult::error_kind(
                crate::mcp::error::ToolErrorKind::InvalidArgument {
                    field: "reference".to_string(),
                    reason: format!("'{reference}' appears more than once on this board"),
                },
                format!(
                    "Footprint reference '{reference}' appears more than once on the board, so \
                     it does not identify one footprint. The board file was not modified — \
                     give the duplicates distinct references first."
                ),
            ),
            Self::AmbiguousTarget {
                reference,
                candidates,
                reason,
            } => ambiguous_footprint_result(
                reference,
                candidates,
                format!("{reason}. The board file was not modified."),
            ),
            Self::StaleTarget { target, reason } => CallToolResult::error_kind(
                crate::mcp::error::ToolErrorKind::StaleTarget {
                    target,
                    reason: reason.clone(),
                },
                format!("{reason}. The board file was not modified."),
            ),
            Self::Unusable(reason) => CallToolResult::error_kind(
                crate::mcp::error::ToolErrorKind::MalformedFile {
                    path: board_path.display().to_string(),
                    reason: reason.clone(),
                },
                format!("Refusing to edit the board: {reason}. The board file was not modified."),
            ),
            Self::FlipBlocked(reason) => CallToolResult::error_kind(
                crate::mcp::error::ToolErrorKind::PlanBlocked {
                    operation: "flip_component".to_string(),
                    reasons: vec![reason.clone()],
                },
                format!("Refusing to flip: {reason}. The board file was not modified."),
            ),
            // Writer and I/O errors go back to dispatch so it classifies
            // readback mismatches, revision conflicts, KiCad editor locks, and
            // remaining I/O failures uniformly (#418, #419).
            Self::Io(error) => return Err(error),
        })
    }
}

fn update_closed_board_footprint(
    board_path: &Path,
    reference: &str,
    update: FootprintPlacementUpdate,
) -> Result<(), ClosedBoardError> {
    let content = read_consistent(board_path).map_err(|e| ClosedBoardError::Io(e.into()))?;
    let updated = prepare_closed_board_footprint_update(&content, reference, update)?;
    persist_board_replacement(board_path, &content, &updated)
        .map_err(|e| ClosedBoardError::Io(e.into()))?;
    Ok(())
}

pub(crate) fn update_closed_board_footprints(
    board_path: &Path,
    placements: &[copperpour_ipc::types::IpcFootprintPlacement],
) -> Result<Vec<copperpour_ipc::types::IpcFootprintPlacement>, ClosedBoardError> {
    let content =
        read_consistent(board_path).map_err(|error| ClosedBoardError::Io(error.into()))?;
    let (updated, applied) = place_closed_board_footprints(&content, placements)?;
    persist_board_replacement(board_path, &content, &updated)
        .map_err(|error| ClosedBoardError::Io(error.into()))?;
    Ok(applied)
}

/// The board text `content` with each footprint set to its placement, and
/// the placements as that text holds them. Nothing is read or written.
pub(super) fn place_closed_board_footprints(
    content: &str,
    placements: &[copperpour_ipc::types::IpcFootprintPlacement],
) -> Result<(String, Vec<copperpour_ipc::types::IpcFootprintPlacement>), ClosedBoardError> {
    let mut updated = content.to_string();
    for placement in placements {
        updated = prepare_closed_board_footprint_update(
            &updated,
            &placement.reference,
            FootprintPlacementUpdate::Set {
                x: placement.x,
                y: placement.y,
                rotation: placement.rotation,
            },
        )?;
    }
    // Report what was written, not what was asked: the file path normalizes
    // the root angle to KiCad's (-180, 180] (a requested 270 is stored as
    // -90), and the response has to say the number the file now holds.
    let mut applied = Vec::with_capacity(placements.len());
    for placement in placements {
        let root_at = footprint_root_at(&updated, &placement.reference).ok_or_else(|| {
            ClosedBoardError::Unusable(format!(
                "footprint '{}' was updated but cannot be read back from the updated board",
                placement.reference
            ))
        })?;
        applied.push(copperpour_ipc::types::IpcFootprintPlacement {
            reference: placement.reference.clone(),
            x: root_at.0,
            y: root_at.1,
            rotation: root_at.2,
        });
    }
    Ok((updated, applied))
}

/// The root `(at x y angle)` of the footprint `reference` in a board's text.
fn footprint_root_at(content: &str, reference: &str) -> Option<(f64, f64, f64)> {
    find_direct_child_blocks(content, "kicad_pcb")
        .into_iter()
        .filter_map(|(start, end)| copperpour_sexp::parse_sexp(&content[start..end]).ok())
        .filter(|node| node.head() == Some("footprint"))
        .find(|node| footprint_reference(node).as_deref() == Some(reference))
        .and_then(|node| {
            let at = node.find("at")?;
            Some((at.get_f64(1)?, at.get_f64(2)?, at.get_f64(3).unwrap_or(0.0)))
        })
}

/// A dry run on the closed-file path: the updates are prepared exactly as an
/// apply prepares them, so the same references and board shapes are refused,
/// and each footprint's current and target pose is read from the board text
/// before and after. Nothing is written, and the copper check, which needs a
/// live KiCad, is reported as not run.
fn closed_board_dry_run(
    board_path: &Path,
    updates: &[(String, FootprintPlacementUpdate)],
    reason: &NoLiveBoard,
) -> anyhow::Result<CallToolResult> {
    let planned = read_consistent(board_path)
        .map_err(|error| ClosedBoardError::Io(error.into()))
        .and_then(|content| {
            let mut updated = content.clone();
            for (reference, update) in updates {
                updated = prepare_closed_board_footprint_update(&updated, reference, *update)?;
            }
            updates
                .iter()
                .map(|(reference, _)| {
                    let pose = |text: &str| {
                        footprint_root_at(text, reference)
                            .map(|(x, y, rotation)| json!({ "x": x, "y": y, "rotation": rotation }))
                    };
                    match (pose(&content), pose(&updated)) {
                        (Some(current), Some(target)) => Ok(json!({
                            "reference": reference,
                            "current": current,
                            "target": target,
                        })),
                        _ => Err(ClosedBoardError::Unusable(format!(
                            "footprint '{reference}' has no readable root placement"
                        ))),
                    }
                })
                .collect::<Result<Vec<_>, _>>()
        });
    match planned {
        Ok(placements) => Ok(CallToolResult::json(&json!({
            "dry_run": true,
            "count": placements.len(),
            "placements": placements,
            "source": "file",
            "fallback_reason": reason.evidence(),
            "copper_check": pcb_copper_check::not_run_on_closed_board()
        }))),
        Err(error) => error.into_result(board_path),
    }
}

fn prepare_closed_board_footprint_update(
    content: &str,
    reference: &str,
    update: FootprintPlacementUpdate,
) -> Result<String, ClosedBoardError> {
    if let Err(reason) = check_single_board_form(content) {
        return Err(ClosedBoardError::Unusable(reason.to_string()));
    }

    let mut matched = None;
    for (start, end) in find_direct_child_blocks(content, "kicad_pcb") {
        let block = &content[start..end];
        let footprint = match copperpour_sexp::parse_sexp(block) {
            Ok(node) if node.head() == Some("footprint") => node,
            _ => continue,
        };
        if footprint_reference(&footprint).as_deref() == Some(reference)
            && matched.replace((start, end)).is_some()
        {
            return Err(ClosedBoardError::ReferenceAmbiguous(reference.to_string()));
        }
    }

    let (start, end) =
        matched.ok_or_else(|| ClosedBoardError::ReferenceNotFound(reference.to_string()))?;
    let replacement = update_footprint_placement(&content[start..end], update)
        .map_err(|error| ClosedBoardError::Unusable(format!("{error:#}")))?;
    let updated = apply_edits(
        content.to_string(),
        vec![SexpEdit::replace(start, end, replacement)],
    );
    // Defensive, and deliberately kept despite having no reachable trigger:
    // `update_footprint_placement` already refuses anything whose root is not
    // a `footprint`, and `apply_edits` swaps one balanced block for another,
    // so there is no input that reaches here with a broken board. Neutering it
    // changes no test, which is the honest state of affairs — it exists to
    // make "we never write a board we did not just re-validate" true by
    // construction rather than by tracing every caller.
    if let Err(reason) = check_single_board_form(&updated) {
        return Err(ClosedBoardError::Unusable(format!(
            "updating '{reference}' would have produced {reason}"
        )));
    }
    Ok(updated)
}

fn update_footprint_placement(
    footprint: &str,
    update: FootprintPlacementUpdate,
) -> anyhow::Result<String> {
    let at_ranges: Vec<_> = find_direct_child_blocks(footprint, "footprint")
        .into_iter()
        .filter(|(start, end)| {
            copperpour_sexp::parse_sexp(&footprint[*start..*end])
                .ok()
                .is_some_and(|node| node.head() == Some("at"))
        })
        .collect();
    let [(at_start, at_end)] = at_ranges.as_slice() else {
        anyhow::bail!("footprint must contain exactly one root placement (at ...) block");
    };
    let at = copperpour_sexp::parse_sexp(&footprint[*at_start..*at_end])?;
    let old_x = at
        .get_f64(1)
        .context("footprint root placement has an invalid X position")?;
    let old_y = at
        .get_f64(2)
        .context("footprint root placement has an invalid Y position")?;
    let old_rotation = at.get_f64(3).unwrap_or(0.0);

    // A move preserves the existing orientation *exactly as spelled* — it is
    // not this tool's business to renormalise an angle the caller did not ask
    // to change. A rotation writes KiCad's spelling of the new one.
    let (x, y, rotation) = match update {
        FootprintPlacementUpdate::Move { x, y } => (x, y, old_rotation),
        FootprintPlacementUpdate::Rotate { rotation } => {
            (old_x, old_y, normalize_root_angle(rotation))
        }
        FootprintPlacementUpdate::Set { x, y, rotation } => (x, y, normalize_root_angle(rotation)),
    };

    // Replace the root `(at …)` FIRST, while `at_start`/`at_end` still index
    // the string they were measured against.
    //
    // `apply_rotation_to_children` rewrites the `(at …)` inside every pad,
    // property and fp_text, and those replacements change length — an angle
    // can gain digits, or reach zero and be dropped entirely. Rotating first
    // and splicing after left the root offsets stale by that delta whenever a
    // child `(at …)` preceded the root one, which is legal S-expression even
    // though KiCad's own writer does not emit it. A −45° property angle
    // rotating to 0 shortens by four bytes and lands the splice inside the
    // preceding block: `(at (at 10 20 75) (pad "1" …`.
    //
    // The two passes are safe in this order because they touch disjoint
    // blocks: the root `(at …)` is a direct child of `footprint`, and
    // `apply_rotation_to_children` only rewrites the first `(at …)` *inside* a
    // pad/property/fp_text — and it recomputes its own offsets against the
    // string it is given.
    let updated = apply_edits(
        footprint.to_string(),
        vec![SexpEdit::replace(
            *at_start,
            *at_end,
            format_at(x, y, rotation),
        )],
    );
    let updated = match update {
        FootprintPlacementUpdate::Move { .. } => updated,
        FootprintPlacementUpdate::Rotate { rotation }
        | FootprintPlacementUpdate::Set { rotation, .. } => {
            apply_rotation_to_children(&updated, rotation - old_rotation)
        }
    };
    let parsed = copperpour_sexp::parse_sexp(&updated)?;
    if parsed.head() != Some("footprint") {
        anyhow::bail!("placement update changed the footprint root");
    }
    Ok(updated)
}

fn direct_children_with_tags(
    source: &str,
    parent_tag: &str,
) -> anyhow::Result<Vec<(usize, usize, String)>> {
    find_direct_child_blocks(source, parent_tag)
        .into_iter()
        .map(|(start, end)| {
            let node = copperpour_sexp::parse_sexp(&source[start..end])?;
            let tag = node
                .head()
                .context("direct child has no S-expression tag")?
                .to_string();
            Ok((start, end, tag))
        })
        .collect()
}

/// Whether `source` has at least one direct `(child_tag …)`, without caring
/// how many or failing on a block that has none.
fn has_direct_child(source: &str, parent_tag: &str, child_tag: &str) -> bool {
    direct_children_with_tags(source, parent_tag)
        .map(|children| children.iter().any(|(_, _, tag)| tag == child_tag))
        .unwrap_or(false)
}

fn exactly_one_direct_child(
    source: &str,
    parent_tag: &str,
    child_tag: &str,
) -> anyhow::Result<(usize, usize)> {
    let matches: Vec<_> = direct_children_with_tags(source, parent_tag)?
        .into_iter()
        .filter_map(|(start, end, tag)| (tag == child_tag).then_some((start, end)))
        .collect();
    let [(start, end)] = matches.as_slice() else {
        anyhow::bail!("{parent_tag} must contain exactly one direct ({child_tag} ...) block");
    };
    Ok((*start, *end))
}

fn at_components(block: &str) -> anyhow::Result<(f64, f64, f64, Vec<String>)> {
    let at = copperpour_sexp::parse_sexp(block)?;
    if at.head() != Some("at") {
        anyhow::bail!("expected an (at ...) block");
    }
    let x = at
        .get_f64(1)
        .context("(at ...) has an invalid X position")?;
    let y = at
        .get_f64(2)
        .context("(at ...) has an invalid Y position")?;
    let children = at.children().unwrap_or_default();
    let angle = at.get_f64(3).unwrap_or(0.0);
    let suffix_start = if at.get_f64(3).is_some() { 4 } else { 3 };
    let suffix = children
        .iter()
        .skip(suffix_start)
        .map(|child| {
            child
                .as_str()
                .map(str::to_string)
                .context("(at ...) contains a non-atomic suffix")
        })
        .collect::<anyhow::Result<Vec<_>>>()?;
    Ok((x, y, angle, suffix))
}

fn format_at_with_suffix(x: f64, y: f64, angle: f64, suffix: &[String]) -> String {
    let mut formatted = format_at(x, y, angle);
    if !suffix.is_empty() {
        formatted.insert_str(formatted.len() - 1, &format!(" {}", suffix.join(" ")));
    }
    formatted
}

fn format_xy(tag: &str, x: f64, y: f64) -> String {
    let at = format_at(x, y, 0.0);
    format!("({tag}{})", &at["(at".len()..at.len() - 1])
}

fn normalize_angle(angle: f64) -> f64 {
    angle.rem_euclid(360.0)
}

fn normalize_angle_180(angle: f64) -> f64 {
    let normalized = normalize_angle(angle);
    if normalized > 180.0 {
        normalized - 360.0
    } else {
        normalized
    }
}

fn flip_layer_block(block: &str, stack: CopperStack) -> anyhow::Result<String> {
    let layer = copperpour_sexp::parse_sexp(block)?;
    let name = layer
        .get(1)
        .and_then(copperpour_sexp::SexpNode::as_str)
        .context("(layer ...) has no layer name")?;
    Ok(format!(
        "(layer {})",
        quote_sexp_string(&flip_layer(name, stack)?)
    ))
}

fn flip_layers_block(block: &str, stack: CopperStack) -> anyhow::Result<String> {
    let layers = copperpour_sexp::parse_sexp(block)?;
    let names = layers
        .children()
        .unwrap_or_default()
        .iter()
        .skip(1)
        .map(|child| {
            let name = child
                .as_str()
                .context("(layers ...) contains a non-atomic layer name")?;
            Ok(quote_sexp_string(&flip_layer(name, stack)?))
        })
        .collect::<anyhow::Result<Vec<_>>>()?;
    Ok(format!("(layers {})", names.join(" ")))
}

fn toggle_text_mirror(effects: &str) -> anyhow::Result<String> {
    let justify = direct_children_with_tags(effects, "effects")?
        .into_iter()
        .filter_map(|(start, end, tag)| (tag == "justify").then_some((start, end)))
        .collect::<Vec<_>>();
    match justify.as_slice() {
        [] => Ok(apply_edits(
            effects.to_string(),
            vec![SexpEdit::insert(effects.len() - 1, " (justify mirror)")],
        )),
        [(start, end)] => {
            let node = copperpour_sexp::parse_sexp(&effects[*start..*end])?;
            let mut values = node
                .children()
                .unwrap_or_default()
                .iter()
                .skip(1)
                .map(|child| {
                    child
                        .as_str()
                        .map(str::to_string)
                        .context("(justify ...) contains a non-atomic value")
                })
                .collect::<anyhow::Result<Vec<_>>>()?;
            if let Some(index) = values.iter().position(|value| value == "mirror") {
                values.remove(index);
            } else {
                values.push("mirror".to_string());
            }
            let replacement = if values.is_empty() {
                String::new()
            } else {
                format!("(justify {})", values.join(" "))
            };
            Ok(apply_edits(
                effects.to_string(),
                vec![SexpEdit::replace(*start, *end, replacement)],
            ))
        }
        _ => anyhow::bail!("text effects contain duplicate (justify ...) blocks"),
    }
}

fn flip_text_block(block: &str, tag: &str, stack: CopperStack) -> anyhow::Result<String> {
    let (at_start, at_end) = exactly_one_direct_child(block, tag, "at")?;
    let (x, y, angle, suffix) = at_components(&block[at_start..at_end])?;
    let (layer_start, layer_end) = exactly_one_direct_child(block, tag, "layer")?;
    let layer_block = &block[layer_start..layer_end];
    let layer = copperpour_sexp::parse_sexp(layer_block)?;
    let layer_name = layer
        .get(1)
        .and_then(copperpour_sexp::SexpNode::as_str)
        .context("(layer ...) has no layer name")?;
    let flipped_layer_name = flip_layer(layer_name, stack)?;
    let (effects_start, effects_end) = exactly_one_direct_child(block, tag, "effects")?;
    let effects = if flipped_layer_name == layer_name {
        block[effects_start..effects_end].to_string()
    } else {
        toggle_text_mirror(&block[effects_start..effects_end])?
    };
    Ok(apply_edits(
        block.to_string(),
        vec![
            SexpEdit::replace(
                at_start,
                at_end,
                format_at_with_suffix(x, -y, normalize_angle(180.0 - angle), &suffix),
            ),
            SexpEdit::replace(
                layer_start,
                layer_end,
                format!("(layer {})", quote_sexp_string(&flipped_layer_name)),
            ),
            SexpEdit::replace(effects_start, effects_end, effects),
        ],
    ))
}

fn flip_graphic_block(block: &str, tag: &str, stack: CopperStack) -> anyhow::Result<String> {
    let point_tags: &[&str] = match tag {
        "fp_line" | "fp_rect" => &["start", "end"],
        "fp_circle" => &["center", "end"],
        "fp_arc" => &["start", "mid", "end"],
        _ => anyhow::bail!("unsupported footprint graphic '{tag}'"),
    };
    let mut edits = Vec::new();
    let mut points = Vec::new();
    for point_tag in point_tags {
        let (start, end) = exactly_one_direct_child(block, tag, point_tag)?;
        let point = copperpour_sexp::parse_sexp(&block[start..end])?;
        points.push((
            start,
            end,
            point
                .get_f64(1)
                .with_context(|| format!("({point_tag} ...) has an invalid X position"))?,
            -point
                .get_f64(2)
                .with_context(|| format!("({point_tag} ...) has an invalid Y position"))?,
        ));
    }
    let source_order: Vec<usize> = if tag == "fp_arc" {
        vec![2, 1, 0]
    } else {
        (0..points.len()).collect()
    };
    for (target_index, point_tag) in point_tags.iter().enumerate() {
        let (target_start, target_end, _, _) = points[target_index];
        let (_, _, x, y) = points[source_order[target_index]];
        edits.push(SexpEdit::replace(
            target_start,
            target_end,
            format_xy(point_tag, x, y),
        ));
    }
    let (layer_start, layer_end) = exactly_one_direct_child(block, tag, "layer")?;
    edits.push(SexpEdit::replace(
        layer_start,
        layer_end,
        flip_layer_block(&block[layer_start..layer_end], stack)?,
    ));
    Ok(apply_edits(block.to_string(), edits))
}

fn flip_poly_block(block: &str, stack: CopperStack) -> anyhow::Result<String> {
    let (pts_start, pts_end) = exactly_one_direct_child(block, "fp_poly", "pts")?;
    let pts = &block[pts_start..pts_end];
    let mut point_edits = Vec::new();
    for (start, end, tag) in direct_children_with_tags(pts, "pts")? {
        if tag != "xy" {
            anyhow::bail!("fp_poly contains unsupported point block '{tag}'");
        }
        let point = copperpour_sexp::parse_sexp(&pts[start..end])?;
        let x = point.get_f64(1).context("(xy ...) has an invalid X")?;
        let y = point.get_f64(2).context("(xy ...) has an invalid Y")?;
        point_edits.push(SexpEdit::replace(start, end, format_xy("xy", x, -y)));
    }
    let mirrored_pts = apply_edits(pts.to_string(), point_edits);
    let (layer_start, layer_end) = exactly_one_direct_child(block, "fp_poly", "layer")?;
    Ok(apply_edits(
        block.to_string(),
        vec![
            SexpEdit::replace(pts_start, pts_end, mirrored_pts),
            SexpEdit::replace(
                layer_start,
                layer_end,
                flip_layer_block(&block[layer_start..layer_end], stack)?,
            ),
        ],
    ))
}

fn contains_descendant_tag(node: &copperpour_sexp::SexpNode, tag: &str) -> bool {
    node.children().is_some_and(|children| {
        children
            .iter()
            .any(|child| child.head() == Some(tag) || contains_descendant_tag(child, tag))
    })
}

fn flip_pad_block(block: &str, stack: CopperStack) -> anyhow::Result<String> {
    let pad = copperpour_sexp::parse_sexp(block)?;
    if pad.get(3).and_then(copperpour_sexp::SexpNode::as_str) == Some("custom") {
        anyhow::bail!("custom pads are not supported by closed-board footprint flipping");
    }
    // KiCad mirrors a per-layer padstack's copper entries; this does not.
    if contains_descendant_tag(&pad, "padstack") {
        anyhow::bail!("per-layer padstacks are not supported by closed-board footprint flipping");
    }
    for unsupported in ["offset", "rect_delta", "chamfer_ratio", "primitives"] {
        if contains_descendant_tag(&pad, unsupported) {
            anyhow::bail!(
                "pad geometry containing ({unsupported} ...) is not supported by closed-board footprint flipping"
            );
        }
    }
    let (at_start, at_end) = exactly_one_direct_child(block, "pad", "at")?;
    let (x, y, angle, suffix) = at_components(&block[at_start..at_end])?;
    let (layers_start, layers_end) = exactly_one_direct_child(block, "pad", "layers")?;
    Ok(apply_edits(
        block.to_string(),
        vec![
            SexpEdit::replace(
                at_start,
                at_end,
                format_at_with_suffix(x, -y, normalize_angle(-angle), &suffix),
            ),
            SexpEdit::replace(
                layers_start,
                layers_end,
                flip_layers_block(&block[layers_start..layers_end], stack)?,
            ),
        ],
    ))
}

/// Refuse a `(model …)` whose placement a flip would have to move.
///
/// This guard applies only to the closed-board file-fallback path. An open
/// board flips through KiCad's own native `FlipItems` IPC command (10.0.6+),
/// which transforms a model's Y offset and X/Y rotation correctly, so that
/// path never reaches this function. The closed-board path directly edits
/// the `.kicad_pcb` file and leaves `(model …)` untouched, which is silently
/// wrong for any model where those are non-zero. Rather than guess the
/// transform — reproducing KiCad's own `FOOTPRINT::Flip` 3D-model math is out
/// of scope for the file fallback (issue #604) — this refuses, consistent
/// with how the rest of this path treats geometry it cannot mirror.
///
/// The cost of refusing is close to nothing. Across all **14,818** footprints
/// in KiCad 10's standard libraries that carry a `(model …)`: `offset.y` is
/// non-zero in **3**, and `rotate.x`/`rotate.y` in **none**. The three are
/// `RaspberryPi_Pico_Common_THT` (-24.13 mm, badly wrong if ignored) and two
/// sub-0.04 mm cases. The 84 footprints with a non-zero `rotate.z` are
/// unaffected either way, since a flip does not touch Z.
fn refuse_model_a_flip_would_move(block: &str) -> anyhow::Result<()> {
    let model = copperpour_sexp::parse_sexp(block)?;
    for (tag, fields) in [("offset", ["x", "y", "z"]), ("rotate", ["x", "y", "z"])] {
        let Some(node) = model.find_all(tag).into_iter().next() else {
            continue;
        };
        let Some(xyz) = node.find_all("xyz").into_iter().next() else {
            continue;
        };
        // A flip negates offset.y, rotate.x and rotate.y; Z and offset.x ride
        // along unchanged, so a non-zero value there is not a problem.
        let moved: &[usize] = if tag == "offset" { &[2] } else { &[1, 2] };
        for &index in moved {
            let value = xyz.get_f64(index).unwrap_or(0.0);
            if value != 0.0 {
                anyhow::bail!(
                    "the 3D model's {tag}.{} is {value}, and a flip would have to move it; \
                     flipping this footprint would leave the model where it was",
                    fields[index - 1]
                );
            }
        }
    }
    Ok(())
}

fn flip_footprint_block(footprint: &str, stack: CopperStack) -> anyhow::Result<String> {
    let root = copperpour_sexp::parse_sexp(footprint)?;
    if root.head() != Some("footprint") {
        anyhow::bail!("expected a footprint root");
    }
    let (root_at_start, root_at_end) = exactly_one_direct_child(footprint, "footprint", "at")?;
    let (x, y, angle, suffix) = at_components(&footprint[root_at_start..root_at_end])?;
    let (root_layer_start, root_layer_end) =
        exactly_one_direct_child(footprint, "footprint", "layer")?;
    let mut edits = vec![
        SexpEdit::replace(
            root_at_start,
            root_at_end,
            format_at_with_suffix(x, y, normalize_angle_180(-angle), &suffix),
        ),
        SexpEdit::replace(
            root_layer_start,
            root_layer_end,
            flip_layer_block(&footprint[root_layer_start..root_layer_end], stack)?,
        ),
    ];

    for (start, end, tag) in direct_children_with_tags(footprint, "footprint")? {
        let block = &footprint[start..end];
        let replacement = match tag.as_str() {
            // A property with no `(at …)` carries no geometry, so there is
            // nothing to mirror and it passes through untouched.
            //
            // This is not an edge case. KiCad writes
            // `(property ki_fp_filters "R_* Resistor_*")` — a bare token, no
            // position, no layer — into every footprint it places from a
            // library: 779 of them across the 19 boards shipped in
            // `share/kicad/demos`. Requiring exactly one `(at …)` on every
            // property therefore refused practically every real board with
            // "property must contain exactly one direct (at ...) block",
            // which is what the first live run of this tool hit. The
            // synthetic fixture has only positioned properties, so nothing
            // offline could have caught it.
            "property" if !has_direct_child(block, "property", "at") => None,
            "property" | "fp_text" => Some(flip_text_block(block, &tag, stack)?),
            "fp_line" | "fp_rect" | "fp_circle" | "fp_arc" => {
                Some(flip_graphic_block(block, &tag, stack)?)
            }
            "fp_poly" => Some(flip_poly_block(block, stack)?),
            "pad" => Some(flip_pad_block(block, stack)?),
            "model" => {
                refuse_model_a_flip_would_move(block)?;
                None
            }
            unsupported if unsupported.starts_with("fp_") || unsupported == "zone" => {
                anyhow::bail!(
                    "unsupported footprint child '{unsupported}' prevents a safe closed-board flip"
                )
            }
            _ => None,
        };
        if let Some(replacement) = replacement {
            edits.push(SexpEdit::replace(start, end, replacement));
        }
    }

    let flipped = apply_edits(footprint.to_string(), edits);
    let parsed = copperpour_sexp::parse_sexp(&flipped)?;
    if parsed.head() != Some("footprint") {
        anyhow::bail!("flip changed the footprint root");
    }
    Ok(flipped)
}

fn footprint_layer(footprint: &str) -> anyhow::Result<String> {
    let (start, end) = exactly_one_direct_child(footprint, "footprint", "layer")?;
    let layer = copperpour_sexp::parse_sexp(&footprint[start..end])?;
    layer
        .get(1)
        .and_then(copperpour_sexp::SexpNode::as_str)
        .map(str::to_string)
        .context("footprint layer has no name")
}

/// A footprint the closed-board flip may mean: its span, UUID, reference and
/// position in millimetres.
struct ClosedBoardTarget {
    span: (usize, usize),
    uuid: String,
    reference: Option<String>,
    at: (f64, f64),
}

/// The span of the footprint to flip, chosen as the live flip chooses it
/// (`select_footprint_target` in copperpour-ipc): the one with UUID `kiid`
/// when given, so long as it still carries `reference`, otherwise the one
/// footprint carrying `reference`. `targets` are the footprints carrying
/// `reference`, or with `kiid` the ones with that UUID.
fn select_closed_board_target(
    mut targets: Vec<ClosedBoardTarget>,
    reference: &str,
    kiid: Option<&str>,
) -> Result<(usize, usize), ClosedBoardError> {
    if let Some(kiid) = kiid {
        return match targets.len() {
            0 => Err(ClosedBoardError::StaleTarget {
                target: kiid.to_string(),
                reason: format!(
                    "No placed footprint on the board has KIID '{kiid}' (expected reference \
                     '{reference}')"
                ),
            }),
            1 => match targets[0].reference.as_deref() {
                Some(observed) if observed == reference => Ok(targets[0].span),
                observed => Err(ClosedBoardError::StaleTarget {
                    target: kiid.to_string(),
                    reason: format!(
                        "Footprint KIID '{kiid}' currently has reference '{}', not \
                         '{reference}'",
                        observed.unwrap_or_default()
                    ),
                }),
            },
            n => Err(ClosedBoardError::Unusable(format!(
                "the board lists {n} footprints with UUID '{kiid}'"
            ))),
        };
    }
    match targets.len() {
        0 => Err(ClosedBoardError::ReferenceNotFound(reference.to_string())),
        1 => Ok(targets[0].span),
        count => {
            // Distinct UUIDs let the caller pick one with `kiid`; a UUID
            // missing or listed twice leaves nothing to pick by.
            targets.sort_by(|a, b| a.uuid.cmp(&b.uuid));
            let distinct = targets.windows(2).all(|pair| pair[0].uuid != pair[1].uuid);
            if !distinct || targets.iter().any(|target| target.uuid.is_empty()) {
                return Err(ClosedBoardError::ReferenceAmbiguous(reference.to_string()));
            }
            let listed: Vec<_> = targets
                .iter()
                .map(|target| {
                    format!(
                        "{} at ({:.3}, {:.3}) mm",
                        target.uuid, target.at.0, target.at.1
                    )
                })
                .collect();
            Err(ClosedBoardError::AmbiguousTarget {
                reference: reference.to_string(),
                candidates: targets.into_iter().map(|target| target.uuid).collect(),
                reason: format!(
                    "Footprint reference '{reference}' matches {count} placed footprints; pass \
                     kiid to choose one: {}",
                    listed.join(", ")
                ),
            })
        }
    }
}

fn prepare_closed_board_footprint_side(
    content: &str,
    reference: &str,
    kiid: Option<&str>,
    target_layer: &str,
) -> Result<(String, bool), ClosedBoardError> {
    if let Err(reason) = check_single_board_form(content) {
        return Err(ClosedBoardError::Unusable(reason.to_string()));
    }
    let mut targets = Vec::new();
    let mut layer_table = None;
    for (start, end, tag) in
        direct_children_with_tags(content, "kicad_pcb").map_err(ClosedBoardError::Io)?
    {
        if tag == "layers" && layer_table.replace(&content[start..end]).is_some() {
            return Err(ClosedBoardError::Unusable(
                "the board has more than one (layers ...) table".to_string(),
            ));
        }
        if tag != "footprint" {
            continue;
        }
        let footprint = copperpour_sexp::parse_sexp(&content[start..end])
            .map_err(|e| ClosedBoardError::Io(e.into()))?;
        // KiCad reads a footprint's `tstamp`, in older files, as its KIID too.
        let uuid = footprint
            .find_str("uuid")
            .or_else(|| footprint.find_str("tstamp"))
            .unwrap_or_default()
            .to_string();
        let footprint_ref = footprint_reference(&footprint);
        let named = match kiid {
            Some(kiid) => uuid == kiid,
            None => footprint_ref.as_deref() == Some(reference),
        };
        if named {
            let at = footprint.find("at");
            targets.push(ClosedBoardTarget {
                span: (start, end),
                uuid,
                reference: footprint_ref,
                at: (
                    at.and_then(|at| at.get_f64(1)).unwrap_or_default(),
                    at.and_then(|at| at.get_f64(2)).unwrap_or_default(),
                ),
            });
        }
    }
    let (start, end) = select_closed_board_target(targets, reference, kiid)?;
    let block = &content[start..end];
    let current_layer = footprint_layer(block).map_err(ClosedBoardError::Io)?;
    if !matches!(current_layer.as_str(), "F.Cu" | "B.Cu") {
        return Err(ClosedBoardError::Unusable(format!(
            "footprint '{reference}' sits on root layer '{current_layer}', which is neither \
             side of the board"
        )));
    }
    if current_layer == target_layer {
        return Ok((content.to_string(), false));
    }
    // With no table, KiCad's default board has two copper layers.
    let stack = match layer_table {
        Some(table) => {
            let table =
                copperpour_sexp::parse_sexp(table).map_err(|e| ClosedBoardError::Io(e.into()))?;
            let layers = copperpour_sexp::layers::table_layers(&table);
            CopperStack::from_layer_names(layers.iter().map(|layer| layer.name.as_str()))
        }
        None => CopperStack::new(2),
    }
    .map_err(|error| ClosedBoardError::FlipBlocked(error.to_string()))?;
    let flipped = flip_footprint_block(block, stack).map_err(|error| {
        match error.downcast_ref::<FlipLayerError>() {
            Some(FlipLayerError::InnerLayerNotOnBoard { .. }) => {
                ClosedBoardError::FlipBlocked(format!("footprint '{reference}': {error:#}"))
            }
            _ => ClosedBoardError::Unusable(format!("{error:#}")),
        }
    })?;
    if footprint_layer(&flipped).map_err(ClosedBoardError::Io)? != target_layer {
        return Err(ClosedBoardError::Unusable(format!(
            "flipping '{reference}' did not produce target layer '{target_layer}'"
        )));
    }
    let updated = apply_edits(
        content.to_string(),
        vec![SexpEdit::replace(start, end, flipped)],
    );
    if let Err(reason) = check_single_board_form(&updated) {
        return Err(ClosedBoardError::Unusable(format!(
            "flipping '{reference}' would have produced {reason}"
        )));
    }
    Ok((updated, true))
}

fn set_closed_board_footprint_side(
    board_path: &Path,
    reference: &str,
    kiid: Option<&str>,
    target_layer: &str,
) -> Result<bool, ClosedBoardError> {
    let content = read_consistent(board_path).map_err(|e| ClosedBoardError::Io(e.into()))?;
    let (updated, changed) =
        prepare_closed_board_footprint_side(&content, reference, kiid, target_layer)?;
    if changed {
        persist_board_replacement(board_path, &content, &updated)
            .map_err(|e| ClosedBoardError::Io(e.into()))?;
    }
    Ok(changed)
}

/// Verify `content` is exactly one `(kicad_pcb …)` form and nothing else.
///
/// Checking only that *a* balanced block exists is too weak to back the promise
/// above: `find_balanced_block` skips whatever precedes the first paren, so
/// leading garbage would pass, as would a well-formed form that is not a board
/// at all.
pub(crate) fn check_single_board_form(content: &str) -> Result<(), String> {
    let trimmed = content.trim();
    let (start, end) = find_balanced_block(trimmed, 0)
        .ok_or_else(|| "the result is not a balanced S-expression".to_string())?;

    if start != 0 {
        return Err(format!(
            "{} bytes of content precede the opening paren",
            start
        ));
    }
    if end != trimmed.len() {
        return Err(format!(
            "{} bytes of content follow the closing paren",
            trimmed.len() - end
        ));
    }
    if !trimmed[1..].trim_start().starts_with("kicad_pcb") {
        return Err("the root expression is not (kicad_pcb …)".to_string());
    }
    Ok(())
}

/// Prefix every non-empty line with `indent`, joining them with `eol`.
fn indent_block(block: &str, indent: &str, eol: &str) -> String {
    // `lines()` strips a trailing \r along with the \n, so rejoining with `eol`
    // re-imposes one convention on a block that may have arrived with another —
    // a CRLF library footprint going into an LF board, or the reverse.
    block
        .lines()
        .map(|l| {
            if l.trim().is_empty() {
                String::new()
            } else {
                format!("{indent}{l}")
            }
        })
        .collect::<Vec<_>>()
        .join(eol)
}

// ─── Tool definitions ─────────────────────────────────────────────────────────

pub fn tools() -> Vec<ToolDef> {
    let mut tools = vec![
        tool!(
            "place_component",
            "Place a footprint on the PCB. Uses live KiCAD IPC when reachable; otherwise safely \
             edits a closed board file with revision checks.",
            json!({
                "type": "object",
                "properties": {
                    "board":      { "type": "string" },
                    "footprint":  { "type": "string", "description": "Library:Footprint (e.g. 'Resistor_SMD:R_0402')" },
                    "reference":  { "type": "string", "description": "Reference designator" },
                    "x":          { "type": "number" },
                    "y":          { "type": "number" },
                    "rotation":   { "type": "number", "default": 0 },
                    "layer":      { "type": "string", "default": "F.Cu" }
                },
                "required": ["board", "footprint", "reference", "x", "y"]
            }),
            |args, ctx| async move { handle_place_component(args, ctx).await }
        )
        .with_board_access(crate::tools::BoardAccess::LivePreferredWithFallback),
        tool!(
            "move_component",
            "Move a placed footprint to a new X/Y position. Uses live KiCAD IPC when reachable; \
             otherwise safely edits a closed board file with revision checks. On a live board it \
             first checks that no moved pad would overlap a via, track or arc of another net \
             (overlap only, not clearance; zones and other footprints' pads are not checked) and \
             refuses as plan_blocked, moving nothing, if one would. allow_copper_conflicts applies \
             anyway; dry_run only reports. The closed-file path cannot run the check and reports \
             copper_check.status not_run.",
            json!({
                "type": "object",
                "properties": {
                    "board":     { "type": "string" },
                    "reference": { "type": "string" },
                    "x":         { "type": "number" },
                    "y":         { "type": "number" },
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
                "required": ["board", "reference", "x", "y"]
            }),
            |args, ctx| async move { handle_move_component(args, ctx).await }
        )
        .with_board_access(crate::tools::BoardAccess::LivePreferredWithFallback),
        tool!(
            "rotate_component",
            "Set the rotation angle of a placed footprint. Uses live KiCAD IPC when reachable; \
             otherwise safely edits a closed board file with revision checks. On a live board it \
             first checks that no rotated pad would overlap a via, track or arc of another net \
             (overlap only, not clearance; zones and other footprints' pads are not checked) and \
             refuses as plan_blocked, moving nothing, if one would. allow_copper_conflicts applies \
             anyway; dry_run only reports. The closed-file path cannot run the check and reports \
             copper_check.status not_run.",
            json!({
                "type": "object",
                "properties": {
                    "board":     { "type": "string" },
                    "reference": { "type": "string" },
                    "rotation":  { "type": "number", "description": "Rotation angle in degrees" },
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
                "required": ["board", "reference", "rotation"]
            }),
            |args, ctx| async move { handle_rotate_component(args, ctx).await }
        )
        .with_board_access(crate::tools::BoardAccess::LivePreferredWithFallback),
        tool!(
            "set_component_placements",
            "Set X/Y positions and rotations for multiple existing footprints atomically. Uses one live KiCAD IPC update and one undo step when reachable; otherwise safely edits a closed board file once with revision checks. On a live board it first checks that no moved pad would overlap a via, track or arc of another net (overlap only, not clearance; zones and other footprints' pads, including others in the batch, are not checked) and refuses the whole batch as plan_blocked, moving nothing, if one would. allow_copper_conflicts applies anyway; dry_run only reports. The closed-file path cannot run the check and reports copper_check.status not_run.",
            json!({
                "type": "object",
                "properties": {
                    "board": { "type": "string" },
                    "placements": {
                        "type": "array",
                        "minItems": 1,
                        "items": {
                            "type": "object",
                            "properties": {
                                "reference": { "type": "string" },
                                "x": { "type": "number", "description": "Target X coordinate in millimetres" },
                                "y": { "type": "number", "description": "Target Y coordinate in millimetres" },
                                "rotation": { "type": "number", "description": "Target absolute rotation in degrees" }
                            },
                            "required": ["reference", "x", "y", "rotation"]
                        }
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
                "required": ["board", "placements"]
            }),
            |args, ctx| async move { handle_set_component_placements(args, ctx).await }
        )
        .with_board_access(crate::tools::BoardAccess::LivePreferredWithFallback),
        tool!(
            "flip_component",
            "Set a placed footprint to F.Cu or B.Cu. When the board is open in KiCad 10.0.6 or \
             newer, this uses KiCad's own native FlipItems IPC command — the same transform the \
             GUI's F key performs, including 3D-model offset/rotation — inside one KiCad undo \
             step. Older or unsupported KiCad reachable over IPC returns an explicit \
             unsupported_capability error rather than falling back to the file. Only when no \
             live KiCad holds this board does it fall back to a guarded closed-board file edit, \
             which flips supported geometry with revision checks and refuses any footprint whose \
             3D model would need a transform the file path cannot perform. Optional kiid pins \
             one footprint: without it, a reference several footprints share (such as 'REF**') \
             is refused as ambiguous_target naming each KIID.",
            json!({
                "type": "object",
                "properties": {
                    "board":     { "type": "string" },
                    "reference": { "type": "string" },
                    "layer":     { "type": "string", "enum": ["F.Cu", "B.Cu"] },
                    "kiid":      { "type": "string", "description": "Exact footprint KIID (UUID) to pin one instance or disambiguate duplicate references (optional)" }
                },
                "required": ["board", "reference", "layer"]
            }),
            |args, ctx| async move { handle_flip_component(args, ctx).await }
        )
        .with_board_access(crate::tools::BoardAccess::LivePreferredWithFallback),
        tool!(
            "delete_component",
            "Remove a footprint from the board via KiCAD IPC.",
            json!({
                "type": "object",
                "properties": {
                    "board":     { "type": "string" },
                    "reference": { "type": "string" }
                },
                "required": ["board", "reference"]
            }),
            |args, ctx| async move { handle_delete_component(args, ctx).await }
        )
        .with_board_access(crate::tools::BoardAccess::LiveOnly),
        tool!(
            "edit_component",
            "Change a placed footprint's reference (new_reference, board-owned footprints only), \
             value, lock (locked) and assembly flags: dnp (do not populate), exclude_from_bom and \
             exclude_from_pos_files. Direct board-side rename is permitted only for footprints not \
             linked to a schematic symbol; schematic-owned references must be renamed with \
             edit_schematic_component and propagated with update_pcb_from_schematic. Renaming to a \
             reference already used by another footprint on the board is refused. Optional kiid \
             pins one exact footprint instance (and disambiguates duplicate references such as \
             'REF**': without it, a shared reference is refused as ambiguous_target naming each \
             KIID). A renamed footprint with no symbol link that takes a reference the \
             schematic uses is adopted by the next update_pcb_from_schematic, which matches it by \
             reference, as KiCad's Update PCB does. Every property not named is kept. One KiCad undo step; the result is a \
             fresh readback by KIID of what the board holds, and a readback that does not match \
             the request is reported as an error rather than success. This edit sends the whole \
             footprint over IPC and can drop symbol-unit mappings on KiCad 10.0–10.0.7. Rule areas \
             inside footprints can also change. Readback verifies neither. See 'Footprint unit \
             metadata disappears after a live PCB edit' in Troubleshooting. \
             update_pcb_from_schematic sets dnp and exclude_from_bom from a linked footprint's \
             symbol again. With no property named, reports the footprint without changing it. Does \
             not save. Requires KiCad running with the board open.",
            json!({
                "type": "object",
                "properties": {
                    "board":         { "type": "string" },
                    "reference":     { "type": "string" },
                    "kiid":          { "type": "string", "description": "Exact footprint KIID (UUID) to pin one instance or disambiguate duplicate references (optional)" },
                    "new_reference": { "type": "string", "description": "New reference designator for a board-owned footprint not linked to a schematic symbol (optional)" },
                    "value":         { "type": "string", "description": "New value string (optional)" },
                    "dnp":           { "type": "boolean", "description": "Do not populate (optional)" },
                    "exclude_from_bom":       { "type": "boolean", "description": "Exclude from the bill of materials (optional)" },
                    "exclude_from_pos_files": { "type": "boolean", "description": "Exclude from position (pick-and-place) files (optional)" },
                    "locked":        { "type": "boolean", "description": "Lock (true) or unlock (false) the footprint (optional)" }
                },
                "required": ["board", "reference"]
            }),
            |args, ctx| async move { handle_edit_component(args, ctx).await }
        )
        .with_board_access(crate::tools::BoardAccess::LiveOnly),
        tool!(
            "repair_corrupted_footprints",
            "Repair the exact legacy corruption from Konnect issue #244: footprint drawing \
             shapes rewritten as anonymous pads with empty layer sets. Resolves each affected \
             footprint from its library, restores its pads and drawing shapes while preserving \
             placement, identity and pad nets, and applies every requested repair in one KiCad \
             undo commit. Defaults to a non-mutating dry run; apply requires the exact returned \
             plan revision. Requires KiCAD running with the requested board open.",
            json!({
                "type": "object",
                "properties": {
                    "board": { "type": "string" },
                    "references": {
                        "type": "array",
                        "description": "Optional reference-designator allowlist. Omit to scan every footprint.",
                        "items": { "type": "string" }
                    },
                    "dry_run": {
                        "type": "boolean",
                        "default": true,
                        "description": "Report the repair plan without changing the board."
                    },
                    "expected_plan_revision": {
                        "type": "string",
                        "description": "Exact plan_revision returned by a current dry run; required for apply."
                    }
                },
                "required": ["board"]
            }),
            |args, ctx| async move { handle_repair_corrupted_footprints(args, ctx).await }
        )
        .with_board_access(crate::tools::BoardAccess::LiveOnly),
        tool!(
            "find_component",
            "Find a footprint on the board by reference designator and return its position and \
             KIID. Optional kiid pins one footprint: without it, a reference several footprints \
             share (such as 'REF**') is refused as ambiguous_target naming each KIID.",
            json!({
                "type": "object",
                "properties": {
                    "board":     { "type": "string" },
                    "reference": { "type": "string" },
                    "kiid":      { "type": "string", "description": "Exact footprint KIID (UUID) to pin one instance or disambiguate duplicate references (optional)" }
                },
                "required": ["board", "reference"]
            }),
            |args, ctx| async move { handle_find_component(args, ctx).await }
        )
        .with_board_access(crate::tools::BoardAccess::LiveOnly),
        super::pcb_footprint_update::tool(),
        tool!(
            "list_board_footprint_graphics",
            "List the graphic items inside a footprint placed on the board — silkscreen, fabrication, and courtyard artwork — with the UUID needed to edit one. Points are footprint-local millimetres, as the .kicad_mod shows them. Each item reports 'editable', plus 'outlines' and 'holes' for polygons: 'points' covers the first outline only, so an item with more than one outline or any holes is reported but cannot be edited here. Requires KiCAD running with the board open.",
            json!({
                "type": "object",
                "properties": {
                    "board":     { "type": "string" },
                    "reference": { "type": "string", "description": "Reference designator, e.g. 'J2'" },
                    "layer":     { "type": "string", "description": "Only list items on this layer, e.g. 'F.SilkS' (optional)" }
                },
                "required": ["board", "reference"]
            }),
            |args, ctx| async move { handle_list_board_footprint_graphics(args, ctx).await }
        )
        .with_board_access(crate::tools::BoardAccess::LiveOnly),
        tool!(
            "edit_board_footprint_graphic",
            "Replace the vertices of a polygon inside a footprint placed on the board, selected by UUID. Points are footprint-local millimetres, as the .kicad_mod shows them. Use this to bring one placed instance in line with a library change without re-placing the part. Only a single-outline polygon with no holes can be replaced; anything else is refused by name rather than flattened. Requires KiCAD running with the board open.",
            json!({
                "type": "object",
                "properties": {
                    "board":     { "type": "string" },
                    "reference": { "type": "string", "description": "Reference designator, e.g. 'J2'" },
                    "uuid":      { "type": "string", "description": "UUID of the graphic item, from list_board_footprint_graphics" },
                    "points": {
                        "type": "array",
                        "description": "Replacement vertices in footprint-local millimetres; at least 3.",
                        "items": {
                            "type": "object",
                            "properties": {
                                "x": { "type": "number" },
                                "y": { "type": "number" }
                            },
                            "required": ["x", "y"]
                        }
                    }
                },
                "required": ["board", "reference", "uuid", "points"]
            }),
            |args, ctx| async move { handle_edit_board_footprint_graphic(args, ctx).await }
        )
        .with_board_access(crate::tools::BoardAccess::LiveOnly),
        tool!(
            "get_component_pads",
            "Return live board-space pad positions, effective rotations, physical geometry, drills, \
             copper-layer shapes, layers and net assignments for a footprint. \
             Reads the board open in KiCad when it is reachable and falls back to the file only \
             when no live KiCad holds this board — IPC unreachable, or that board not open \
             ('board_source' selects auto, live, or saved) — \
             'source', 'sources' and 'source_evidence' say which, so unsaved placements are visible without a save. \
             A pad's 'net' is its net name, \"\" if the pad carries no net \
             (unconnected), or — reading the file — null if the net node is present \
             but unreadable; treat null as an error, not as an unconnected pad.",
            json!({
                "type": "object",
                "properties": {
                    "board":        { "type": "string" },
                    "reference":    { "type": "string" },
                    "board_source": crate::tools::board_source::board_source_schema()
                },
                "required": ["board", "reference"]
            }),
            |args, ctx| async move { handle_get_component_pads(args, ctx).await }
        )
        .with_board_access(crate::tools::BoardAccess::LivePreferredWithFallback),
        tool!(
            "get_pad_position",
            "Return the live board-space position, effective rotation, physical geometry, drill, \
             layers and net of a specific pad number on a footprint ('board_source' selects auto, \
             live, or saved).",
            json!({
                "type": "object",
                "properties": {
                    "board":        { "type": "string" },
                    "reference":    { "type": "string" },
                    "pad_number":   { "type": "string" },
                    "board_source": crate::tools::board_source::board_source_schema()
                },
                "required": ["board", "reference", "pad_number"]
            }),
            |args, ctx| async move { handle_get_pad_position(args, ctx).await }
        )
        .with_board_access(crate::tools::BoardAccess::LivePreferredWithFallback),
        tool!(
            "get_component_list",
            "List all footprints on the board with their positions, layers, and values.",
            json!({
                "type": "object",
                "properties": {
                    "board": { "type": "string" }
                },
                "required": ["board"]
            }),
            |args, ctx| async move { handle_get_component_list(args, ctx).await }
        )
        .with_board_access(crate::tools::BoardAccess::LiveOnly),
        tool!(
            "place_component_array",
            "Place multiple copies of a footprint in a grid or line array via KiCAD IPC.",
            json!({
                "type": "object",
                "properties": {
                    "board":        { "type": "string" },
                    "footprint":    { "type": "string" },
                    "start_x":      { "type": "number" },
                    "start_y":      { "type": "number" },
                    "count_x":      { "type": "integer", "description": "Number of columns" },
                    "count_y":      { "type": "integer", "description": "Number of rows", "default": 1 },
                    "spacing_x":    { "type": "number", "description": "Column spacing in mm" },
                    "spacing_y":    { "type": "number", "description": "Row spacing in mm", "default": 0 },
                    "ref_prefix":   { "type": "string", "description": "Reference prefix (e.g. 'R')", "default": "U" },
                    "ref_start":    { "type": "integer", "description": "Starting reference number", "default": 1 }
                },
                "required": ["board", "footprint", "start_x", "start_y", "count_x", "spacing_x"]
            }),
            |args, ctx| async move { handle_place_array(args, ctx).await }
        )
        .with_board_access(crate::tools::BoardAccess::LiveOnly),
        tool!(
            "align_components",
            "Align multiple footprints along a common X or Y axis via KiCAD IPC.",
            json!({
                "type": "object",
                "properties": {
                    "board":       { "type": "string" },
                    "references":  { "type": "array", "items": { "type": "string" } },
                    "axis":        { "type": "string", "description": "'x' or 'y'", "default": "x" },
                    "value":       { "type": "number", "description": "Target coordinate to align to" }
                },
                "required": ["board", "references", "value"]
            }),
            |args, ctx| async move { handle_align_components(args, ctx).await }
        )
        .with_board_access(crate::tools::BoardAccess::LiveOnly),
        tool!(
            "duplicate_component",
            "Duplicate an existing footprint at a new position via KiCAD IPC.",
            json!({
                "type": "object",
                "properties": {
                    "board":         { "type": "string" },
                    "reference":     { "type": "string", "description": "Reference to duplicate" },
                    "new_reference": { "type": "string", "description": "New reference designator" },
                    "x":             { "type": "number" },
                    "y":             { "type": "number" }
                },
                "required": ["board", "reference", "new_reference", "x", "y"]
            }),
            |args, ctx| async move { handle_duplicate_component(args, ctx).await }
        )
        .with_board_access(crate::tools::BoardAccess::LiveOnly),
        tool!(
            "get_board_2d_view",
            "Render the saved .kicad_pcb file on disk (unsaved KiCad editor changes are excluded) \
             with kicad-cli and return provenance metadata alongside a base64 PNG. Note this is \
             kicad-cli's 3-D board render viewed from the top, not a layer plot -- there is \
             no layer selection. Use export_svg for layer-aware 2-D output.",
            json!({
                "type": "object",
                "properties": {
                    "board":  { "type": "string" },
                    "width":  { "type": "integer", "default": 800, "description": "Render width in pixels, clamped to 100-4000 (kept small since the image lands in LLM context, raise it when detail matters)" },
                    "height": { "type": "integer", "default": 600, "description": "Render height in pixels, clamped to 100-4000" }
                },
                "required": ["board"]
            }),
            |args, ctx| async move { handle_get_board_2d_view(args, ctx).await }
        )
        .with_board_access(crate::tools::BoardAccess::CliSaved),
    ];
    tools.push(super::placed_footprint_models::tool());
    tools
}

// ─── Handlers ─────────────────────────────────────────────────────────────────

/// Whether `source` is a pre-6.0 `(module …)` footprint. KiCad 6 renamed the
/// root to `footprint` and began quoting every string; generators such as
/// easyeda2kicad still write the old form.
fn is_legacy_module_footprint(source: &str) -> bool {
    copperpour_sexp::parse_sexp(source)
        .ok()
        .is_some_and(|root| root.head() == Some("module"))
}

/// Flip a footprint `place_component` has just created on F.Cu over to B.Cu
/// with KiCad's native `FlipItems` — the transform the GUI's F key performs,
/// 3D models included.
///
/// The flip keeps the anchor and negates the orientation, which is why the
/// footprint was built at the negated rotation. When the flip cannot be made
/// the new footprint is deleted again, so a refusal does not leave a part on
/// the wrong side of the board; `Ok(Err(…))` carries that refusal.
fn flip_new_footprint_to_back(
    client: &KiCadIpcClient,
    reference: &str,
) -> anyhow::Result<Result<copperpour_ipc::IpcFootprint, CallToolResult>> {
    use copperpour_ipc::IpcFlipOutcome;
    let removal_note = |removed: bool| {
        if removed {
            "The footprint created on F.Cu was removed again."
        } else {
            "Removing the footprint created on F.Cu could not be confirmed; inspect the board."
        }
    };
    let outcome = match client.flip_footprint(reference, None, "B.Cu") {
        Ok(outcome) => outcome,
        Err(error) => {
            let removed = client.delete_footprint(reference).is_ok();
            return Err(error.context(format!(
                "flipping new footprint '{reference}' to B.Cu failed. {}",
                removal_note(removed)
            )));
        }
    };
    match outcome {
        IpcFlipOutcome::Flipped { .. } => {
            let placed = client.get_footprint(reference)?.with_context(|| {
                format!("footprint '{reference}' is missing from the read-back after its flip")
            })?;
            if placed.layer != "B.Cu" {
                return Ok(Err(crate::tools::ipc_uncertain_result(&format!(
                    "footprint '{reference}' reads back on {} after KiCad reported flipping it \
                     to B.Cu",
                    placed.layer
                ))));
            }
            Ok(Ok(placed))
        }
        IpcFlipOutcome::Unsupported { kicad_version } => {
            let removed = client.delete_footprint(reference).is_ok();
            Ok(Err(CallToolResult::error_kind(
                crate::mcp::error::ToolErrorKind::UnsupportedCapability {
                    capability: "place_component on B.Cu".to_string(),
                    kicad_version: kicad_version.map(|version| version.full_version),
                },
                format!(
                    "Placing on B.Cu over IPC uses KiCad's native footprint flip, which needs \
                     KiCad {FLIP_ITEMS_MIN_KICAD_VERSION} or newer. {}",
                    removal_note(removed)
                ),
            )))
        }
        IpcFlipOutcome::StaleTarget { reason, .. }
        | IpcFlipOutcome::AmbiguousTarget { reason, .. } => {
            let removed = client.delete_footprint(reference).is_ok();
            Err(anyhow::anyhow!(
                "flipping new footprint '{reference}' to B.Cu was refused: {reason}. {}",
                removal_note(removed)
            ))
        }
        uncertain @ IpcFlipOutcome::Uncertain { .. } => Ok(Err(flip_outcome_result(uncertain))),
    }
}

async fn handle_place_component(
    args: &serde_json::Value,
    ctx: &ToolContext,
) -> anyhow::Result<CallToolResult> {
    let board = get_path(args, "board")?;
    let footprint = match require_str(args, "footprint") {
        Ok(v) => v.to_string(),
        Err(e) => return Ok(e),
    };
    let reference = match require_str(args, "reference") {
        Ok(v) => v.to_string(),
        Err(e) => return Ok(e),
    };
    let x = match require_f64(args, "x") {
        Ok(v) => v,
        Err(e) => return Ok(e),
    };
    let y = match require_f64(args, "y") {
        Ok(v) => v,
        Err(e) => return Ok(e),
    };
    let rotation = args["rotation"].as_f64().unwrap_or(0.0);
    let layer = args["layer"].as_str().unwrap_or("F.Cu").to_string();
    let on_back = match layer.as_str() {
        "F.Cu" => false,
        "B.Cu" => true,
        other => {
            return Ok(crate::tools::invalid_arg(
                "layer",
                &format!("footprints are placed on F.Cu or B.Cu, got '{other}'"),
            ))
        }
    };
    let (library_path, source) =
        match read_footprint_source_or_error(&footprint, &board, Some("footprint")) {
            Ok(loaded) => loaded,
            Err(refusal) => return Ok(refusal),
        };
    // A pre-6.0 `(module …)` file — what easyeda2kicad and many vendor
    // downloads still write — is modernised by KiCad's own converter in a
    // private copy, so the library on disk is left exactly as it was.
    let source = if is_legacy_module_footprint(&source) {
        let entry = footprint
            .split_once(':')
            .map_or(footprint.as_str(), |(_, entry)| entry);
        match super::cli::upgrade_legacy_footprint(&ctx.config.kicad_cli, &source, entry).await {
            Ok(upgraded) => upgraded,
            Err(error) => {
                let message = format!(
                    "{} is a pre-6.0 (module …) footprint and kicad-cli could not upgrade a \
                     copy of it: {error:#}. Convert the library with `kicad-cli fp upgrade \
                     <dir-or-file>`, then retry.",
                    library_path.display()
                );
                return Ok(CallToolResult::error_kind(
                    crate::mcp::error::ToolErrorKind::MalformedFile {
                        path: library_path.display().to_string(),
                        reason: format!("pre-6.0 (module …) footprint upgrade failed: {error:#}"),
                    },
                    message,
                ));
            }
        }
    } else {
        source
    };
    let prepared = match PreparedFootprintPlacement::from_library(
        &source,
        &footprint,
        &reference,
        None,
        x,
        y,
        rotation,
        &layer,
        board.parent(),
    ) {
        Ok(prepared) => prepared,
        Err(error) => {
            let message = error.to_string();
            return Ok(CallToolResult::error_kind(
                crate::mcp::error::ToolErrorKind::MalformedFile {
                    path: library_path.display().to_string(),
                    reason: message.clone(),
                },
                message,
            ));
        }
    };

    // Try IPC first. The fallback gate is the typed transport classification:
    // only when the transport is unreachable and this server has never
    // observed the requested board live is the guarded file path available.
    // A KiCad that answered — even with an error — fails closed.
    let footprint_ipc = footprint.clone();
    let reference_ipc = reference.clone();
    let requested_board = board.clone();
    let prepared_ipc = prepared.clone();
    let attempt = attempt_ipc_write(ctx, &board, "placement", move |c| {
        let (carried, older_kicad) = prepared_ipc.resolve_kicad_carried(c)?;
        let item = prepared_ipc.ipc_item(&footprint_ipc, &reference_ipc, x, y, carried)?;
        let placed = c.place_footprint_item(&requested_board, &reference_ipc, item)?;
        let placed = if on_back {
            match flip_new_footprint_to_back(c, &reference_ipc)? {
                Ok(placed) => placed,
                Err(refusal) => return Ok(Err(refusal)),
            }
        } else {
            placed
        };
        let document = c.find_open_board(&requested_board)?;
        let models = c.footprint_3d_model_snapshot_in(document, &reference_ipc)?;
        Ok(Ok((placed, models.models, older_kicad)))
    })
    .await?;

    match attempt {
        BoardWrite::Ipc(Ok((fp, models, older_kicad))) => {
            let mut response = json!({
                "placed": fp.reference,
                "footprint": fp.footprint,
                "x": fp.position.x, "y": fp.position.y,
                "rotation": fp.rotation, "layer": fp.layer,
                "models": models,
                "source": "ipc",
                "undo": if on_back {
                    "Two KiCad undo steps: the placement on F.Cu, then KiCad's native flip to B.Cu."
                } else {
                    "One KiCad undo step reverses the placement."
                }
            });
            if let Some(warning) = prepared.library_fields_not_carried(&footprint, &reference) {
                response["warning"] = json!(warning);
            }
            let not_carried = prepared.not_carried(older_kicad.as_ref());
            if !not_carried.is_empty() {
                response["not_carried"] = json!(not_carried);
            }
            Ok(CallToolResult::json(&response))
        }
        BoardWrite::Ipc(Err(refusal)) => Ok(refusal),
        BoardWrite::Refused(result) => Ok(result),
        BoardWrite::File(reason) => {
            // No live KiCad on the other end of this transport: fall back to
            // editing the board file directly.
            if board_contains_reference(&board, &reference)? {
                return Ok(CallToolResult::error_kind(
                    crate::mcp::error::ToolErrorKind::InvalidArgument {
                        field: "reference".to_string(),
                        reason: format!(
                            "footprint reference '{reference}' already exists on the board"
                        ),
                    },
                    format!("Footprint reference '{reference}' already exists on the board"),
                ));
            }
            if let Err(error) = prepared.file_block() {
                let reason = format!("{error:#}");
                return Ok(CallToolResult::error_kind(
                    crate::mcp::error::ToolErrorKind::PlanBlocked {
                        operation: "place_component".to_string(),
                        reasons: vec![reason.clone()],
                    },
                    format!("{reason}. The board file was not modified."),
                ));
            }
            prepared.insert_into_board(&board)?;
            Ok(CallToolResult::json(&json!({
                "placed": reference,
                "footprint": footprint,
                "x": x, "y": y, "rotation": rotation, "layer": layer,
                "source": "file",
                "fallback_reason": reason.evidence(),
                "warning": reason.warning()
            })))
        }
    }
}

async fn handle_move_component(
    args: &serde_json::Value,
    ctx: &ToolContext,
) -> anyhow::Result<CallToolResult> {
    let board = get_path(args, "board")?;
    let reference = match require_str(args, "reference") {
        Ok(v) => v.to_string(),
        Err(e) => return Ok(e),
    };
    let x = match require_f64(args, "x") {
        Ok(v) => v,
        Err(e) => return Ok(e),
    };
    let y = match require_f64(args, "y") {
        Ok(v) => v,
        Err(e) => return Ok(e),
    };

    let options = match CheckOptions::from_args(args) {
        Ok(options) => options,
        Err(e) => return Ok(e),
    };

    let ref_ipc = reference.clone();
    let requests = vec![PoseRequest::Move {
        reference: reference.clone(),
        x_mm: x,
        y_mm: y,
    }];
    let named = requests.clone();
    match attempt_ipc_write_naming(
        ctx,
        &board,
        "move",
        move |refusal| pcb_copper_check::refusal_kind("move_component", &named, refusal),
        move |c, document| {
            pcb_copper_check::check_then_apply(
                c,
                document.clone(),
                "move_component",
                &requests,
                options,
                |c| c.move_footprint_verified_in(document, &ref_ipc, x, y),
            )
        },
    )
    .await?
    {
        BoardWrite::Ipc(Checked::Applied { check, applied }) => Ok(check.applied_result(
            json!({ "moved": reference, "x": applied.x, "y": applied.y, "source": "ipc" }),
        )),
        BoardWrite::Ipc(Checked::Blocked(check)) => Ok(check.blocked_result("move_component")),
        BoardWrite::Ipc(Checked::DryRun(check)) => Ok(check.dry_run_result()),
        BoardWrite::Ipc(Checked::Refused(refused)) => Ok(refused),
        BoardWrite::Refused(result) => Ok(result),
        BoardWrite::File(reason) if options.dry_run => closed_board_dry_run(
            &board,
            &[(reference, FootprintPlacementUpdate::Move { x, y })],
            &reason,
        ),
        BoardWrite::File(reason) => {
            match update_closed_board_footprint(
                &board,
                &reference,
                FootprintPlacementUpdate::Move { x, y },
            ) {
                Ok(()) => Ok(CallToolResult::json(&json!({
                    "moved": reference,
                    "x": x,
                    "y": y,
                    "source": "file",
                    "fallback_reason": reason.evidence(),
                    "warning": reason.warning(),
                    "copper_check": pcb_copper_check::not_run_on_closed_board()
                }))),
                Err(error) => error.into_result(&board),
            }
        }
    }
}

async fn handle_list_board_footprint_graphics(
    args: &serde_json::Value,
    ctx: &ToolContext,
) -> anyhow::Result<CallToolResult> {
    let reference = match require_str(args, "reference") {
        Ok(v) => v.to_string(),
        Err(e) => return Ok(e),
    };
    let layer_filter = args["layer"].as_str().map(|s| s.to_string());

    let ref_ipc = reference.clone();
    let graphics: Vec<copperpour_ipc::types::IpcFootprintGraphic> =
        ipc!(ctx, args, |c| c.list_footprint_graphics(&ref_ipc));

    let graphics: Vec<_> = graphics
        .into_iter()
        .filter(|g| layer_filter.as_deref().is_none_or(|l| g.layer == l))
        .collect();

    Ok(CallToolResult::json(&json!({
        "count": graphics.len(),
        "reference": reference,
        "graphics": graphics,
    })))
}

async fn handle_edit_board_footprint_graphic(
    args: &serde_json::Value,
    ctx: &ToolContext,
) -> anyhow::Result<CallToolResult> {
    let reference = match require_str(args, "reference") {
        Ok(v) => v.to_string(),
        Err(e) => return Ok(e),
    };
    let uuid = match require_str(args, "uuid") {
        Ok(v) => v.to_string(),
        Err(e) => return Ok(e),
    };
    // A malformed `points` is the caller's mistake, so it gets the structured
    // `invalid_argument` the rest of this file returns — bubbling `anyhow`
    // here would surface it as a handler error with the offending field
    // flattened into prose, which is the class #194 is open against.
    let points = match parse_points(&args["points"]) {
        Ok(p) => p,
        Err(e) => return Ok(e),
    };

    let (ref_ipc, uuid_ipc, pts) = (reference.clone(), uuid.clone(), points.clone());
    let kind: Result<String, CallToolResult> = ipc!(ctx, args, |c, document| {
        match super::pcb_footprint_update::refuse_owned_groups(
            c,
            &document,
            "edit_board_footprint_graphic",
            "edit the graphic of",
            Some(&[ref_ipc.as_str()]),
        )? {
            Some(refused) => Ok(Err(refused)),
            None => c
                .set_footprint_graphic_points(&ref_ipc, &uuid_ipc, &pts)
                .map(Ok),
        }
    });
    let kind = match kind {
        Ok(kind) => kind,
        Err(refused) => return Ok(refused),
    };

    Ok(CallToolResult::json(&json!({
        "edited": reference,
        "uuid": uuid,
        "kind": kind,
        "points": points.len(),
    })))
}

async fn handle_rotate_component(
    args: &serde_json::Value,
    ctx: &ToolContext,
) -> anyhow::Result<CallToolResult> {
    let board = get_path(args, "board")?;
    let reference = match require_str(args, "reference") {
        Ok(v) => v.to_string(),
        Err(e) => return Ok(e),
    };
    let rotation = match require_f64(args, "rotation") {
        Ok(v) => v,
        Err(e) => return Ok(e),
    };

    let options = match CheckOptions::from_args(args) {
        Ok(options) => options,
        Err(e) => return Ok(e),
    };

    let ref_ipc = reference.clone();
    let requests = vec![PoseRequest::Rotate {
        reference: reference.clone(),
        rotation_deg: rotation,
    }];
    let named = requests.clone();
    match attempt_ipc_write_naming(
        ctx,
        &board,
        "rotation",
        move |refusal| pcb_copper_check::refusal_kind("rotate_component", &named, refusal),
        move |c, document| {
            pcb_copper_check::check_then_apply(
                c,
                document,
                "rotate_component",
                &requests,
                options,
                |c| c.rotate_footprint(&ref_ipc, rotation),
            )
        },
    )
    .await?
    {
        BoardWrite::Ipc(Checked::Applied { check, .. }) => Ok(check.applied_result(json!({
            "rotated": reference,
            "rotation": rotation,
            "source": "ipc"
        }))),
        BoardWrite::Ipc(Checked::Blocked(check)) => Ok(check.blocked_result("rotate_component")),
        BoardWrite::Ipc(Checked::DryRun(check)) => Ok(check.dry_run_result()),
        BoardWrite::Ipc(Checked::Refused(refused)) => Ok(refused),
        BoardWrite::Refused(result) => Ok(result),
        BoardWrite::File(reason) if options.dry_run => closed_board_dry_run(
            &board,
            &[(reference, FootprintPlacementUpdate::Rotate { rotation })],
            &reason,
        ),
        BoardWrite::File(reason) => {
            match update_closed_board_footprint(
                &board,
                &reference,
                FootprintPlacementUpdate::Rotate { rotation },
            ) {
                Ok(()) => Ok(CallToolResult::json(&json!({
                    "rotated": reference,
                    "rotation": rotation,
                    "source": "file",
                    "fallback_reason": reason.evidence(),
                    "warning": reason.warning(),
                    "copper_check": pcb_copper_check::not_run_on_closed_board()
                }))),
                Err(error) => error.into_result(&board),
            }
        }
    }
}

fn invalid_placement(field: String, reason: impl Into<String>) -> CallToolResult {
    let reason = reason.into();
    CallToolResult::error_kind(
        crate::mcp::error::ToolErrorKind::InvalidArgument {
            field: field.clone(),
            reason: reason.clone(),
        },
        format!("Argument '{field}' is invalid: {reason}"),
    )
}

fn parse_component_placements(
    args: &serde_json::Value,
) -> Result<Vec<copperpour_ipc::types::IpcFootprintPlacement>, CallToolResult> {
    let values = require_array(args, "placements")?;
    if values.is_empty() {
        return Err(invalid_placement(
            "placements".to_string(),
            "must contain at least one placement",
        ));
    }

    let mut references = HashSet::new();
    let mut placements = Vec::with_capacity(values.len());
    for (index, value) in values.iter().enumerate() {
        let field = |name: &str| format!("placements[{index}].{name}");
        let Some(object) = value.as_object() else {
            return Err(invalid_placement(
                format!("placements[{index}]"),
                "must be an object",
            ));
        };
        let reference = object
            .get("reference")
            .and_then(serde_json::Value::as_str)
            .filter(|reference| !reference.is_empty())
            .ok_or_else(|| invalid_placement(field("reference"), "missing or empty"))?
            .to_string();
        if !references.insert(reference.clone()) {
            return Err(invalid_placement(
                field("reference"),
                format!("duplicate footprint reference '{reference}'"),
            ));
        }
        let number = |name: &str| {
            object
                .get(name)
                .and_then(serde_json::Value::as_f64)
                .ok_or_else(|| invalid_placement(field(name), "missing or not a number"))
        };
        placements.push(copperpour_ipc::types::IpcFootprintPlacement {
            reference,
            x: number("x")?,
            y: number("y")?,
            rotation: number("rotation")?,
        });
    }
    Ok(placements)
}

async fn handle_set_component_placements(
    args: &serde_json::Value,
    ctx: &ToolContext,
) -> anyhow::Result<CallToolResult> {
    let board = get_path(args, "board")?;
    let placements = match parse_component_placements(args) {
        Ok(placements) => placements,
        Err(error) => return Ok(error),
    };

    let options = match CheckOptions::from_args(args) {
        Ok(options) => options,
        Err(error) => return Ok(error),
    };

    let placements_ipc = placements.clone();
    let requests: Vec<_> = placements.iter().cloned().map(PoseRequest::Set).collect();
    let named = requests.clone();
    match attempt_ipc_write_naming(
        ctx,
        &board,
        "component placement batch",
        move |refusal| pcb_copper_check::refusal_kind("set_component_placements", &named, refusal),
        move |client, document| {
            pcb_copper_check::check_then_apply(
                client,
                document,
                "set_component_placements",
                &requests,
                options,
                |client| client.set_footprint_placements(&placements_ipc),
            )
        },
    )
    .await?
    {
        BoardWrite::Ipc(Checked::Applied { applied, check }) => Ok(check.applied_result(json!({
            "count": applied.len(),
            "placements": applied,
            "source": "ipc",
            "undo": "One KiCad undo step reverses the whole placement batch."
        }))),
        BoardWrite::Ipc(Checked::Blocked(check)) => {
            Ok(check.blocked_result("set_component_placements"))
        }
        BoardWrite::Ipc(Checked::DryRun(check)) => Ok(check.dry_run_result()),
        BoardWrite::Ipc(Checked::Refused(refused)) => Ok(refused),
        BoardWrite::Refused(result) => Ok(result),
        BoardWrite::File(reason) if options.dry_run => {
            let updates: Vec<_> = placements
                .iter()
                .map(|placement| {
                    (
                        placement.reference.clone(),
                        FootprintPlacementUpdate::Set {
                            x: placement.x,
                            y: placement.y,
                            rotation: placement.rotation,
                        },
                    )
                })
                .collect();
            closed_board_dry_run(&board, &updates, &reason)
        }
        BoardWrite::File(reason) => match update_closed_board_footprints(&board, &placements) {
            Ok(applied) => Ok(CallToolResult::json(&json!({
                "count": applied.len(),
                "placements": applied,
                "source": "file",
                "fallback_reason": reason.evidence(),
                "warning": reason.warning(),
                "copper_check": pcb_copper_check::not_run_on_closed_board()
            }))),
            Err(error) => error.into_result(&board),
        },
    }
}

/// Minimum KiCad version whose native `FlipItems` command Copperpour drives over
/// IPC (issue #604). Older reachable KiCad answers `AS_UNHANDLED`, which this
/// handler treats as authoritative capability evidence, never inferred from a
/// version string on its own.
const FLIP_ITEMS_MIN_KICAD_VERSION: &str = "10.0.6";

async fn handle_flip_component(
    args: &serde_json::Value,
    ctx: &ToolContext,
) -> anyhow::Result<CallToolResult> {
    let board = get_path(args, "board")?;
    let reference = match require_str(args, "reference") {
        Ok(value) => value.to_string(),
        Err(error) => return Ok(error),
    };
    let layer = match require_str(args, "layer") {
        Ok(value) if matches!(value, "F.Cu" | "B.Cu") => value.to_string(),
        Ok(value) => {
            return Ok(CallToolResult::error_kind(
                crate::mcp::error::ToolErrorKind::InvalidArgument {
                    field: "layer".to_string(),
                    reason: format!("must be F.Cu or B.Cu, got '{value}'"),
                },
                format!("Footprints can only be flipped between F.Cu and B.Cu, got '{value}'"),
            ))
        }
        Err(error) => return Ok(error),
    };
    let kiid = match optional_kiid(args) {
        Ok(kiid) => kiid,
        Err(error) => return Ok(error),
    };

    // KiCad 10.0.6 added a native FlipItems IPC command, so an open board now
    // flips through the same live-IPC-preferred-with-fallback shape as
    // set_component_placements, rather than refusing outright whenever KiCad
    // is reachable. The closed-board file fallback is unchanged (issue #604).
    let reference_ipc = reference.clone();
    let kiid_ipc = kiid.clone();
    let layer_ipc = layer.clone();
    match attempt_ipc_write(ctx, &board, "footprint flip", move |client| {
        client.flip_footprint(&reference_ipc, kiid_ipc.as_deref(), &layer_ipc)
    })
    .await?
    {
        BoardWrite::Ipc(outcome) => Ok(flip_outcome_result(outcome)),
        BoardWrite::Refused(result) => Ok(result),
        BoardWrite::File(reason) => {
            match set_closed_board_footprint_side(&board, &reference, kiid.as_deref(), &layer) {
                Ok(changed) => Ok(CallToolResult::json(&json!({
                    "flipped": reference,
                    "layer": layer,
                    "changed": changed,
                    "source": "file",
                    "fallback_reason": reason.evidence(),
                    "warning": reason.warning()
                }))),
                Err(error) => error.into_result(&board),
            }
        }
    }
}

/// Turn a [`copperpour_ipc::IpcFlipOutcome`] into the tool's `CallToolResult`.
///
/// `Flipped` is the only case reported as a plain success. `Unsupported` and
/// `Uncertain` are both IPC-observed states — the board is open and
/// reachable — so neither one may fall back to the file path; each gets its
/// own structured result naming what the caller should do next.
fn flip_outcome_result(outcome: copperpour_ipc::IpcFlipOutcome) -> CallToolResult {
    use copperpour_ipc::IpcFlipOutcome;
    match outcome {
        IpcFlipOutcome::Flipped {
            reference,
            kiid,
            previous_layer,
            layer,
            already_on_layer,
            models,
        } => CallToolResult::json(&json!({
            "flipped": reference,
            "kiid": kiid,
            "previous_layer": previous_layer,
            "layer": layer,
            "already_on_layer": already_on_layer,
            "models": models,
            "source": "ipc",
            "undo": "One KiCad undo step reverses the flip."
        })),
        IpcFlipOutcome::Unsupported { kicad_version } => CallToolResult::error_kind(
            crate::mcp::error::ToolErrorKind::UnsupportedCapability {
                capability: "flip_component".to_string(),
                kicad_version: kicad_version.map(|v| v.full_version),
            },
            format!(
                "The running KiCad endpoint answered AS_UNHANDLED for the native footprint-flip \
                 command. Copperpour requires KiCad {FLIP_ITEMS_MIN_KICAD_VERSION} or newer to flip \
                 a footprint on an open board over IPC. The board file was not modified."
            ),
        ),
        IpcFlipOutcome::StaleTarget { target, reason } => CallToolResult::error_kind(
            crate::mcp::error::ToolErrorKind::StaleTarget {
                target,
                reason: reason.clone(),
            },
            format!("flip_component refused: {reason}. Nothing was written."),
        ),
        IpcFlipOutcome::AmbiguousTarget {
            target,
            candidates,
            reason,
        } => ambiguous_footprint_result(
            target,
            candidates,
            format!("flip_component refused: {reason}. Nothing was written."),
        ),
        IpcFlipOutcome::Uncertain {
            reference,
            kiid,
            requested_layer,
            reason,
        } => CallToolResult::error_kind(
            crate::mcp::error::ToolErrorKind::MutationOutcomeUncertain {
                operation: "flip_component".to_string(),
                path: format!("{reference} ({kiid})"),
                reason: reason.clone(),
            },
            format!(
                "KiCad's footprint-flip mutation for '{reference}' toward {requested_layer} may \
                 or may not have taken effect: {reason}. Inspect the board in KiCad and reconcile \
                 before retrying — this may not be a no-op."
            ),
        ),
    }
}

async fn handle_delete_component(
    args: &serde_json::Value,
    ctx: &ToolContext,
) -> anyhow::Result<CallToolResult> {
    let reference = match require_str(args, "reference") {
        Ok(v) => v.to_string(),
        Err(e) => return Ok(e),
    };
    if reference.is_empty() {
        return Ok(crate::tools::invalid_arg("reference", "must not be empty"));
    }

    let ref_ipc = reference.clone();
    ipc!(ctx, args, |c, document| c
        .delete_footprint_in(document, &ref_ipc));
    Ok(CallToolResult::json(&json!({ "deleted": reference })))
}

async fn handle_edit_component(
    args: &serde_json::Value,
    ctx: &ToolContext,
) -> anyhow::Result<CallToolResult> {
    let reference = match require_str(args, "reference") {
        Ok(v) => v.to_string(),
        Err(e) => return Ok(e),
    };
    let kiid = match optional_kiid(args) {
        Ok(kiid) => kiid,
        Err(e) => return Ok(e),
    };
    let new_reference = match &args["new_reference"] {
        serde_json::Value::Null => None,
        serde_json::Value::String(value) => {
            if value.trim().is_empty() {
                return Ok(crate::tools::invalid_arg(
                    "new_reference",
                    "must not be empty",
                ));
            }
            if value.chars().any(|c| c.is_control() || c.is_whitespace()) {
                return Ok(crate::tools::invalid_arg(
                    "new_reference",
                    "must not contain whitespace or control characters",
                ));
            }
            Some(value.clone())
        }
        _ => {
            return Ok(crate::tools::invalid_arg(
                "new_reference",
                "must be a string",
            ))
        }
    };
    let edit = copperpour_ipc::IpcFootprintEdit {
        new_reference,
        value: args["value"].as_str().map(str::to_string),
        dnp: args["dnp"].as_bool(),
        exclude_from_bom: args["exclude_from_bom"].as_bool(),
        exclude_from_pos_files: args["exclude_from_pos_files"].as_bool(),
        locked: args["locked"].as_bool(),
    };
    let live_reference = reference.clone();
    let live_kiid = kiid.clone();
    if edit.is_empty() {
        let lookup = ipc!(ctx, args, |c, document| c.footprint_state_in(
            document,
            &live_reference,
            live_kiid.as_deref()
        ));
        return Ok(match lookup {
            copperpour_ipc::IpcFootprintLookupOutcome::Found(state) => {
                CallToolResult::json(&edit_component_body(&state, "unchanged", None, None))
            }
            copperpour_ipc::IpcFootprintLookupOutcome::StaleTarget { target, reason } => {
                CallToolResult::error_kind(
                    crate::mcp::error::ToolErrorKind::StaleTarget {
                        target,
                        reason: reason.clone(),
                    },
                    format!("edit_component refused: {reason}. Nothing was written."),
                )
            }
            copperpour_ipc::IpcFootprintLookupOutcome::AmbiguousTarget {
                target,
                candidates,
                reason,
            } => ambiguous_footprint_result(
                target,
                candidates,
                format!("edit_component refused: {reason}. Nothing was written."),
            ),
        });
    }
    let outcome = ipc!(ctx, args, |c, document| {
        match super::pcb_footprint_update::refuse_owned_groups(
            c,
            &document,
            "edit_component",
            "edit",
            Some(&[live_reference.as_str()]),
        )? {
            Some(refused) => Ok(Err(refused)),
            None => c
                .edit_footprint_in(document, &live_reference, live_kiid.as_deref(), &edit)
                .map(Ok),
        }
    });
    let outcome = match outcome {
        Ok(outcome) => outcome,
        Err(refused) => return Ok(refused),
    };
    Ok(edit_component_result(outcome, &get_path(args, "board")?))
}

/// Sent with every footprint `edit_component` sent back to KiCad.
const WHOLE_FOOTPRINT_WARNING: &str = "Whole-footprint IPC edits can drop symbol-unit mappings \
     on KiCad 10.0–10.0.7. They can also rewrite rule areas inside footprints. Readback verifies \
     neither. See https://github.com/Copperpour/Copperpour/blob/main/docs/TROUBLESHOOTING.md#footprint-unit-metadata-disappears-after-a-live-pcb-edit";

/// The flags `update_pcb_from_schematic` sets from a linked footprint's
/// symbol, as they would undo an edit of them on the board.
fn schematic_owned_flags_changed(
    before: &copperpour_ipc::IpcFootprintState,
    after: &copperpour_ipc::IpcFootprintState,
) -> Vec<&'static str> {
    let mut changed = Vec::new();
    if after.linked_to_schematic {
        if before.dnp != after.dnp {
            changed.push("dnp");
        }
        if before.exclude_from_bom != after.exclude_from_bom {
            changed.push("exclude_from_bom");
        }
    }
    changed
}

fn edit_component_state_json(state: &copperpour_ipc::IpcFootprintState) -> serde_json::Value {
    json!({
        "reference": state.reference,
        "value": state.value,
        "dnp": state.dnp,
        "exclude_from_bom": state.exclude_from_bom,
        "exclude_from_pos_files": state.exclude_from_pos_files,
        "locked": state.locked,
    })
}

/// `edit_component`'s answer: the footprint as KiCad holds it, at the top
/// level as the tool always answered, with `before` and the changed
/// properties when it sent an edit.
fn edit_component_body(
    state: &copperpour_ipc::IpcFootprintState,
    status: &str,
    before: Option<&copperpour_ipc::IpcFootprintState>,
    warning: Option<String>,
) -> serde_json::Value {
    let after = edit_component_state_json(state);
    let before_json = before.map(edit_component_state_json);
    let changed: Vec<&str> = match &before_json {
        Some(before_json) => [
            "reference",
            "value",
            "dnp",
            "exclude_from_bom",
            "exclude_from_pos_files",
            "locked",
        ]
        .into_iter()
        .filter(|key| before_json[*key] != after[*key])
        .collect(),
        None => Vec::new(),
    };
    json!({
        "status": status,
        "applied": status == "applied",
        "source": "ipc",
        "kiid": state.kiid,
        "reference": state.reference,
        "value": state.value,
        "footprint": state.footprint,
        "dnp": state.dnp,
        "exclude_from_bom": state.exclude_from_bom,
        "exclude_from_pos_files": state.exclude_from_pos_files,
        "locked": state.locked,
        "linked_to_schematic": state.linked_to_schematic,
        "changed": changed,
        "before": before_json.unwrap_or_else(|| after.clone()),
        "undo": if status == "applied" { json!("one KiCad undo step") } else { serde_json::Value::Null },
        "warning": warning,
    })
}

/// The optional `kiid` that pins one placed footprint, or the
/// `invalid_argument` refusal for one that is not a non-empty string.
fn optional_kiid(args: &serde_json::Value) -> Result<Option<String>, CallToolResult> {
    match &args["kiid"] {
        serde_json::Value::Null => Ok(None),
        serde_json::Value::String(value) => {
            if value.trim().is_empty() {
                return Err(crate::tools::invalid_arg("kiid", "must not be empty"));
            }
            Ok(Some(value.clone()))
        }
        _ => Err(crate::tools::invalid_arg("kiid", "must be a string")),
    }
}

/// `ambiguous_target` for a reference several placed footprints carry,
/// naming their KIIDs so the caller can retry with `kiid`.
fn ambiguous_footprint_result(
    target: String,
    candidates: Vec<String>,
    message: String,
) -> CallToolResult {
    CallToolResult::error_kind(
        crate::mcp::error::ToolErrorKind::AmbiguousTarget { target, candidates },
        message,
    )
}

fn edit_component_result(
    outcome: copperpour_ipc::IpcFootprintEditOutcome,
    board: &Path,
) -> CallToolResult {
    use copperpour_ipc::IpcFootprintEditOutcome;
    match outcome {
        IpcFootprintEditOutcome::Unchanged { current } => {
            CallToolResult::json(&edit_component_body(&current, "unchanged", None, None))
        }
        IpcFootprintEditOutcome::Applied { before, after } => {
            let mut warning = WHOLE_FOOTPRINT_WARNING.to_string();
            let owned = schematic_owned_flags_changed(&before, &after);
            if !owned.is_empty() {
                warning.push_str(&format!(
                    " '{}' is linked to a schematic symbol: update_pcb_from_schematic sets {} \
                     from the symbol again. Set the symbol too to keep the change \
                     (edit_schematic_component takes dnp and in_bom).",
                    after.reference,
                    owned.join(" and ")
                ));
            }
            CallToolResult::json(&edit_component_body(
                &after,
                "applied",
                Some(&before),
                Some(warning),
            ))
        }
        IpcFootprintEditOutcome::Blocked { reason } => CallToolResult::error_kind(
            crate::mcp::error::ToolErrorKind::PlanBlocked {
                operation: "edit_component".to_string(),
                reasons: vec![reason.clone()],
            },
            format!("edit_component refused: {reason}. Nothing was written."),
        ),
        IpcFootprintEditOutcome::StaleTarget { target, reason } => CallToolResult::error_kind(
            crate::mcp::error::ToolErrorKind::StaleTarget {
                target,
                reason: reason.clone(),
            },
            format!("edit_component refused: {reason}. Nothing was written."),
        ),
        IpcFootprintEditOutcome::AmbiguousTarget {
            target,
            candidates,
            reason,
        } => ambiguous_footprint_result(
            target,
            candidates,
            format!("edit_component refused: {reason}. Nothing was written."),
        ),
        IpcFootprintEditOutcome::Uncertain {
            expected,
            observed,
            mismatches,
            reason,
            ..
        } => {
            let detail = json!({
                "expected": edit_component_state_json(&expected),
                "observed": observed.as_ref().map(edit_component_state_json),
                "mismatches": mismatches,
            });
            CallToolResult::error_kind(
                crate::mcp::error::ToolErrorKind::MutationOutcomeUncertain {
                    operation: "edit_component".to_string(),
                    path: board.display().to_string(),
                    reason: if mismatches.is_empty() {
                        reason.clone()
                    } else {
                        format!("{reason}; {}", mismatches.join("; "))
                    },
                },
                format!(
                    "KiCad may have changed the footprint, but a fresh readback did not prove \
                     the requested result. If it did change, it is one KiCad undo step. Inspect \
                     before retrying. {reason}. Evidence: {detail}"
                ),
            )
        }
    }
}

fn footprint_instance_reference(
    footprint: &copperpour_ipc::gen::kiapi::board::types::FootprintInstance,
) -> String {
    footprint
        .reference_field
        .as_ref()
        .and_then(|field| field.text.as_ref())
        .and_then(|text| text.text.as_ref())
        .map(|text| text.text.clone())
        .unwrap_or_default()
}

fn issue_244_phantom_pad(pad: &copperpour_ipc::gen::kiapi::board::types::Pad) -> bool {
    use copperpour_ipc::gen::kiapi;

    let layerless = pad
        .pad_stack
        .as_ref()
        .is_none_or(|stack| stack.layers.is_empty());
    // Once an affected board is saved, KiCad can materialise proto3's empty
    // pad defaults as a PTH pad on *.Cu with a 1 nm drill. That is still #244,
    // not a real hole. The plan additionally requires a one-for-one missing
    // library graphic and an exact match of every legitimate library pad, so
    // a genuine unnumbered mechanical pad cannot be removed by this test.
    let normalised_zero_drill = pad.r#type == kiapi::board::types::PadType::PtPth as i32
        && pad
            .pad_stack
            .as_ref()
            .and_then(|stack| stack.drill.as_ref())
            .and_then(|drill| drill.diameter.as_ref())
            .is_some_and(|diameter| {
                diameter.x_nm.unsigned_abs() <= 1_000 && diameter.y_nm.unsigned_abs() <= 1_000
            });
    pad.number.is_empty()
        // KiCad may normalise an absent proto net into an empty Net message
        // when the board is saved and reloaded. Neither form names copper.
        && pad.net.as_ref().is_none_or(|net| net.name.is_empty())
        && (layerless || normalised_zero_drill)
}

fn issue_244_counts(
    footprint: &copperpour_ipc::gen::kiapi::board::types::FootprintInstance,
) -> anyhow::Result<(usize, usize, BTreeMap<String, usize>)> {
    use copperpour_ipc::gen::kiapi;

    let definition = footprint
        .definition
        .as_ref()
        .context("board footprint has no library definition")?;
    let mut phantom_pads = 0usize;
    let mut graphic_shapes = 0usize;
    let mut legitimate_pad_numbers = BTreeMap::new();
    for child in &definition.items {
        if copperpour_ipc::builders::any_is(child, "kiapi.board.types.Pad") {
            let pad = kiapi::board::types::Pad::decode(child.value.as_slice())
                .context("footprint declares a pad that cannot be decoded")?;
            if issue_244_phantom_pad(&pad) {
                phantom_pads += 1;
            } else {
                *legitimate_pad_numbers.entry(pad.number).or_insert(0) += 1;
            }
        } else if copperpour_ipc::builders::any_is(child, "kiapi.board.types.BoardGraphicShape") {
            graphic_shapes += 1;
        }
    }
    Ok((phantom_pads, graphic_shapes, legitimate_pad_numbers))
}

/// Replace only a footprint definition's mixed child list with a clean copy
/// built from its library. Everything owned by the placed instance — KIID,
/// placement, symbol path, flags and fields — stays on the original message.
/// Named/legitimate pads keep their live KIID, net and lock state by number;
/// only #244's anonymous, layerless pads disappear.
fn merge_clean_footprint_children(
    current: &prost_types::Any,
    clean: &prost_types::Any,
) -> anyhow::Result<prost_types::Any> {
    use copperpour_ipc::gen::kiapi;

    let mut current = kiapi::board::types::FootprintInstance::decode(current.value.as_slice())
        .context("KiCad returned an invalid current footprint")?;
    let mut clean = kiapi::board::types::FootprintInstance::decode(clean.value.as_slice())
        .context("the library produced an invalid clean footprint")?;
    let current_definition = current
        .definition
        .as_mut()
        .context("current footprint has no library definition")?;
    let clean_definition = clean
        .definition
        .as_mut()
        .context("clean footprint has no library definition")?;

    // #244 changed only BoardGraphicShape children into Pad children. BoardText
    // and any future non-shape child types survived and may contain deliberate
    // per-board customisation, so retain those exact live messages rather than
    // replacing the whole mixed child list from the library.
    let preserved_non_shapes = current_definition
        .items
        .iter()
        .filter(|child| {
            !copperpour_ipc::builders::any_is(child, "kiapi.board.types.Pad")
                && !copperpour_ipc::builders::any_is(child, "kiapi.board.types.BoardGraphicShape")
        })
        .cloned()
        .collect::<Vec<_>>();

    let mut current_pads: BTreeMap<String, VecDeque<kiapi::board::types::Pad>> = BTreeMap::new();
    for child in &current_definition.items {
        if !copperpour_ipc::builders::any_is(child, "kiapi.board.types.Pad") {
            continue;
        }
        let pad = kiapi::board::types::Pad::decode(child.value.as_slice())
            .context("current footprint declares a pad that cannot be decoded")?;
        if !issue_244_phantom_pad(&pad) {
            current_pads
                .entry(pad.number.clone())
                .or_default()
                .push_back(pad);
        }
    }

    let mut clean_items = Vec::new();
    for mut child in std::mem::take(&mut clean_definition.items) {
        if !copperpour_ipc::builders::any_is(&child, "kiapi.board.types.Pad") {
            if copperpour_ipc::builders::any_is(&child, "kiapi.board.types.BoardGraphicShape") {
                clean_items.push(child);
            }
            continue;
        }
        let mut pad = kiapi::board::types::Pad::decode(child.value.as_slice())
            .context("clean footprint declares a pad that cannot be decoded")?;
        let preserved = current_pads
            .get_mut(&pad.number)
            .and_then(VecDeque::pop_front)
            .with_context(|| {
                format!(
                    "clean library footprint has pad '{}' that the board footprint does not",
                    pad.number
                )
            })?;
        pad.id = preserved.id;
        pad.net = preserved.net;
        pad.locked = preserved.locked;
        child = copperpour_ipc::builders::pack_any(&pad, "kiapi.board.types.Pad");
        clean_items.push(child);
    }
    let leftovers = current_pads
        .iter()
        .filter(|(_, pads)| !pads.is_empty())
        .map(|(number, pads)| format!("'{number}' x{}", pads.len()))
        .collect::<Vec<_>>();
    if !leftovers.is_empty() {
        anyhow::bail!(
            "board footprint has legitimate pads absent from the library: {}",
            leftovers.join(", ")
        );
    }

    clean_items.extend(preserved_non_shapes);
    current_definition.items = clean_items;
    Ok(copperpour_ipc::builders::pack_any(
        &current,
        "kiapi.board.types.FootprintInstance",
    ))
}

fn merge_corrupted_footprint_candidate(
    reference: &str,
    current: &prost_types::Any,
    clean: &prost_types::Any,
    diagnostics: &mut Vec<serde_json::Value>,
) -> Option<prost_types::Any> {
    match merge_clean_footprint_children(current, clean) {
        Ok(repaired) => Some(repaired),
        Err(error) => {
            diagnostics.push(json!({
                "reference": reference,
                "code": "repair_merge_failed",
                "message": format!("{error:#}")
            }));
            None
        }
    }
}

struct CorruptedFootprintRepair {
    reference: String,
    footprint: String,
    phantom_pads: usize,
    restored_graphics: usize,
    expected_graphics: usize,
    repaired_item: prost_types::Any,
}

fn repair_plan_revision(
    board: &Path,
    repairs: &[CorruptedFootprintRepair],
    source_digests: &[(String, Vec<u8>)],
) -> String {
    let mut hasher = Sha256::new();
    hasher.update(board.as_os_str().as_encoded_bytes());
    for repair in repairs {
        hasher.update(repair.reference.as_bytes());
        hasher.update(repair.footprint.as_bytes());
        hasher.update(&repair.repaired_item.value);
    }
    for (reference, source) in source_digests {
        hasher.update(reference.as_bytes());
        hasher.update(source);
    }
    encode_hex(&hasher.finalize())
}

async fn handle_repair_corrupted_footprints(
    args: &serde_json::Value,
    ctx: &ToolContext,
) -> anyhow::Result<CallToolResult> {
    use copperpour_ipc::gen::kiapi;

    let board = get_path(args, "board")?;
    let dry_run = args["dry_run"].as_bool().unwrap_or(true);
    let expected_revision = args["expected_plan_revision"].as_str().map(str::to_string);
    if !dry_run && expected_revision.is_none() {
        return Ok(CallToolResult::error_kind(
            crate::mcp::error::ToolErrorKind::InvalidArgument {
                field: "expected_plan_revision".to_string(),
                reason: "required when dry_run is false".to_string(),
            },
            "Apply requires the plan revision returned by a current dry run.",
        ));
    }

    let selected = if args
        .get("references")
        .is_none_or(serde_json::Value::is_null)
    {
        None
    } else {
        let values = match require_array(args, "references") {
            Ok(values) => values,
            Err(error) => return Ok(error),
        };
        let mut selected = HashSet::new();
        for (index, value) in values.iter().enumerate() {
            let Some(reference) = value.as_str().filter(|reference| !reference.is_empty()) else {
                return Ok(CallToolResult::error_kind(
                    crate::mcp::error::ToolErrorKind::InvalidArgument {
                        field: "references".to_string(),
                        reason: format!("item {index} must be a non-empty string"),
                    },
                    format!("Argument 'references' item {index} must be a non-empty string"),
                ));
            };
            if !selected.insert(reference.to_string()) {
                return Ok(CallToolResult::error_kind(
                    crate::mcp::error::ToolErrorKind::InvalidArgument {
                        field: "references".to_string(),
                        reason: format!("duplicate reference '{reference}'"),
                    },
                    format!("Argument 'references' contains duplicate '{reference}'"),
                ));
            }
        }
        Some(selected)
    };

    let board_for_ipc = board.clone();
    let expected_for_ipc = expected_revision.clone();
    let outcome = attempt_ipc_write(
        ctx,
        &board,
        "legacy footprint repair",
        move |client| {
            let document = client.find_open_board(&board_for_ipc)?;
            let summaries = client
                .list_footprints_in(document.clone())?
                .into_iter()
                .map(|footprint| (footprint.reference.clone(), footprint))
                .collect::<HashMap<_, _>>();
            let items = client.get_items_in(
                document.clone(),
                kiapi::common::types::KiCadObjectType::KotPcbFootprint,
            )?;
            let mut seen = HashSet::new();
            let mut repairs = Vec::new();
            let mut source_digests = Vec::new();
            let mut diagnostics = Vec::new();
            let mut not_carried = Vec::new();
            let mut not_carried_seen = HashSet::new();
            let mut kicad_version: Option<copperpour_ipc::IpcKiCadVersion> = None;

            for item in items {
                let footprint = kiapi::board::types::FootprintInstance::decode(
                    item.value.as_slice(),
                )
                .context("KiCad returned an invalid footprint instance")?;
                let reference = footprint_instance_reference(&footprint);
                if reference.is_empty()
                    || selected
                        .as_ref()
                        .is_some_and(|selected| !selected.contains(&reference))
                {
                    continue;
                }
                seen.insert(reference.clone());
                let (phantom_pads, current_graphics, board_pad_numbers) =
                    issue_244_counts(&footprint)?;
                if phantom_pads == 0 {
                    continue;
                }

                let Some(summary) = summaries.get(&reference) else {
                    diagnostics.push(json!({
                        "reference": reference,
                        "code": "live_summary_missing",
                        "message": "KiCad returned the footprint item but not its summary"
                    }));
                    continue;
                };
                let source = match resolve_footprint_source(&summary.footprint, &board_for_ipc) {
                    Ok(source) => source,
                    Err(error) => {
                        diagnostics.push(json!({
                            "reference": reference,
                            "code": "library_resolution_failed",
                            "message": format!("{error:#}")
                        }));
                        continue;
                    }
                };
                let pads = match extract_pad_definitions(&source) {
                    Ok(pads) => pads,
                    Err(error) => {
                        diagnostics.push(json!({
                            "reference": reference,
                            "code": "library_pad_parse_failed",
                            "message": format!("{error:#}")
                        }));
                        continue;
                    }
                };
                let graphics = match extract_graphic_definitions(&source) {
                    Ok(graphics) => graphics,
                    Err(error) => {
                        diagnostics.push(json!({
                            "reference": reference,
                            "code": "library_graphic_parse_failed",
                            "message": format!("{error:#}")
                        }));
                        continue;
                    }
                };
                let expected_graphics = graphics
                    .iter()
                    .filter(|graphic| {
                        !matches!(graphic, copperpour_ipc::IpcGraphicDefinition::Text { .. })
                    })
                    .count();
                let restored_graphics = expected_graphics.saturating_sub(current_graphics);
                let library_pad_numbers = pads.iter().fold(BTreeMap::new(), |mut counts, pad| {
                    *counts.entry(pad.number.clone()).or_insert(0) += 1;
                    counts
                });
                if phantom_pads != restored_graphics || board_pad_numbers != library_pad_numbers {
                    diagnostics.push(json!({
                        "reference": reference,
                        "code": "signature_mismatch",
                        "message": format!(
                            "found {phantom_pads} anonymous layerless pads but the library is missing {restored_graphics} drawing shapes; legitimate board pads and library pads must also match exactly"
                        )
                    }));
                    continue;
                }

                let root = copperpour_sexp::parse_sexp(&source)
                    .context("invalid footprint S-expression")?;
                let pad_nodes = root.find_all("pad");
                let pad_properties =
                    super::library_footprint::pad_properties(&pad_nodes, pads.len())?;
                let pad_zones = super::library_footprint::pad_zones(&pad_nodes)?;
                let pad_margins = super::library_footprint::pad_margins(&root, &pad_nodes)?;
                let has_fab_props = pad_properties
                    .iter()
                    .any(|p| *p != super::library::PadFabricationProperty::None);
                let carry_10_0_7 = if has_fab_props {
                    let kicad = match &kicad_version {
                        Some(v) => v,
                        None => {
                            let v = client.get_kicad_version()?;
                            kicad_version.insert(v)
                        }
                    };
                    let carries = super::library_footprint::carries_kicad_10_0_7_fields(kicad);
                    if !carries {
                        for reason in super::library_footprint::not_carried_10_0_7_reasons(
                            &pad_properties,
                            &pads,
                            &[],
                            kicad,
                        ) {
                            let entry = format!("{reference} ({}): {reason}", summary.footprint);
                            if not_carried_seen.insert(entry.clone()) {
                                not_carried.push(entry);
                            }
                        }
                    }
                    carries
                } else {
                    false
                };
                for reason in super::library_footprint::unsupported_features(&root)? {
                    let entry = format!("{reference} ({}): {reason}", summary.footprint);
                    if not_carried_seen.insert(entry.clone()) {
                        not_carried.push(entry);
                    }
                }

                let mut clean = KiCadIpcClient::build_footprint_item(
                    &summary.footprint,
                    &summary.reference,
                    &summary.value,
                    &pads,
                    &graphics,
                    &extract_field_placement(&source),
                    summary.position.x,
                    summary.position.y,
                    summary.rotation,
                    &summary.layer,
                )?;
                let mut clean_fp = kiapi::board::types::FootprintInstance::decode(
                    clean.value.as_slice(),
                )?;
                if let Some(definition) = clean_fp.definition.as_mut() {
                    super::library_footprint::apply_pad_settings(
                        definition,
                        &pad_properties,
                        &pad_zones,
                        &pad_margins,
                        carry_10_0_7,
                    )?;
                }
                clean = copperpour_ipc::builders::pack_any(
                    &clean_fp,
                    "kiapi.board.types.FootprintInstance",
                );
                let Some(repaired_item) = merge_corrupted_footprint_candidate(
                    &reference,
                    &item,
                    &clean,
                    &mut diagnostics,
                ) else {
                    continue;
                };
                source_digests.push((reference.clone(), source.into_bytes()));
                repairs.push(CorruptedFootprintRepair {
                    reference,
                    footprint: summary.footprint.clone(),
                    phantom_pads,
                    restored_graphics,
                    expected_graphics,
                    repaired_item,
                });
            }

            if let Some(selected) = &selected {
                for reference in selected.difference(&seen) {
                    diagnostics.push(json!({
                        "reference": reference,
                        "code": "reference_not_found",
                        "message": "the requested footprint reference is not present on the open board"
                    }));
                }
            }
            // A repair sends the whole footprint back, which drops its groups
            // (#701). Read at the dry run and again at apply.
            if !repairs.is_empty() {
                let targets: Vec<&str> =
                    repairs.iter().map(|repair| repair.reference.as_str()).collect();
                for found in super::pcb_footprint_update::owned_group_reasons(
                    client,
                    &document,
                    "update",
                    Some(&targets),
                )? {
                    diagnostics.push(json!({
                        "reference": found.reference,
                        "code": "footprint_children_not_carried",
                        "message": found.reason,
                    }));
                }
            }
            repairs.sort_by(|left, right| left.reference.cmp(&right.reference));
            source_digests.sort_by(|left, right| left.0.cmp(&right.0));
            let plan_revision = repair_plan_revision(&board_for_ipc, &repairs, &source_digests);
            let candidates = repairs
                .iter()
                .map(|repair| json!({
                    "reference": repair.reference,
                    "footprint": repair.footprint,
                    "phantom_pads_removed": repair.phantom_pads,
                    "drawing_shapes_restored": repair.restored_graphics,
                    "expected_drawing_shapes": repair.expected_graphics
                }))
                .collect::<Vec<_>>();

            if !diagnostics.is_empty() {
                return Ok(json!({
                    "status": "conflict",
                    "plan_revision": plan_revision,
                    "candidate_count": repairs.len(),
                    "candidates": candidates,
                    "diagnostics": diagnostics,
                    "applied": false
                }));
            }
            if repairs.is_empty() {
                return Ok(json!({
                    "status": "noop",
                    "plan_revision": plan_revision,
                    "candidate_count": 0,
                    "candidates": [],
                    "diagnostics": [],
                    "applied": false
                }));
            }
            if dry_run {
                let mut response = json!({
                    "status": "ready",
                    "plan_revision": plan_revision,
                    "candidate_count": repairs.len(),
                    "candidates": candidates,
                    "diagnostics": [],
                    "applied": false
                });
                if !not_carried.is_empty() {
                    response["not_carried"] = json!(not_carried);
                }
                return Ok(response);
            }
            if expected_for_ipc.as_deref() != Some(plan_revision.as_str()) {
                return Ok(json!({
                    "status": "conflict",
                    "plan_revision": plan_revision,
                    "candidate_count": repairs.len(),
                    "candidates": candidates,
                    "diagnostics": [{
                        "code": "stale_plan_revision",
                        "message": "The live board or footprint library changed; rerun dry run and apply its new plan revision."
                    }],
                    "applied": false
                }));
            }

            let repaired_items = repairs
                .iter()
                .map(|repair| repair.repaired_item.clone())
                .collect::<Vec<_>>();
            client.run_commit("Repair legacy-corrupted footprint graphics", |client| {
                client.update_items_in(document.clone(), repaired_items)
            })?;

            let updated = client.get_items_in(
                document,
                kiapi::common::types::KiCadObjectType::KotPcbFootprint,
            )?;
            let expected = repairs
                .iter()
                .map(|repair| (repair.reference.as_str(), repair.expected_graphics))
                .collect::<HashMap<_, _>>();
            let mut verified = HashSet::new();
            for item in updated {
                let footprint = kiapi::board::types::FootprintInstance::decode(
                    item.value.as_slice(),
                )
                .context("KiCad returned an invalid repaired footprint")?;
                let reference = footprint_instance_reference(&footprint);
                let Some(expected_graphics) = expected.get(reference.as_str()) else {
                    continue;
                };
                let (phantom_pads, graphic_shapes, _) = issue_244_counts(&footprint)?;
                if phantom_pads != 0 || graphic_shapes != *expected_graphics {
                    anyhow::bail!(
                        "KiCad accepted the repair for {reference} but read-back found {phantom_pads} phantom pads and {graphic_shapes}/{expected_graphics} drawing shapes; use Ctrl-Z and inspect the footprint"
                    );
                }
                verified.insert(reference);
            }
            if verified.len() != repairs.len() {
                anyhow::bail!(
                    "KiCad accepted {} repairs but only {} repaired footprints were found on read-back; use Ctrl-Z and inspect the board",
                    repairs.len(),
                    verified.len()
                );
            }

            let mut response = json!({
                "status": "applied",
                "plan_revision": plan_revision,
                "candidate_count": repairs.len(),
                "repaired_count": repairs.len(),
                "candidates": candidates,
                "diagnostics": [],
                "applied": true,
                "undo": "Ctrl-Z reverses the complete repair."
            });
            if !not_carried.is_empty() {
                response["not_carried"] = json!(not_carried);
            }
            Ok(response)
        },
    )
    .await?;

    Ok(match outcome {
        BoardWrite::Ipc(value) => CallToolResult::json(&value),
        BoardWrite::Refused(result) => result,
        BoardWrite::File(reason) => CallToolResult::error_kind(
            crate::mcp::error::ToolErrorKind::EditorUnavailable {
                editor: "pcb".to_string(),
                reason: reason.premise().to_string(),
            },
            format!(
                "{} repair_corrupted_footprints is live-IPC-only and never edits the board file \
                 directly. Open the requested board in KiCad and retry.",
                reason.premise()
            ),
        ),
    })
}

async fn handle_find_component(
    args: &serde_json::Value,
    ctx: &ToolContext,
) -> anyhow::Result<CallToolResult> {
    let reference = match require_str(args, "reference") {
        Ok(v) => v.to_string(),
        Err(e) => return Ok(e),
    };
    let kiid = match optional_kiid(args) {
        Ok(kiid) => kiid,
        Err(e) => return Ok(e),
    };
    let lookup = ipc!(ctx, args, |c, document| c.find_footprint_in(
        document,
        &reference,
        kiid.as_deref()
    ));
    Ok(match lookup {
        copperpour_ipc::IpcFootprintLookupOutcome::Found(fp) => CallToolResult::json(&json!({
            "reference": fp.reference,
            "kiid": fp.kiid,
            "value": fp.value,
            "footprint": fp.footprint,
            "x": fp.position.x, "y": fp.position.y,
            "rotation": fp.rotation, "layer": fp.layer
        })),
        copperpour_ipc::IpcFootprintLookupOutcome::StaleTarget { target, reason } => {
            CallToolResult::error_kind(
                crate::mcp::error::ToolErrorKind::StaleTarget {
                    target,
                    reason: reason.clone(),
                },
                format!("find_component refused: {reason}."),
            )
        }
        copperpour_ipc::IpcFootprintLookupOutcome::AmbiguousTarget {
            target,
            candidates,
            reason,
        } => ambiguous_footprint_result(
            target,
            candidates,
            format!("find_component refused: {reason}."),
        ),
    })
}

/// How many pads the saved file gives `reference`, or `None` when the file
/// has no footprint by that name.
fn saved_pad_count(board_path: &std::path::Path, reference: &str) -> Option<usize> {
    let content = std::fs::read_to_string(board_path).ok()?;
    let tree = copperpour_sexp::parser::parse_sexp(&content).ok()?;
    tree.find_all("footprint")
        .into_iter()
        .find(|fp| {
            fp.find_all("property").iter().any(|p| {
                p.get(1).and_then(|n| n.as_str()) == Some("Reference")
                    && p.get(2).and_then(|n| n.as_str()) == Some(reference)
            })
        })
        .map(|fp| fp.find_all("pad").len())
}

pub(crate) fn file_pad_drill(
    pad: &copperpour_sexp::SexpNode,
) -> Option<copperpour_ipc::IpcPadDrill> {
    let drill = pad.find("drill")?;
    let oval = drill.get(1).and_then(copperpour_sexp::SexpNode::as_str) == Some("oval");
    let first = if oval { 2 } else { 1 };
    let x = drill.get_f64(first)?;
    let y = drill.get_f64(first + 1).unwrap_or(x);
    Some(copperpour_ipc::IpcPadDrill {
        shape: Some(if oval { "oval" } else { "circle" }.to_string()),
        size: Some(copperpour_ipc::IpcVector2 { x, y }),
        // The serialized pad drill does not independently name a layer span.
        // Do not turn the pad's wildcard/copper layer list into measured drill
        // endpoints; live IPC provides those when KiCad exposes them.
        start_layer: None,
        end_layer: None,
    })
}

/// Parse the geometry KiCad saved for one pad and express it in the same typed
/// shape returned by the live IPC path.
///
/// `x`, `y`, and `rotation_deg` are already in the caller's declared coordinate
/// space: board-space for `get_component_pads`, footprint-local for library
/// inspection.  Keeping this serializer shared prevents the two file readers
/// from drifting as new native pad fields are exposed.
pub(crate) fn pad_info_from_sexp(
    pad: &copperpour_sexp::SexpNode,
    x: f64,
    y: f64,
    rotation_deg: f64,
    net: String,
) -> anyhow::Result<copperpour_ipc::IpcPad> {
    let number = pad
        .get(1)
        .and_then(copperpour_sexp::SexpNode::as_str)
        .context("pad has no readable number")?
        .to_string();
    let pad_type = pad
        .get(2)
        .and_then(copperpour_sexp::SexpNode::as_str)
        .and_then(|kind| {
            match kind {
                "thru_hole" => Some("thru_hole"),
                "smd" => Some("smd"),
                "connect" => Some("edge_connector"),
                "np_thru_hole" => Some("np_thru_hole"),
                _ => None,
            }
            .map(str::to_string)
        });
    let shape = pad
        .get(3)
        .and_then(copperpour_sexp::SexpNode::as_str)
        .and_then(|shape| {
            matches!(
                shape,
                "circle"
                    | "rect"
                    | "oval"
                    | "trapezoid"
                    | "roundrect"
                    | "chamfered_rect"
                    | "custom"
            )
            .then(|| shape.to_string())
        });
    let size = pad.find("size").and_then(|size| {
        Some(copperpour_ipc::IpcVector2 {
            x: size.get_f64(1)?,
            y: size.get_f64(2)?,
        })
    });
    let layers: Vec<String> = pad
        .find("layers")
        .and_then(copperpour_sexp::SexpNode::children)
        .unwrap_or_default()
        .iter()
        .skip(1)
        .filter_map(copperpour_sexp::SexpNode::as_str)
        .map(str::to_string)
        .collect();
    let offset = pad.find("offset").and_then(|offset| {
        Some(copperpour_ipc::IpcVector2 {
            x: offset.get_f64(1)?,
            y: offset.get_f64(2)?,
        })
    });
    let corner_rounding_ratio = pad
        .find("roundrect_rratio")
        .and_then(|node| node.get_f64(1));
    let chamfer_ratio = pad.find("chamfer_ratio").and_then(|node| node.get_f64(1));
    let copper_layers = layers
        .iter()
        .filter(|layer| layer.as_str() == "*.Cu" || layer.ends_with(".Cu"))
        .map(|layer| copperpour_ipc::IpcPadLayerGeometry {
            layer: layer.clone(),
            shape: shape.clone(),
            size: size.clone(),
            offset: offset.clone(),
            corner_rounding_ratio,
            chamfer_ratio,
        })
        .collect();
    let uuid = pad
        .find_str("uuid")
        .filter(|uuid| !uuid.is_empty())
        .map(str::to_string);

    Ok(copperpour_ipc::IpcPad {
        uuid,
        number,
        x,
        y,
        net,
        layers,
        pad_type,
        rotation_deg: Some(rotation_deg),
        shape,
        size,
        drill: file_pad_drill(pad),
        copper_layers,
    })
}

async fn handle_get_component_pads(
    args: &serde_json::Value,
    ctx: &ToolContext,
) -> anyhow::Result<CallToolResult> {
    let board_path = get_path(args, "board")?;
    let source_selector = match crate::tools::board_source::BoardSource::from_args(args) {
        Ok(source) => source,
        Err(refusal) => return Ok(refusal),
    };
    let reference = match require_str(args, "reference") {
        Ok(v) => v.to_string(),
        Err(e) => return Ok(e),
    };

    // The board open in KiCad first: a part placed but not yet saved has no
    // pads in the file at all, so reading the file would either error or
    // answer about a stale board while the writers in this toolset act on the
    // live one. The file stays the fallback for an offline session.
    let ipc_reference = reference.clone();
    let observation = crate::tools::board_source::read_board(
        ctx,
        &board_path,
        source_selector,
        "component pads",
        move |c, document| c.get_footprint_pads_in(document, &ipc_reference),
    )
    .await?;
    let saved_why = match observation {
        crate::tools::board_source::BoardRead::Refused(refusal) => return Ok(refusal),
        // KiCad has the part and reports pads for it.
        crate::tools::board_source::BoardRead::Live(Some(pads)) if !pads.is_empty() => {
            let items: Vec<serde_json::Value> = pads
                .iter()
                .map(|pad| serde_json::to_value(pad).expect("IpcPad is serializable"))
                .collect();
            let mut body = json!({
                "reference": reference,
                "pad_count": items.len(),
                "pads": items,
                "source": "ipc"
            });
            crate::tools::board_source::provenance(
                &mut body,
                json!({ "pads": crate::tools::board_source::FROM_IPC }),
                crate::tools::board_source::live_evidence(),
            );
            return Ok(CallToolResult::json(&body));
        }
        // KiCad has the part and reports no pads. A pad-less footprint is
        // legal — a logo, a mounting graphic — so this is not wrong by
        // itself. But "no pads" is also what an unread response shape would
        // look like, and it reads as a plausible answer rather than a
        // failure, so it is refused whenever the saved file disagrees.
        //
        // That leaves one deliberate false positive: delete a footprint's pads
        // in KiCad without saving and the file still has them, so a correct
        // "no pads" is refused until the next save. Preferring that to
        // silently reporting an unread response as an empty pad list is the
        // whole point — and the message says which key to press.
        crate::tools::board_source::BoardRead::Live(Some(_)) => {
            if saved_pad_count(&board_path, &reference).is_some_and(|count| count > 0) {
                return Ok(CallToolResult::error_kind(
                    crate::mcp::error::ToolErrorKind::Conflict {
                        paths: vec![board_path.display().to_string()],
                    },
                    format!(
                        "KiCad reports no pads for footprint '{reference}' while the saved file \
                         has some. Refusing to answer 'no pads' — save the board in KiCad and \
                         retry, and report this if it persists."
                    ),
                ));
            }
            let mut body = json!({
                "reference": reference,
                "pad_count": 0,
                "pads": [],
                "source": "ipc"
            });
            crate::tools::board_source::provenance(
                &mut body,
                json!({ "pads": crate::tools::board_source::FROM_IPC }),
                crate::tools::board_source::live_evidence(),
            );
            return Ok(CallToolResult::json(&body));
        }
        // KiCad holds this board and does not have the part: the file may
        // still carry a footprint the user has deleted, so answering from it
        // would be answering about a board that no longer exists.
        crate::tools::board_source::BoardRead::Live(None) => {
            let message = format!("Footprint '{reference}' not found on the board open in KiCad");
            return Ok(CallToolResult::error_kind(
                crate::mcp::error::ToolErrorKind::InvalidArgument {
                    field: "reference".to_string(),
                    reason: message.clone(),
                },
                message,
            ));
        }
        crate::tools::board_source::BoardRead::Saved(why) => why,
    };

    let content = std::fs::read_to_string(&board_path)?;
    let tree = copperpour_sexp::parser::parse_sexp(&content)?;

    // Find the footprint with matching reference
    let fp_node = tree.find_all("footprint").into_iter().find(|fp| {
        fp.find_all("property").iter().any(|p| {
            p.get(1).and_then(|n| n.as_str()) == Some("Reference")
                && p.get(2).and_then(|n| n.as_str()) == Some(reference.as_str())
        })
    });

    let fp_node = match fp_node {
        Some(n) => n,
        None => {
            let message = format!("Footprint '{}' not found", reference);
            return Ok(CallToolResult::error_kind(
                crate::mcp::error::ToolErrorKind::InvalidArgument {
                    field: "reference".to_string(),
                    reason: message.clone(),
                },
                message,
            ));
        }
    };

    let fp_at = fp_node.find("at");
    let fp_x = fp_at.and_then(|a| a.get_f64(1)).unwrap_or(0.0);
    let fp_y = fp_at.and_then(|a| a.get_f64(2)).unwrap_or(0.0);
    let fp_rot = fp_at.and_then(|a| a.get_f64(3)).unwrap_or(0.0);

    let mut pads = Vec::new();
    for pad in fp_node.find_all("pad") {
        let pad_number = pad
            .get(1)
            .and_then(copperpour_sexp::SexpNode::as_str)
            .unwrap_or("<unreadable>");
        let Some(pad_at) = pad.find("at") else {
            let message =
                format!("Pad '{pad_number}' on footprint '{reference}' has no readable position");
            return Ok(CallToolResult::error_kind(
                crate::mcp::error::ToolErrorKind::MalformedFile {
                    path: board_path.display().to_string(),
                    reason: message.clone(),
                },
                message,
            ));
        };
        let (Some(local_x), Some(local_y)) = (pad_at.get_f64(1), pad_at.get_f64(2)) else {
            let message =
                format!("Pad '{pad_number}' on footprint '{reference}' has an invalid position");
            return Ok(CallToolResult::error_kind(
                crate::mcp::error::ToolErrorKind::MalformedFile {
                    path: board_path.display().to_string(),
                    reason: message.clone(),
                },
                message,
            ));
        };
        // Transform local pad coords to board space (rotation only).
        // Uses the canonical KiCAD transform — see copperpour_sexp::geometry.
        let (board_x, board_y) =
            copperpour_sexp::geometry::transform_pad(local_x, local_y, fp_x, fp_y, fp_rot);
        // Three outcomes, deliberately distinguishable. No (net …) node at
        // all is an unconnected pad, and "" says so. A node we can read
        // gives its name. A node that is present but unreadable gives
        // null — previously it gave "" too, so a fully connected KiCad 10
        // pad was indistinguishable from an unconnected one. See
        // copperpour_sexp::net for the two shapes.
        let net = match pad.find("net") {
            None => json!(""),
            Some(node) => match copperpour_sexp::net::net_name(node) {
                Some(name) => json!(name),
                None => serde_json::Value::Null,
            },
        };
        // In a saved board KiCad writes the pad's effective board-space
        // angle here. Unlike a library footprint, adding the footprint
        // angle again would double-rotate the reported geometry.
        let rotation_deg = pad_at.get_f64(3).unwrap_or(0.0);
        let pad_info = match pad_info_from_sexp(
            pad,
            board_x,
            board_y,
            rotation_deg,
            net.as_str().unwrap_or_default().to_string(),
        ) {
            Ok(info) => info,
            Err(error) => {
                let message = format!(
                    "Could not read pad '{pad_number}' on footprint '{reference}': {error}"
                );
                return Ok(CallToolResult::error_kind(
                    crate::mcp::error::ToolErrorKind::MalformedFile {
                        path: board_path.display().to_string(),
                        reason: message.clone(),
                    },
                    message,
                ));
            }
        };
        let mut value = serde_json::to_value(pad_info)?;
        // Preserve the established three-state net contract.  The shared
        // typed pad uses a string for known/unconnected values; malformed
        // saved net nodes remain an explicit JSON null here.
        value["net"] = net;
        pads.push(value);
    }

    let mut body = json!({
        "reference": reference,
        "pad_count": pads.len(),
        "pads": pads,
        "source": "file"
    });
    crate::tools::board_source::provenance(
        &mut body,
        json!({ "pads": crate::tools::board_source::FROM_SAVED_BOARD }),
        saved_why.evidence(),
    );
    Ok(CallToolResult::json(&body))
}

async fn handle_get_pad_position(
    args: &serde_json::Value,
    ctx: &ToolContext,
) -> anyhow::Result<CallToolResult> {
    let pad_number = match require_str(args, "pad_number") {
        Ok(v) => v.to_string(),
        Err(e) => return Ok(e),
    };
    let pads_result = handle_get_component_pads(args, ctx).await?;
    if pads_result.is_error {
        return Ok(pads_result);
    }
    // Parse the result and filter for the specific pad number
    if let Some(crate::mcp::protocol::ToolContent::Text { text }) = pads_result.content.first() {
        if let Ok(parsed) = serde_json::from_str::<serde_json::Value>(text) {
            if let Some(pads) = parsed["pads"].as_array() {
                if let Some(pad) = pads
                    .iter()
                    .find(|p| p["number"].as_str() == Some(&pad_number))
                {
                    // Carry the pad list's source and provenance, so a caller
                    // measuring one pad can still tell whether it measured the
                    // live board and why any saved fallback occurred.
                    let mut pad = pad.clone();
                    if let Some(object) = pad.as_object_mut() {
                        if let Some(source) = parsed.get("source") {
                            object.insert("source".to_string(), source.clone());
                        }
                        if let Some(pad_source) = parsed.pointer("/sources/pads") {
                            object.insert("sources".to_string(), json!({ "pad": pad_source }));
                        }
                        if let Some(evidence) = parsed.get("source_evidence") {
                            object.insert("source_evidence".to_string(), evidence.clone());
                        }
                    }
                    return Ok(CallToolResult::json(&pad));
                }
            }
        }
    }
    let message = format!("Pad '{}' not found", pad_number);
    Ok(CallToolResult::error_kind(
        crate::mcp::error::ToolErrorKind::InvalidArgument {
            field: "pad_number".to_string(),
            reason: message.clone(),
        },
        message,
    ))
}

async fn handle_get_component_list(
    _args: &serde_json::Value,
    ctx: &ToolContext,
) -> anyhow::Result<CallToolResult> {
    let fps = ipc!(ctx, _args, |c| c.list_footprints());
    let items: Vec<serde_json::Value> = fps
        .iter()
        .map(|fp| {
            json!({
                "reference": fp.reference,
                "value": fp.value,
                "footprint": fp.footprint,
                "x": fp.position.x, "y": fp.position.y,
                "rotation": fp.rotation, "layer": fp.layer
            })
        })
        .collect();
    Ok(CallToolResult::json(
        &json!({ "count": items.len(), "components": items }),
    ))
}

async fn handle_place_array(
    args: &serde_json::Value,
    ctx: &ToolContext,
) -> anyhow::Result<CallToolResult> {
    let board = get_path(args, "board")?;
    let footprint = match require_str(args, "footprint") {
        Ok(v) => v.to_string(),
        Err(e) => return Ok(e),
    };
    let start_x = match require_f64(args, "start_x") {
        Ok(v) => v,
        Err(e) => return Ok(e),
    };
    let start_y = match require_f64(args, "start_y") {
        Ok(v) => v,
        Err(e) => return Ok(e),
    };
    // `count_x` is schema-required and defaulted to 1, so a caller who meant a
    // 10x10 grid and mistyped the key got a single column of real footprints
    // committed to the live board, and `{"placed_count": N}` back. The zero
    // guard below could never fire on that path either — 1 is not 0 — so it
    // only ever caught an explicit zero, which it still does (#218).
    let count_x = match require_u64(args, "count_x") {
        Ok(v) => v,
        Err(e) => return Ok(e),
    };
    let count_y = args["count_y"].as_u64().unwrap_or(1);
    let Some(total_count) = count_x.checked_mul(count_y) else {
        let message = "Array dimensions overflow.".to_string();
        return Ok(CallToolResult::error_kind(
            crate::mcp::error::ToolErrorKind::InvalidArgument {
                field: "count_x".to_string(),
                reason: message.clone(),
            },
            message,
        ));
    };
    if count_x == 0 || count_y == 0 || total_count > 10_000 {
        let field = if count_x == 0 {
            "count_x"
        } else if count_y == 0 {
            "count_y"
        } else {
            "count_x"
        };
        let message =
            "Array dimensions must be non-zero and contain at most 10,000 components.".to_string();
        return Ok(CallToolResult::error_kind(
            crate::mcp::error::ToolErrorKind::InvalidArgument {
                field: field.to_string(),
                reason: message.clone(),
            },
            message,
        ));
    }
    let count_x = count_x as usize;
    let count_y = count_y as usize;
    let spacing_x = match require_f64(args, "spacing_x") {
        Ok(v) => v,
        Err(e) => return Ok(e),
    };
    let spacing_y = args["spacing_y"].as_f64().unwrap_or(spacing_x);
    let prefix = args["ref_prefix"].as_str().unwrap_or("U").to_string();
    let ref_start = args["ref_start"].as_u64().unwrap_or(1);
    if ref_start.checked_add(total_count - 1).is_none() {
        let message = "Reference number overflow.".to_string();
        return Ok(CallToolResult::error_kind(
            crate::mcp::error::ToolErrorKind::InvalidArgument {
                field: "ref_start".to_string(),
                reason: message.clone(),
            },
            message,
        ));
    }
    let (library_path, source) =
        match read_footprint_source_or_error(&footprint, &board, Some("footprint")) {
            Ok(loaded) => loaded,
            Err(refusal) => return Ok(refusal),
        };
    let mut planned = Vec::with_capacity(total_count as usize);
    for row in 0..count_y {
        for col in 0..count_x {
            let x = start_x + col as f64 * spacing_x;
            let y = start_y + row as f64 * spacing_y;
            let reference = format!("{prefix}{}", ref_start + planned.len() as u64);
            let prepared = match PreparedFootprintPlacement::from_library_for_ipc(
                &source, &footprint, &reference, None, x, y, 0.0, "F.Cu",
            ) {
                Ok(prepared) => prepared,
                Err(error) => {
                    let message = error.to_string();
                    return Ok(CallToolResult::error_kind(
                        crate::mcp::error::ToolErrorKind::MalformedFile {
                            path: library_path.display().to_string(),
                            reason: message.clone(),
                        },
                        message,
                    ));
                }
            };
            planned.push((reference, prepared, x, y));
        }
    }

    let footprint_id = footprint.clone();
    let (placed, not_carried) = match with_board_ipc_classified(ctx, &board, move |c, _| {
        let existing = c
            .list_footprints()?
            .into_iter()
            .map(|footprint| footprint.reference)
            .collect::<HashSet<_>>();
        let conflicts = planned
            .iter()
            .filter(|(reference, _, _, _)| existing.contains(reference))
            .map(|(reference, _, _, _)| reference.as_str())
            .collect::<Vec<_>>();
        if !conflicts.is_empty() {
            let message = format!(
                "footprint references already exist on the board: {}",
                conflicts.join(", ")
            );
            return Ok(Err(CallToolResult::error_kind(
                crate::mcp::error::ToolErrorKind::InvalidArgument {
                    field: "ref_start".to_string(),
                    reason: message.clone(),
                },
                message,
            )));
        }

        let (carried, older_kicad) = match planned.first() {
            Some((_, first, _, _)) => first.resolve_kicad_carried(c)?,
            None => (Default::default(), None),
        };
        let not_carried = planned
            .first()
            .map(|(_, first, _, _)| first.not_carried(older_kicad.as_ref()))
            .unwrap_or_default();

        let items = planned
            .iter()
            .map(|(reference, prepared, x, y)| {
                prepared
                    .ipc_item(&footprint_id, reference, *x, *y, carried)
                    .with_context(|| format!("failed to prepare {reference}"))
            })
            .collect::<anyhow::Result<Vec<_>>>()?;
        c.run_commit("Place footprint array", |c| c.create_items(items))?;

        let mut created = c
            .list_footprints()?
            .into_iter()
            .map(|footprint| (footprint.reference.clone(), footprint))
            .collect::<HashMap<_, _>>();
        let components = planned
            .into_iter()
            .map(|(reference, _, _, _)| {
                let footprint = created.remove(&reference).with_context(|| {
                    format!("committed footprint '{reference}' was not found on the board")
                })?;
                Ok(json!({
                    "reference": reference,
                    "x": footprint.position.x,
                    "y": footprint.position.y
                }))
            })
            .collect::<anyhow::Result<Vec<_>>>()?;
        Ok(Ok((components, not_carried)))
    })
    .await?
    {
        Ok(Ok(result)) => result,
        Ok(Err(refusal)) => return Ok(refusal),
        Err(failure) => return Ok(crate::tools::ipc_failure_result(failure)),
    };
    let mut response = json!({ "placed_count": placed.len(), "components": placed });
    if !not_carried.is_empty() {
        response["not_carried"] = json!(not_carried);
    }
    Ok(CallToolResult::json(&response))
}

async fn handle_align_components(
    args: &serde_json::Value,
    ctx: &ToolContext,
) -> anyhow::Result<CallToolResult> {
    let board = get_path(args, "board")?;
    let refs = match args["references"].as_array() {
        Some(references) if !references.is_empty() => references,
        _ => {
            let message = "'references' must be a non-empty array.".to_string();
            return Ok(CallToolResult::error_kind(
                crate::mcp::error::ToolErrorKind::InvalidArgument {
                    field: "references".to_string(),
                    reason: message.clone(),
                },
                message,
            ));
        }
    };
    let references = match refs
        .iter()
        .map(|reference| reference.as_str().map(String::from))
        .collect::<Option<Vec<_>>>()
    {
        Some(references) => references,
        None => {
            let message = "Every reference must be a string.".to_string();
            return Ok(CallToolResult::error_kind(
                crate::mcp::error::ToolErrorKind::InvalidArgument {
                    field: "references".to_string(),
                    reason: message.clone(),
                },
                message,
            ));
        }
    };
    let axis = args["axis"].as_str().unwrap_or("x").to_string();
    if axis != "x" && axis != "y" {
        let message = "'axis' must be either 'x' or 'y'.".to_string();
        return Ok(CallToolResult::error_kind(
            crate::mcp::error::ToolErrorKind::InvalidArgument {
                field: "axis".to_string(),
                reason: message.clone(),
            },
            message,
        ));
    }
    let value = match require_f64(args, "value") {
        Ok(v) => v,
        Err(e) => return Ok(e),
    };

    let named = references.clone();
    let aligned = match with_board_ipc_classified(ctx, &board, move |c, document| {
        let targets: Vec<&str> = references.iter().map(String::as_str).collect();
        if let Some(refused) = super::pcb_footprint_update::refuse_owned_groups(
            c,
            &document,
            "align_components",
            "move",
            Some(&targets),
        )? {
            return Ok(Err(refused));
        }
        c.run_commit("Align footprints", |c| {
            references
                .iter()
                .map(|reference| {
                    let footprint = c.get_footprint(reference)?.ok_or_else(|| {
                        copperpour_ipc::PreflightRefusal::new(
                            copperpour_ipc::PreflightCause::MissingReferences {
                                references: vec![reference.clone()],
                            },
                            format!("footprint '{reference}' not found"),
                        )
                    })?;
                    let (x, y) = if axis == "y" {
                        (footprint.position.x, value)
                    } else {
                        (value, footprint.position.y)
                    };
                    c.move_footprint(reference, x, y)?;
                    Ok(json!({ "reference": reference, "x": x, "y": y }))
                })
                .collect::<anyhow::Result<Vec<_>>>()
        })
        .map(Ok)
    })
    .await?
    {
        Ok(Ok(aligned)) => aligned,
        Ok(Err(refused)) => return Ok(refused),
        // Copperpour's own refusal, inside the commit it then dropped (#240).
        Err(copperpour_ipc::IpcFailure::Preflight(refusal)) => {
            let kind = crate::tools::pcb_board::preflight_refusal_kind(
                "align_components",
                &refusal,
                |reference, _| match named.iter().position(|named| named == reference) {
                    Some(index) => format!("references[{index}]"),
                    None => "references".to_string(),
                },
            );
            return Ok(crate::tools::pcb_board::preflight_refusal_result(
                "alignment",
                &refusal,
                Some(kind),
            ));
        }
        Err(failure) => return Ok(crate::tools::ipc_failure_result(failure)),
    };
    Ok(CallToolResult::json(
        &json!({ "aligned_count": aligned.len(), "components": aligned }),
    ))
}

async fn handle_duplicate_component(
    args: &serde_json::Value,
    ctx: &ToolContext,
) -> anyhow::Result<CallToolResult> {
    let board = get_path(args, "board")?;
    let reference = match require_str(args, "reference") {
        Ok(v) => v.to_string(),
        Err(e) => return Ok(e),
    };
    let new_reference = match require_str(args, "new_reference") {
        Ok(v) => v.to_string(),
        Err(e) => return Ok(e),
    };
    let x = match require_f64(args, "x") {
        Ok(v) => v,
        Err(e) => return Ok(e),
    };
    let y = match require_f64(args, "y") {
        Ok(v) => v,
        Err(e) => return Ok(e),
    };

    // Get the source footprint's footprint ID and rotation
    let ref_ipc = reference.clone();
    let src = ipc!(ctx, args, |c| {
        c.get_footprint(&ref_ipc)?
            .ok_or_else(|| copperpour_ipc::PreflightRefusal::missing_footprint(&ref_ipc).into())
    });
    if let Some(rejection) = back_side_layer_error(&src.layer) {
        return Ok(rejection);
    }
    let (library_path, source) = match read_footprint_source_or_error(&src.footprint, &board, None)
    {
        Ok(loaded) => loaded,
        Err(refusal) => return Ok(refusal),
    };
    let prepared = match PreparedFootprintPlacement::from_library_for_ipc(
        &source,
        &src.footprint,
        &new_reference,
        Some(&src.value),
        x,
        y,
        src.rotation,
        &src.layer,
    ) {
        Ok(prepared) => prepared,
        Err(error) => {
            let message = error.to_string();
            return Ok(CallToolResult::error_kind(
                crate::mcp::error::ToolErrorKind::MalformedFile {
                    path: library_path.display().to_string(),
                    reason: message.clone(),
                },
                message,
            ));
        }
    };
    let ipc_reference = new_reference.clone();
    let fp_id = src.footprint.clone();
    let dup_board = board.clone();
    let (fp, not_carried) = ipc!(ctx, args, |c| {
        let (carried, older_kicad) = prepared.resolve_kicad_carried(c)?;
        let item = prepared.ipc_item(&fp_id, &ipc_reference, x, y, carried)?;
        let fp = c.place_footprint_item(&dup_board, &ipc_reference, item)?;
        let not_carried = prepared.not_carried(older_kicad.as_ref());
        Ok((fp, not_carried))
    });
    let mut response = json!({
        "duplicated_from": reference,
        "new_reference": fp.reference,
        "x": fp.position.x, "y": fp.position.y
    });
    if !not_carried.is_empty() {
        response["not_carried"] = json!(not_carried);
    }
    Ok(CallToolResult::json(&response))
}

async fn handle_get_board_2d_view(
    args: &serde_json::Value,
    ctx: &ToolContext,
) -> anyhow::Result<CallToolResult> {
    use base64::Engine;
    let board_path = get_path(args, "board")?;
    let width = args["width"].as_u64().unwrap_or(800).clamp(100, 4000) as u32;
    let height = args["height"].as_u64().unwrap_or(600).clamp(100, 4000) as u32;

    let tmp = board_path.with_extension("render.png");
    super::cli::render_pcb_png(&ctx.config.kicad_cli, &board_path, &tmp, width, height).await?;
    let bytes = tokio::fs::read(&tmp).await?;
    let _ = tokio::fs::remove_file(&tmp).await;

    let b64 = base64::engine::general_purpose::STANDARD.encode(&bytes);
    let mut metadata = json!({
        "board": board_path.to_string_lossy(),
        "width": width,
        "height": height,
    });
    super::board_source::provenance(
        &mut metadata,
        json!({
            "board": super::board_source::FROM_SAVED_BOARD,
            "render": super::board_source::FROM_KICAD_CLI,
        }),
        super::board_source::saved_snapshot_evidence(ctx, &board_path),
    );
    let mut result = CallToolResult::json(&metadata);
    result
        .content
        .extend(CallToolResult::image(b64, "image/png").content);
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn two_layers() -> CopperStack {
        CopperStack::new(2).unwrap()
    }

    const FOOTPRINT: &str = r#"(footprint "R_0402"
  (version 20240108)
  (generator pcbnew)
  (layer "F.Cu")
  (property "Reference" "REF**" (at 0 -1 0) (layer "F.SilkS"))
  (property "Value" "R_0402" (at 0 1 0) (layer "F.Fab"))
  (pad "1" smd roundrect (at -0.5 0) (size 0.5 0.5)
    (layers "F.Cu" "F.Paste" "F.Mask")))"#;

    fn test_pad(number: &str, layers: Vec<i32>, id: &str, net: Option<&str>) -> prost_types::Any {
        use copperpour_ipc::gen::kiapi;
        let pad = kiapi::board::types::Pad {
            id: Some(kiapi::common::types::Kiid {
                value: id.to_string(),
            }),
            number: number.to_string(),
            net: net.map(|name| kiapi::board::types::Net {
                code: None,
                name: name.to_string(),
            }),
            pad_stack: Some(kiapi::board::types::PadStack {
                layers,
                ..Default::default()
            }),
            ..Default::default()
        };
        copperpour_ipc::builders::pack_any(&pad, "kiapi.board.types.Pad")
    }

    fn normalised_issue_244_pad() -> prost_types::Any {
        use copperpour_ipc::gen::kiapi;
        let mut pad = kiapi::board::types::Pad::decode(
            test_pad("", vec![0], "normalised-phantom", None)
                .value
                .as_slice(),
        )
        .unwrap();
        pad.r#type = kiapi::board::types::PadType::PtPth as i32;
        pad.pad_stack.as_mut().unwrap().drill = Some(kiapi::board::types::DrillProperties {
            diameter: Some(copperpour_ipc::builders::vec2(0.000_001, 0.000_001)),
            ..Default::default()
        });
        copperpour_ipc::builders::pack_any(&pad, "kiapi.board.types.Pad")
    }

    #[test]
    fn issue_244_signature_distinguishes_real_unnumbered_pads() {
        use copperpour_ipc::gen::kiapi;
        let footprint = kiapi::board::types::FootprintInstance {
            definition: Some(kiapi::board::types::Footprint {
                items: vec![
                    test_pad("1", vec![0], "named", None),
                    // A real NPTH/mechanical pad may be unnumbered, but it has
                    // an explicit layer set and must survive the repair.
                    test_pad("", vec![0, 31], "mechanical", None),
                    // #244's proto3 default pad has neither a number nor any
                    // layers. This exact signature is the only one repaired.
                    test_pad("", vec![], "phantom", None),
                    // A save/reload can normalise that same proto default into
                    // *.Cu with a one-nanometre PTH drill.
                    normalised_issue_244_pad(),
                    copperpour_ipc::builders::pack_any(
                        &kiapi::board::types::BoardGraphicShape::default(),
                        "kiapi.board.types.BoardGraphicShape",
                    ),
                ],
                ..Default::default()
            }),
            ..Default::default()
        };

        let (phantoms, graphics, pads) = issue_244_counts(&footprint).unwrap();
        assert_eq!(phantoms, 2);
        assert_eq!(graphics, 1);
        assert_eq!(pads.get("1"), Some(&1));
        assert_eq!(pads.get(""), Some(&1));
    }

    #[test]
    fn repair_restores_graphics_and_preserves_live_pad_identity_and_net() {
        use copperpour_ipc::gen::kiapi;

        let current = kiapi::board::types::FootprintInstance {
            id: Some(kiapi::common::types::Kiid {
                value: "placed-footprint".to_string(),
            }),
            symbol_path: Some(kiapi::common::types::SheetPath {
                path: vec![kiapi::common::types::Kiid {
                    value: "symbol-path".to_string(),
                }],
                path_human_readable: String::new(),
            }),
            definition: Some(kiapi::board::types::Footprint {
                items: vec![
                    test_pad("1", vec![0], "live-pad", Some("GND")),
                    test_pad("", vec![], "phantom", None),
                    copperpour_ipc::builders::pack_any(
                        &kiapi::board::types::BoardText {
                            id: Some(kiapi::common::types::Kiid {
                                value: "custom-board-text".to_string(),
                            }),
                            ..Default::default()
                        },
                        "kiapi.board.types.BoardText",
                    ),
                ],
                ..Default::default()
            }),
            ..Default::default()
        };
        let clean = kiapi::board::types::FootprintInstance {
            definition: Some(kiapi::board::types::Footprint {
                items: vec![
                    test_pad("1", vec![0], "library-pad", None),
                    copperpour_ipc::builders::pack_any(
                        &kiapi::board::types::BoardGraphicShape::default(),
                        "kiapi.board.types.BoardGraphicShape",
                    ),
                    copperpour_ipc::builders::pack_any(
                        &kiapi::board::types::BoardText {
                            id: Some(kiapi::common::types::Kiid {
                                value: "library-text".to_string(),
                            }),
                            ..Default::default()
                        },
                        "kiapi.board.types.BoardText",
                    ),
                ],
                ..Default::default()
            }),
            ..Default::default()
        };
        let current =
            copperpour_ipc::builders::pack_any(&current, "kiapi.board.types.FootprintInstance");
        let clean =
            copperpour_ipc::builders::pack_any(&clean, "kiapi.board.types.FootprintInstance");

        let repaired = merge_clean_footprint_children(&current, &clean).unwrap();
        let repaired =
            kiapi::board::types::FootprintInstance::decode(repaired.value.as_slice()).unwrap();
        assert_eq!(repaired.id.as_ref().unwrap().value, "placed-footprint");
        assert_eq!(
            repaired.symbol_path.as_ref().unwrap().path[0].value,
            "symbol-path"
        );
        let (phantoms, graphics, pads) = issue_244_counts(&repaired).unwrap();
        assert_eq!((phantoms, graphics), (0, 1));
        assert_eq!(pads.get("1"), Some(&1));

        let pad = repaired
            .definition
            .as_ref()
            .unwrap()
            .items
            .iter()
            .find(|item| copperpour_ipc::builders::any_is(item, "kiapi.board.types.Pad"))
            .map(|item| kiapi::board::types::Pad::decode(item.value.as_slice()).unwrap())
            .unwrap();
        assert_eq!(pad.id.as_ref().unwrap().value, "live-pad");
        assert_eq!(pad.net.as_ref().unwrap().name, "GND");
        let texts = repaired
            .definition
            .as_ref()
            .unwrap()
            .items
            .iter()
            .filter(|item| copperpour_ipc::builders::any_is(item, "kiapi.board.types.BoardText"))
            .map(|item| {
                kiapi::board::types::BoardText::decode(item.value.as_slice())
                    .unwrap()
                    .id
                    .unwrap()
                    .value
            })
            .collect::<Vec<_>>();
        assert_eq!(texts, ["custom-board-text"]);
    }

    #[test]
    fn repair_merge_failure_is_scoped_to_one_footprint() {
        use copperpour_ipc::gen::kiapi;

        let valid = kiapi::board::types::FootprintInstance {
            definition: Some(kiapi::board::types::Footprint::default()),
            ..Default::default()
        };
        let valid =
            copperpour_ipc::builders::pack_any(&valid, "kiapi.board.types.FootprintInstance");
        let invalid = prost_types::Any {
            type_url: "type.googleapis.com/kiapi.board.types.FootprintInstance".to_string(),
            value: vec![0xff],
        };
        let mut diagnostics = Vec::new();
        let mut repaired = Vec::new();

        for (reference, clean) in [("R_BAD", &invalid), ("R_GOOD", &valid)] {
            if merge_corrupted_footprint_candidate(reference, &valid, clean, &mut diagnostics)
                .is_some()
            {
                repaired.push(reference);
            }
        }

        assert_eq!(repaired, ["R_GOOD"]);
        assert_eq!(diagnostics.len(), 1);
        assert_eq!(diagnostics[0]["reference"], "R_BAD");
        assert_eq!(diagnostics[0]["code"], "repair_merge_failed");
        assert!(diagnostics[0]["message"]
            .as_str()
            .unwrap()
            .contains("invalid clean footprint"));
    }

    #[test]
    fn prepares_complete_front_footprint() {
        let prepared = prepare_footprint_source(
            FOOTPRINT,
            "Resistor_SMD:R_0402",
            "R17",
            None,
            12.5,
            8.25,
            90.0,
            "F.Cu",
        )
        .unwrap();
        assert!(prepared.starts_with("(footprint \"Resistor_SMD:R_0402\""));
        assert!(prepared.contains("(property \"Reference\" \"R17\""));
        assert!(prepared.contains("(at 12.5 8.25 90)"));
        assert!(prepared.contains("(pad \"1\""));
        assert!(prepared.contains("(layers \"F.Cu\" \"F.Paste\" \"F.Mask\")"));
        let pads = extract_pad_definitions(&prepared).unwrap();
        assert_eq!(pads.len(), 1);
        assert_eq!(pads[0].number, "1");
        assert_eq!(pads[0].shape, "roundrect");
        assert_eq!(pads[0].layers, ["F.Cu", "F.Paste", "F.Mask"]);
    }

    #[test]
    fn back_side_placement_is_rejected_not_string_swapped() {
        // The old implementation did a blind "F. → "B. text swap over the whole
        // footprint, which corrupted property values starting with "F." and
        // left pad X positions unmirrored — wrong geometry presented as
        // success. Until a real mirror flip exists, B.Cu must be refused.
        let error = prepare_footprint_source(
            FOOTPRINT,
            "Resistor_SMD:R_0402",
            "R18",
            Some("10k"),
            1.0,
            2.0,
            0.0,
            "B.Cu",
        )
        .unwrap_err();
        assert!(error.to_string().contains("not yet supported"), "{error}");
    }

    #[test]
    fn rejects_non_outer_copper_layer() {
        let error = prepare_footprint_source(
            FOOTPRINT,
            "Resistor_SMD:R_0402",
            "R19",
            None,
            0.0,
            0.0,
            0.0,
            "In1.Cu",
        )
        .unwrap_err();
        assert!(error.to_string().contains("F.Cu"));
    }

    /// A footprint with the graphics KiCad's own libraries ship: courtyard
    /// rect, silkscreen lines, a fab outline and text, plus hidden built-in
    /// properties that must not be drawn.
    const GRAPHIC_FOOTPRINT: &str = r#"(footprint "R_0402"
  (version 20240108)
  (generator pcbnew)
  (layer "F.Cu")
  (property "Reference" "REF**" (at 0 -1 0) (layer "F.SilkS"))
  (property "Value" "R_0402" (at 0 1 0) (layer "F.Fab"))
  (property "Datasheet" "" (at 0 0 0) (layer "F.Fab") (hide yes))
  (fp_line (start -0.6 -0.5) (end 0.6 -0.5) (stroke (width 0.12) (type solid)) (layer "F.SilkS"))
  (fp_line (start -0.6 0.5) (end 0.6 0.5) (stroke (width 0.12) (type solid)) (layer "F.SilkS"))
  (fp_rect (start -0.8 -0.7) (end 0.8 0.7) (stroke (width 0.05) (type default)) (fill no) (layer "F.CrtYd"))
  (fp_circle (center 0 0) (end 0.25 0) (stroke (width 0.1) (type solid)) (fill yes) (layer "F.Fab"))
  (fp_arc (start -0.3 0) (mid 0 -0.3) (end 0.3 0) (stroke (width 0.12) (type solid)) (layer "F.SilkS"))
  (fp_poly (pts (xy -0.2 -0.2) (xy 0.2 -0.2) (xy 0.2 0.2)) (stroke (width 0.1) (type solid)) (fill yes) (layer "F.Fab"))
  (fp_text user "${REFERENCE}" (at 0 1.17 0) (layer "F.Fab") (effects (font (size 0.26 0.26) (thickness 0.04))))
  (fp_text user "secret" (at 0 0 0) (layer "F.Fab") (hide yes) (effects (font (size 0.26 0.26))))
  (pad "1" smd roundrect (at -0.5 0) (size 0.5 0.5)
    (layers "F.Cu" "F.Paste" "F.Mask")))"#;

    /// Unmodified `Connector_Card:SD_Card_Device_16mm_SlotDepth` from KiCad
    /// 10.0.6's library. Its file draws 20 shapes `(type default)` (14 fp_line,
    /// 5 fp_arc, 1 fp_rect), one `(type solid)` fp_poly and one `(type dash)`
    /// fp_line, and its three user texts are `(unlocked yes)`.
    #[test]
    fn stock_footprint_strokes_and_unlocked_text_are_read_as_kicad_saved_them() {
        use copperpour_ipc::{IpcGraphicDefinition as Graphic, IpcLineStyle};
        let source = include_str!("../../tests/fixtures/sd_card_device_kicad10.kicad_mod");
        let graphics = extract_graphic_definitions(source).unwrap();

        let count = |style| {
            graphics
                .iter()
                .filter(|graphic| match graphic {
                    Graphic::Line { line_style, .. }
                    | Graphic::Rect { line_style, .. }
                    | Graphic::Circle { line_style, .. }
                    | Graphic::Arc { line_style, .. }
                    | Graphic::Poly { line_style, .. } => *line_style == style,
                    Graphic::Text { .. } => false,
                })
                .count()
        };
        assert_eq!(count(IpcLineStyle::Default), 20);
        assert_eq!(count(IpcLineStyle::Solid), 1);
        assert_eq!(count(IpcLineStyle::Dash), 1);
        let upright: Vec<_> = graphics
            .iter()
            .filter_map(|graphic| match graphic {
                Graphic::Text { keep_upright, .. } => Some(*keep_upright),
                _ => None,
            })
            .collect();
        assert_eq!(upright, [false, false, false]);
    }

    /// KiCad 6 and 7 footprints from the Tiny Tapeout demo library KiCad
    /// 10.0.6 ships. KiCad 10.0.6 loads the test point's 24 typeless
    /// `(width …)` lines as `(type solid)` and keeps its user text upright, and
    /// loads the jack's `(at -2.8 3.2 unlocked)` user text as `(unlocked yes)`.
    #[test]
    fn legacy_typeless_strokes_are_solid_and_legacy_unlocked_text_stays_unlocked() {
        use copperpour_ipc::{IpcGraphicDefinition as Graphic, IpcLineStyle};
        let test_point = extract_graphic_definitions(include_str!(
            "../../tests/fixtures/testpoint_keystone_5019_kicad6.kicad_mod"
        ))
        .unwrap();
        let styles: Vec<_> = test_point
            .iter()
            .filter_map(|graphic| match graphic {
                Graphic::Line { line_style, .. } => Some(*line_style),
                _ => None,
            })
            .collect();
        assert_eq!(styles, [IpcLineStyle::Solid; 24]);
        let upright = |graphics: &[Graphic]| -> Vec<bool> {
            graphics
                .iter()
                .filter_map(|graphic| match graphic {
                    Graphic::Text { keep_upright, .. } => Some(*keep_upright),
                    _ => None,
                })
                .collect()
        };
        assert_eq!(upright(&test_point), [true]);

        let jack = extract_graphic_definitions(include_str!(
            "../../tests/fixtures/cui_ujc_hp_3_kicad7.kicad_mod"
        ))
        .unwrap();
        assert_eq!(upright(&jack), [false]);
    }

    #[test]
    fn extracts_all_drawable_graphics_with_layers_and_widths() {
        use copperpour_ipc::IpcGraphicDefinition as Graphic;
        let graphics = extract_graphic_definitions(GRAPHIC_FOOTPRINT).unwrap();

        let lines: Vec<_> = graphics
            .iter()
            .filter(|g| matches!(g, Graphic::Line { .. }))
            .collect();
        assert_eq!(lines.len(), 2);
        assert!(matches!(
            lines[0],
            Graphic::Line { layer, width, start, .. }
                if layer == "F.SilkS" && *width == 0.12 && *start == (-0.6, -0.5)
        ));

        let rect = graphics
            .iter()
            .find(|g| matches!(g, Graphic::Rect { .. }))
            .unwrap();
        assert!(matches!(
            rect,
            Graphic::Rect { layer, width, filled, start, end, .. }
                if layer == "F.CrtYd" && *width == 0.05 && !*filled
                    && *start == (-0.8, -0.7) && *end == (0.8, 0.7)
        ));

        let circle = graphics
            .iter()
            .find(|g| matches!(g, Graphic::Circle { .. }))
            .unwrap();
        assert!(matches!(
            circle,
            Graphic::Circle { layer, filled, end, .. }
                if layer == "F.Fab" && *filled && *end == (0.25, 0.0)
        ));

        assert!(graphics
            .iter()
            .any(|g| matches!(g, Graphic::Arc { layer, mid, .. }
                if layer == "F.SilkS" && *mid == (0.0, -0.3))));

        let poly = graphics
            .iter()
            .find(|g| matches!(g, Graphic::Poly { .. }))
            .unwrap();
        assert!(matches!(
            poly,
            Graphic::Poly { points, filled, .. } if points.len() == 3 && *filled
        ));

        // Exactly one visible text: the fab ${REFERENCE}. The hidden fp_text,
        // the hidden Datasheet property, and the Reference/Value properties
        // (carried as first-class fields) are all excluded.
        let texts: Vec<_> = graphics
            .iter()
            .filter(|g| matches!(g, Graphic::Text { .. }))
            .collect();
        assert_eq!(texts.len(), 1, "{texts:?}");
        assert!(matches!(
            texts[0],
            Graphic::Text { text, layer, size, position, .. }
                if text == "${REFERENCE}" && layer == "F.Fab" && *size == 0.26
                    && *position == (0.0, 1.17)
        ));
    }

    #[test]
    fn legacy_reference_and_value_text_are_not_duplicated_as_graphics() {
        let source = r#"(footprint "Legacy"
  (layer "F.Cu")
  (fp_text reference "REF**" (at 0 -1) (layer "F.SilkS"))
  (fp_text value "Legacy" (at 0 1) (layer "F.Fab"))
  (fp_text user "visible" (at 0 0) (layer "F.Fab")))"#;

        let graphics = extract_graphic_definitions(source).unwrap();

        assert_eq!(graphics.len(), 1, "{graphics:?}");
        assert!(matches!(
            &graphics[0],
            copperpour_ipc::IpcGraphicDefinition::Text { text, .. } if text == "visible"
        ));
    }

    #[test]
    fn a_bare_pads_only_footprint_extracts_no_graphics() {
        assert!(extract_graphic_definitions(FOOTPRINT).unwrap().is_empty());
    }

    fn test_ctx() -> ToolContext {
        ToolContext::new(
            crate::tools::ServerConfig {
                kicad_cli: String::new(),
                kicad_binary: String::new(),
                // No IPC address: any handler that reaches the IPC layer fails
                // with the socket-path configuration error, so a different
                // error proves the handler rejected before trying IPC.
                ipc_address: String::new(),
                project_dir: None,
                jlcpcb_db_path: None,
                auto_load_toolsets: false,
                eager_toolsets: false,
            },
            std::sync::Arc::new(crate::router::ToolRouter::new()),
        )
    }

    fn result_text(res: &CallToolResult) -> String {
        match res.content.first() {
            Some(crate::mcp::protocol::ToolContent::Text { text }) => text.clone(),
            other => panic!("expected text content, got {other:?}"),
        }
    }

    // ─── File-editing fallback (ported from emolitor's PR #66) ────────────────

    /// A library footprint in the exact shape KiCad ships: TAB-indented, name
    /// without a library prefix, `REF**` placeholder, no `(at …)`. CRLF, the
    /// way KiCad's bundled libraries are written.
    fn library_footprint() -> String {
        [
            "(footprint \"R_0805_2012Metric\"",
            "\t(version 20260206)",
            "\t(generator \"kicad-footprint-generator\")",
            "\t(layer \"F.Cu\")",
            "\t(descr \"Resistor SMD 0805\")",
            "\t(property \"Reference\" \"REF**\"",
            "\t\t(at 0 -1.65 0)",
            "\t\t(layer \"F.SilkS\")",
            "\t)",
            "\t(property \"Value\" \"R_0805_2012Metric\"",
            "\t\t(at 0 1.65 0)",
            "\t\t(layer \"F.Fab\")",
            "\t)",
            "\t(pad \"1\" smd roundrect",
            "\t\t(at -0.9125 0)",
            "\t\t(size 1.025 1.4)",
            "\t\t(layers \"F.Cu\" \"F.Paste\" \"F.Mask\")",
            "\t)",
            ")",
            "",
        ]
        .join("\r\n")
    }

    const EMPTY_BOARD: &str = "(kicad_pcb
\t(version 20260206)
\t(generator \"pcbnew\")
\t(net 0 \"\")
)
";

    /// A project directory holding a registered `Resistor_SMD.pretty` library
    /// with one footprint, plus an empty board. The project fp-lib-table makes
    /// `Resistor_SMD:R_0805_2012Metric` resolve hermetically — no global
    /// table, no environment.
    pub(super) fn fallback_fixture(dir: &Path) -> std::path::PathBuf {
        let pretty = dir.join("Resistor_SMD.pretty");
        std::fs::create_dir_all(&pretty).unwrap();
        std::fs::write(
            pretty.join("R_0805_2012Metric.kicad_mod"),
            library_footprint(),
        )
        .unwrap();
        std::fs::write(
            dir.join("fp-lib-table"),
            format!(
                "(fp_lib_table\r\n\t(version 7)\r\n\t(lib (name \"Resistor_SMD\") (type \"KiCad\") (uri \"{}\") (options \"\") (descr \"\"))\r\n)\r\n",
                pretty.to_string_lossy()
            ),
        )
        .unwrap();
        let board = dir.join("b.kicad_pcb");
        std::fs::write(&board, EMPTY_BOARD).unwrap();
        board
    }

    fn legacy_fallback_fixture(dir: &Path) -> std::path::PathBuf {
        let pretty = dir.join("Legacy.pretty");
        std::fs::create_dir_all(&pretty).unwrap();
        std::fs::write(
            pretty.join("Socket.kicad_mod"),
            r#"(footprint "Socket" (version 20221018) (generator pcbnew)
  (layer "F.Cu")
  (attr exclude_from_pos_files)
  (fp_text reference "REF**" (at 0 -8.5) (layer "F.SilkS"))
  (fp_text value "Socket" (at 0 8.5) (layer "F.Fab"))
  (pad "1" thru_hole circle (at 0 0) (size 4 4) (drill 3) (layers "*.Cu" "*.Mask"))
  (model "../models/Socket.step"
    (offset (xyz 0 0 0))
    (scale (xyz 1 1 1))
    (rotate (xyz 0 0 0)))
)
"#,
        )
        .unwrap();
        std::fs::write(
            dir.join("fp-lib-table"),
            format!(
                "(fp_lib_table\n  (version 7)\n  (lib (name \"Legacy\") (type \"KiCad\") (uri \"{}\") (options \"\") (descr \"\"))\n)\n",
                pretty.to_string_lossy()
            ),
        )
        .unwrap();
        let board = dir.join("legacy.kicad_pcb");
        std::fs::write(&board, EMPTY_BOARD).unwrap();
        board
    }

    /// Net paren depth, ignoring anything inside quoted strings.
    fn count_parens(s: &str) -> i32 {
        let (mut depth, mut in_str, mut esc) = (0i32, false, false);
        for ch in s.chars() {
            match ch {
                _ if esc => esc = false,
                '\\' if in_str => esc = true,
                '"' => in_str = !in_str,
                '(' if !in_str => depth += 1,
                ')' if !in_str => depth -= 1,
                _ => {}
            }
        }
        depth
    }

    #[tokio::test]
    async fn unreachable_ipc_falls_back_to_writing_the_board_file() {
        // ipc_address is empty in test_ctx, which classifies as
        // transport-unreachable, and this fresh context has never observed the
        // board live, so the guarded file path is available.
        let tmp = tempfile::tempdir().unwrap();
        let board = fallback_fixture(tmp.path());

        let args = json!({
            "board": board.to_string_lossy(),
            "footprint": "Resistor_SMD:R_0805_2012Metric",
            "reference": "R7",
            "x": 50.0, "y": 60.0,
        });
        let res = handle_place_component(&args, &test_ctx()).await.unwrap();
        assert!(!res.is_error, "handler errored: {:?}", res.content);

        let out: serde_json::Value = serde_json::from_str(&result_text(&res)).unwrap();
        assert_eq!(out["source"], "file");
        assert_eq!(out["placed"], "R7");
        assert!(
            out["warning"]
                .as_str()
                .is_some_and(|w| w.contains("current Copperpour server session")
                    && w.contains("crashed or was force-quit")),
            "the fallback must warn that the file was edited directly: {out}"
        );

        let written = std::fs::read_to_string(&board).unwrap();
        assert_eq!(
            written.matches("(footprint \"").count(),
            1,
            "no footprint:\n{written}"
        );
        assert!(
            written.contains("(footprint \"Resistor_SMD:R_0805_2012Metric\""),
            "board should carry the Library:Footprint id:\n{written}"
        );
        assert!(
            written.contains("(at 50 60 0)"),
            "placement missing:\n{written}"
        );
        assert!(
            written.contains("(property \"Reference\" \"R7\""),
            "{written}"
        );
        assert!(
            written.contains("(pad \"1\" smd roundrect"),
            "the full definition must be carried:\n{written}"
        );
        assert!(written.contains("(uuid \""), "board items need a uuid");
        assert_eq!(
            count_parens(&written),
            0,
            "board is no longer balanced:\n{written}"
        );
    }

    #[tokio::test]
    async fn a_previously_live_board_blocks_place_components_file_fallback() {
        let tmp = tempfile::tempdir().unwrap();
        let board = fallback_fixture(tmp.path());
        let before = std::fs::read(&board).unwrap();
        let ctx = test_ctx();
        ctx.board_session.observe_live(&board);
        let args = json!({
            "board": board.to_string_lossy(),
            "footprint": "Resistor_SMD:R_0805_2012Metric",
            "reference": "R7",
            "x": 50.0,
            "y": 60.0,
        });

        let result = handle_place_component(&args, &ctx).await.unwrap();

        assert!(result.is_error);
        assert_eq!(
            crate::mcp::error::extract_error_kind(&result).as_deref(),
            Some("unsafe_file_fallback")
        );
        assert_eq!(std::fs::read(&board).unwrap(), before);
    }

    #[tokio::test]
    async fn fallback_replaces_legacy_fp_text_reference_and_preserves_models_and_attributes() {
        let tmp = tempfile::tempdir().unwrap();
        let board = legacy_fallback_fixture(tmp.path());
        let args = json!({
            "board": board.to_string_lossy(),
            "footprint": "Legacy:Socket",
            "reference": "SW1",
            "x": 20.0,
            "y": 30.0,
        });

        let result = handle_place_component(&args, &test_ctx()).await.unwrap();

        assert!(!result.is_error, "{:?}", result.content);
        let written = std::fs::read_to_string(&board).unwrap();
        assert!(written.contains("(fp_text reference \"SW1\""), "{written}");
        assert!(!written.contains("REF**"), "{written}");
        assert!(
            written.contains("(attr exclude_from_pos_files)"),
            "{written}"
        );
        assert!(
            written.contains("(model \"../models/Socket.step\""),
            "{written}"
        );
        assert!(copperpour_sexp::parse_sexp(&written).is_ok());
    }

    #[tokio::test]
    async fn fallback_rejects_a_duplicate_reference_without_writing() {
        let tmp = tempfile::tempdir().unwrap();
        let board = fallback_fixture(tmp.path());
        let args = json!({
            "board": board.to_string_lossy(),
            "footprint": "Resistor_SMD:R_0805_2012Metric",
            "reference": "R1",
            "x": 10.0,
            "y": 20.0,
        });
        let first = handle_place_component(&args, &test_ctx()).await.unwrap();
        assert!(!first.is_error, "{:?}", first.content);
        let before_duplicate = std::fs::read_to_string(&board).unwrap();

        let duplicate = handle_place_component(
            &json!({
                "board": board.to_string_lossy(),
                "footprint": "Resistor_SMD:R_0805_2012Metric",
                "reference": "R1",
                "x": 30.0,
                "y": 40.0,
            }),
            &test_ctx(),
        )
        .await
        .unwrap();

        assert!(duplicate.is_error);
        assert!(
            result_text(&duplicate).contains("already exists"),
            "{:?}",
            duplicate.content
        );
        assert_eq!(std::fs::read_to_string(board).unwrap(), before_duplicate);
    }

    /// A board whose footprint carries a `(property …)` *before* its root
    /// `(at …)`, with `angle` chosen so rotating changes the rendered width.
    ///
    /// KiCad's own writer emits `at` before properties and pads, so this shape
    /// never comes out of KiCad — but it is legal S-expression, KiCad loads it,
    /// and Copperpour reads boards it did not write.
    fn board_with_a_property_before_the_root_at(property_angle: &str) -> String {
        format!(
            "(kicad_pcb (version 20241229) (generator \"pcbnew\")\n\
             \x20 (footprint \"Test:R\"\n\
             \x20   (layer \"F.Cu\")\n\
             \x20   (property \"Reference\" \"R1\"\n\
             \x20     (at 0 -1.65{property_angle})\n\
             \x20     (layer \"F.SilkS\")\n\
             \x20     (effects (font (size 1 1)))\n\
             \x20   )\n\
             \x20   (at 10 20 30)\n\
             \x20   (pad \"1\" smd roundrect (at -0.9125 0 30) (size 1 1) (layers \"F.Cu\"))\n\
             \x20 )\n\
             )"
        )
    }

    /// Rotating must not splice the root `(at …)` at offsets measured before
    /// the child rewrite moved them.
    ///
    /// `apply_rotation_to_children` reformats every pad/property/fp_text
    /// `(at …)`, and those replacements change length. Running it before the
    /// root splice left the root offsets stale by that delta.
    ///
    /// Both directions are covered because they failed differently and only
    /// one of them was obviously a failure:
    ///
    /// * shrinking — a −45° property rotating to exactly 0, which `format_at`
    ///   drops, moving the root four bytes: `(at (at 10 20 75) (pad "1" …`;
    /// * growing — a property with no angle at all gaining one, which tripped
    ///   the root-head check with "placement update changed the footprint
    ///   root" and gave no hint why.
    #[test]
    fn rotating_splices_the_root_at_before_the_children_move_it() {
        for (label, property_angle, target, expected_root, expected_property) in [
            ("shrinking", " -45", 75.0, "(at 10 20 75)", "(at 0 -1.65)"),
            // 0° + 240° of delta = 240°, which `rotate_at_block` folds to 60°
            // to keep the text readable. The root becomes -90, KiCad's
            // spelling of 270.
            ("growing", "", 270.0, "(at 10 20 -90)", "(at 0 -1.65 60)"),
        ] {
            let board = board_with_a_property_before_the_root_at(property_angle);
            let updated = prepare_closed_board_footprint_update(
                &board,
                "R1",
                FootprintPlacementUpdate::Rotate { rotation: target },
            )
            .unwrap_or_else(|error| panic!("{label}: {error:?}"));

            assert!(
                copperpour_sexp::parse_sexp(&updated).is_ok(),
                "{label}: result must still parse\n{updated}"
            );
            assert!(
                updated.contains(expected_root),
                "{label}: root placement\n{updated}"
            );
            assert!(
                updated.contains(expected_property),
                "{label}: property angle\n{updated}"
            );
            assert!(
                !updated.contains("(at (at"),
                "{label}: a splice landed inside another block\n{updated}"
            );
        }
    }

    /// The root orientation is written the way KiCad writes it, and the
    /// children are not.
    ///
    /// Measured, not assumed. Rotating `R1` on the ecc83 demo to 247.5°
    /// through this path and then letting KiCad 10.0.5 re-save the board,
    /// KiCad rewrote the root `(at …)` as `-112.5` and left both pad angles at
    /// `247.5` exactly as Konnect had written them. Writing the un-normalised
    /// root means a footprint nobody touched shows a diff after the user's
    /// next save in KiCad, and it makes this path disagree with the IPC path —
    /// which ends up normalised because KiCad does it there.
    #[test]
    fn the_root_orientation_is_spelled_the_way_kicad_spells_it() {
        for (target, expected) in [
            (247.5, "(at 10 20 -112.5)"),
            (270.0, "(at 10 20 -90)"),
            (180.0, "(at 10 20 180)"),
            (181.0, "(at 10 20 -179)"),
            (90.0, "(at 10 20 90)"),
            (-450.0, "(at 10 20 -90)"),
            (360.0, "(at 10 20)"),
        ] {
            let board = board_with_a_property_before_the_root_at(" -45");
            let updated = prepare_closed_board_footprint_update(
                &board,
                "R1",
                FootprintPlacementUpdate::Rotate { rotation: target },
            )
            .unwrap_or_else(|error| panic!("{target}: {error:?}"));
            assert!(updated.contains(expected), "{target} =>\n{updated}");
        }

        // A move must not renormalise the angle it is preserving — that would
        // rewrite a root `(at …)` the caller did not ask to change. Checked on
        // a board whose root angle is already outside (-180, 180], which is
        // the only case where the two behaviours differ.
        let board = board_with_a_property_before_the_root_at(" -45")
            .replace("(at 10 20 30)", "(at 10 20 247.5)");
        let moved = prepare_closed_board_footprint_update(
            &board,
            "R1",
            FootprintPlacementUpdate::Move { x: 1.0, y: 2.0 },
        )
        .expect("move");
        assert!(moved.contains("(at 1 2 247.5)"), "{moved}");
    }

    /// A reference naming two footprints identifies none of them, and a board
    /// Copperpour cannot parse is not one it will write to. Both refuse, both
    /// with a structured reason, and both leave the file byte-identical.
    ///
    /// Added because neutering each guard found nothing: the PR describes all
    /// three behaviours — missing reference, duplicate reference, unusable
    /// board — and only the missing one was exercised, for move or rotate.
    #[tokio::test]
    async fn a_duplicate_reference_or_an_unparseable_board_refuses_without_writing() {
        let tmp = tempfile::tempdir().unwrap();

        let footprint = "  (footprint \"Test:R\"\n    (layer \"F.Cu\")\n    (at 10 20 30)\n    \
             (property \"Reference\" \"R1\" (at 0 -1.65 30) (layer \"F.SilkS\"))\n    \
             (pad \"1\" smd roundrect (at -0.9125 0 30) (size 1 1) (layers \"F.Cu\"))\n  )";
        let duplicated = tmp.path().join("duplicate.kicad_pcb");
        std::fs::write(
            &duplicated,
            format!(
                "(kicad_pcb (version 20241229) (generator \"pcbnew\")\n{footprint}\n{footprint}\n)"
            ),
        )
        .unwrap();

        let truncated = tmp.path().join("truncated.kicad_pcb");
        std::fs::write(
            &truncated,
            "(kicad_pcb (version 20241229)\n  (footprint \"Test:R\"",
        )
        .unwrap();

        for (label, board, structured_reference) in [
            ("duplicate", &duplicated, true),
            ("truncated", &truncated, false),
        ] {
            let before = std::fs::read_to_string(board).unwrap();
            for tool in ["move", "rotate"] {
                let mut arguments = json!({
                    "board": board.to_string_lossy(),
                    "reference": "R1",
                });
                let result = if tool == "move" {
                    arguments["x"] = json!(40.0);
                    arguments["y"] = json!(50.0);
                    handle_move_component(&arguments, &test_ctx())
                        .await
                        .unwrap()
                } else {
                    arguments["rotation"] = json!(90.0);
                    handle_rotate_component(&arguments, &test_ctx())
                        .await
                        .unwrap()
                };

                assert!(result.is_error, "{label}/{tool} must refuse");
                let text = result_text(&result);
                if structured_reference {
                    assert!(
                        text.contains("invalid_argument") && text.contains("reference"),
                        "{label}/{tool}: {text}"
                    );
                } else {
                    assert!(
                        text.contains("Refusing to edit the board"),
                        "{label}/{tool}: {text}"
                    );
                }
                assert_eq!(
                    std::fs::read_to_string(board).unwrap(),
                    before,
                    "{label}/{tool} must leave the board byte-identical"
                );
            }
        }
    }

    /// The same board must survive a move, which does not rewrite children at
    /// all — pinned so a future refactor cannot reintroduce the coupling by
    /// making the move path share the rotate path's ordering.
    #[test]
    fn moving_keeps_every_child_untouched() {
        let board = board_with_a_property_before_the_root_at(" -45");
        let updated = prepare_closed_board_footprint_update(
            &board,
            "R1",
            FootprintPlacementUpdate::Move { x: 40.0, y: 50.0 },
        )
        .expect("move must succeed");

        assert!(updated.contains("(at 40 50 30)"), "{updated}");
        assert!(updated.contains("(at 0 -1.65 -45)"), "{updated}");
        assert!(updated.contains("(at -0.9125 0 30)"), "{updated}");
    }

    async fn placed_fallback_fixture(dir: &Path) -> std::path::PathBuf {
        let board = fallback_fixture(dir);
        let placed = handle_place_component(
            &json!({
                "board": board.to_string_lossy(),
                "footprint": "Resistor_SMD:R_0805_2012Metric",
                "reference": "R1",
                "x": 10.0,
                "y": 20.0,
                "rotation": 30.0,
            }),
            &test_ctx(),
        )
        .await
        .unwrap();
        assert!(!placed.is_error, "{:?}", placed.content);
        board
    }

    #[tokio::test]
    async fn unreachable_ipc_moves_an_existing_footprint_in_the_board_file() {
        let tmp = tempfile::tempdir().unwrap();
        let board = placed_fallback_fixture(tmp.path()).await;

        let moved = handle_move_component(
            &json!({
                "board": board.to_string_lossy(),
                "reference": "R1",
                "x": 40.0,
                "y": 50.0,
            }),
            &test_ctx(),
        )
        .await
        .unwrap();

        assert!(!moved.is_error, "{:?}", moved.content);
        let result: serde_json::Value =
            serde_json::from_str(&result_text(&moved)).expect("move result must be JSON");
        assert_eq!(result["source"], "file");
        let written = std::fs::read_to_string(board).unwrap();
        assert!(written.contains("(at 40 50 30)"), "{written}");
        assert!(written.contains("(pad \"1\" smd roundrect"), "{written}");
        assert!(copperpour_sexp::parse_sexp(&written).is_ok());
    }

    #[tokio::test]
    async fn unreachable_ipc_rotates_an_existing_footprint_in_the_board_file() {
        let tmp = tempfile::tempdir().unwrap();
        let board = placed_fallback_fixture(tmp.path()).await;

        let rotated = handle_rotate_component(
            &json!({
                "board": board.to_string_lossy(),
                "reference": "R1",
                "rotation": 270.0,
            }),
            &test_ctx(),
        )
        .await
        .unwrap();

        assert!(!rotated.is_error, "{:?}", rotated.content);
        let result: serde_json::Value =
            serde_json::from_str(&result_text(&rotated)).expect("rotate result must be JSON");
        assert_eq!(result["source"], "file");
        let written = std::fs::read_to_string(board).unwrap();
        // The root orientation is normalised to (-180, 180] because that is
        // what KiCad writes; the pad and text angles are not, because KiCad
        // leaves those alone. Both measured against a real KiCad 10 re-save.
        assert!(written.contains("(at 10 20 -90)"), "{written}");
        assert!(written.contains("(at -0.9125 0 270)"), "{written}");
        assert!(written.contains("(at 0 -1.65 90)"), "{written}");
        assert!(copperpour_sexp::parse_sexp(&written).is_ok());
    }

    #[tokio::test]
    async fn unreachable_ipc_sets_multiple_placements_with_one_file_write() {
        let tmp = tempfile::tempdir().unwrap();
        let board = fallback_fixture(tmp.path());
        for (reference, x) in [("R1", 10.0), ("R2", 20.0)] {
            let placed = handle_place_component(
                &json!({
                    "board": board.to_string_lossy(),
                    "footprint": "Resistor_SMD:R_0805_2012Metric",
                    "reference": reference,
                    "x": x,
                    "y": 20.0,
                    "rotation": 0.0,
                }),
                &test_ctx(),
            )
            .await
            .unwrap();
            assert!(!placed.is_error, "{reference}: {:?}", placed.content);
        }

        let result = handle_set_component_placements(
            &json!({
                "board": board.to_string_lossy(),
                "placements": [
                    {"reference": "R1", "x": 40.0, "y": 50.0, "rotation": 270.0},
                    {"reference": "R2", "x": 60.0, "y": 70.0, "rotation": 45.0}
                ]
            }),
            &test_ctx(),
        )
        .await
        .unwrap();

        assert!(!result.is_error, "{:?}", result.content);
        let response: serde_json::Value =
            serde_json::from_str(&result_text(&result)).expect("batch result must be JSON");
        assert_eq!(response["source"], "file");
        assert_eq!(response["count"], 2);
        let written = std::fs::read_to_string(&board).unwrap();
        assert!(written.contains("(at 40 50 -90)"), "{written}");
        assert!(written.contains("(at 60 70 45)"), "{written}");
        assert!(copperpour_sexp::parse_sexp(&written).is_ok());

        // The response reports what the file now holds, not what was asked:
        // the requested 270 is stored (and therefore reported) as -90. An
        // echoed 270 here and a -90 in the file would be two different
        // answers for one final state.
        assert_eq!(response["placements"][0]["reference"], "R1");
        assert_eq!(response["placements"][0]["rotation"], -90.0, "{response}");
        assert_eq!(response["placements"][1]["rotation"], 45.0);
        assert_eq!(response["placements"][0]["x"], 40.0);
        assert_eq!(response["placements"][1]["y"], 70.0);
    }

    #[tokio::test]
    async fn placement_batch_is_all_or_nothing_for_missing_and_duplicate_references() {
        let tmp = tempfile::tempdir().unwrap();
        let board = placed_fallback_fixture(tmp.path()).await;
        let before = std::fs::read_to_string(&board).unwrap();

        let missing = handle_set_component_placements(
            &json!({
                "board": board.to_string_lossy(),
                "placements": [
                    {"reference": "R1", "x": 40.0, "y": 50.0, "rotation": 90.0},
                    {"reference": "R404", "x": 60.0, "y": 70.0, "rotation": 0.0}
                ]
            }),
            &test_ctx(),
        )
        .await
        .unwrap();
        assert!(missing.is_error);
        assert!(result_text(&missing).contains("R404"));
        assert_eq!(std::fs::read_to_string(&board).unwrap(), before);

        let duplicate = handle_set_component_placements(
            &json!({
                "board": board.to_string_lossy(),
                "placements": [
                    {"reference": "R1", "x": 40.0, "y": 50.0, "rotation": 90.0},
                    {"reference": "R1", "x": 60.0, "y": 70.0, "rotation": 0.0}
                ]
            }),
            &test_ctx(),
        )
        .await
        .unwrap();
        let text = result_text(&duplicate);
        assert!(duplicate.is_error);
        assert!(text.contains("invalid_argument") && text.contains("placements[1].reference"));
        assert_eq!(std::fs::read_to_string(&board).unwrap(), before);
    }

    #[tokio::test]
    async fn closed_board_move_and_rotate_reject_a_missing_reference_without_writing() {
        let tmp = tempfile::tempdir().unwrap();
        let board = placed_fallback_fixture(tmp.path()).await;
        let before = std::fs::read_to_string(&board).unwrap();

        let moved = handle_move_component(
            &json!({
                "board": board.to_string_lossy(),
                "reference": "R404",
                "x": 40.0,
                "y": 50.0,
            }),
            &test_ctx(),
        )
        .await
        .unwrap();
        assert!(moved.is_error);
        assert!(result_text(&moved).contains("R404"));
        // Not just "an error": a reference that names nothing on this board is
        // the caller's mistake, so it carries the structured field rather than
        // reaching them as an opaque handler_error (#194's class).
        assert!(
            result_text(&moved).contains("invalid_argument")
                && result_text(&moved).contains("\"field\":\"reference\""),
            "{}",
            result_text(&moved)
        );
        assert_eq!(std::fs::read_to_string(&board).unwrap(), before);

        let rotated = handle_rotate_component(
            &json!({
                "board": board.to_string_lossy(),
                "reference": "R404",
                "rotation": 90.0,
            }),
            &test_ctx(),
        )
        .await
        .unwrap();
        assert!(rotated.is_error);
        assert!(result_text(&rotated).contains("R404"));
        assert_eq!(std::fs::read_to_string(&board).unwrap(), before);
    }

    #[tokio::test]
    async fn reachable_rejection_prevents_placement_file_fallbacks() {
        let tmp = tempfile::tempdir().unwrap();
        let board = placed_fallback_fixture(tmp.path()).await;
        let before = std::fs::read_to_string(&board).unwrap();
        let server = spawn_rejecting_kicad();
        let ctx = ToolContext::new(
            crate::tools::ServerConfig {
                kicad_cli: String::new(),
                kicad_binary: String::new(),
                ipc_address: server.address().to_string(),
                project_dir: None,
                jlcpcb_db_path: None,
                auto_load_toolsets: false,
                eager_toolsets: false,
            },
            std::sync::Arc::new(crate::router::ToolRouter::new()),
        );

        let moved = handle_move_component(
            &json!({
                "board": board.to_string_lossy(),
                "reference": "R1",
                "x": 40.0,
                "y": 50.0,
            }),
            &ctx,
        )
        .await
        .unwrap();
        assert!(moved.is_error);
        assert!(result_text(&moved).contains("not modified"));
        assert_eq!(std::fs::read_to_string(&board).unwrap(), before);

        let rotated = handle_rotate_component(
            &json!({
                "board": board.to_string_lossy(),
                "reference": "R1",
                "rotation": 90.0,
            }),
            &ctx,
        )
        .await
        .unwrap();
        assert!(rotated.is_error);
        assert!(result_text(&rotated).contains("not modified"));
        assert_eq!(std::fs::read_to_string(&board).unwrap(), before);

        let batch = handle_set_component_placements(
            &json!({
                "board": board.to_string_lossy(),
                "placements": [
                    {"reference": "R1", "x": 40.0, "y": 50.0, "rotation": 90.0}
                ]
            }),
            &ctx,
        )
        .await
        .unwrap();
        assert!(batch.is_error);
        assert!(result_text(&batch).contains("not modified"));
        assert_eq!(std::fs::read_to_string(&board).unwrap(), before);
    }

    const FLIP_FOOTPRINT: &str = r#"(footprint "Test:Flip"
  (layer "F.Cu")
  (at 10 20 30)
  (property "Reference" "U1"
    (at 1 -2 40)
    (layer "F.SilkS")
    (effects (font (size 1 1)) (justify left))
  )
  (fp_line (start 1 2) (end 3 4)
    (stroke (width 0.1) (type solid))
    (layer "F.SilkS")
  )
  (fp_arc (start 1 2) (mid 3 4) (end 5 6)
    (stroke (width 0.1) (type solid))
    (layer "F.Fab")
  )
  (fp_poly (pts (xy 1 2) (xy 3 4) (xy 5 6))
    (stroke (width 0.1) (type solid))
    (fill no)
    (layer "F.CrtYd")
  )
  (pad "1" smd roundrect (at 2 3 50) (size 1 2)
    (layers "F.Cu" "F.Paste" "F.Mask")
    (roundrect_rratio 0.25)
  )
  (model "../models/Test.step"
    (offset (xyz 0 0 0))
    (scale (xyz 1 1 1))
    (rotate (xyz 0 0 90))
  )
)"#;

    fn flip_board(footprints: &[&str], eol: &str) -> String {
        let body = footprints
            .iter()
            .flat_map(|footprint| footprint.lines())
            .map(|line| format!("  {line}"))
            .collect::<Vec<_>>()
            .join(eol);
        format!(
            "(kicad_pcb{eol}  (version 20260206){eol}  (generator \"pcbnew\"){eol}  \
             (net 0 \"\"){eol}{body}{eol}){eol}"
        )
    }

    #[test]
    fn flip_footprint_matches_kicads_library_frame_transform() {
        let flipped = flip_footprint_block(FLIP_FOOTPRINT, two_layers()).unwrap();

        assert!(flipped.contains("(layer \"B.Cu\")"), "{flipped}");
        assert!(flipped.contains("(at 10 20 -30)"), "{flipped}");
        assert!(flipped.contains("(at 1 2 140)"), "{flipped}");
        assert!(flipped.contains("(layer \"B.SilkS\")"), "{flipped}");
        assert!(flipped.contains("(justify left mirror)"), "{flipped}");
        assert!(flipped.contains("(start 1 -2)"), "{flipped}");
        assert!(flipped.contains("(end 3 -4)"), "{flipped}");
        assert!(flipped.contains("(start 5 -6)"), "{flipped}");
        assert!(flipped.contains("(mid 3 -4)"), "{flipped}");
        assert!(flipped.contains("(end 1 -2)"), "{flipped}");
        assert!(flipped.contains("(xy 1 -2)"), "{flipped}");
        assert!(flipped.contains("(at 2 -3 310)"), "{flipped}");
        assert!(
            flipped.contains("(layers \"B.Cu\" \"B.Paste\" \"B.Mask\")"),
            "{flipped}"
        );
        // The model is carried through verbatim. That is only safe because a
        // model whose placement a flip would have to move is refused outright
        // — see `a_model_a_flip_would_move_is_refused`. `rotate.z` is not one
        // of those: a flip does not touch Z.
        assert!(
            flipped.contains("(offset (xyz 0 0 0))") && flipped.contains("(rotate (xyz 0 0 90))"),
            "a model a flip does not move must survive verbatim: {flipped}"
        );
        assert!(copperpour_sexp::parse_sexp(&flipped).is_ok());
    }

    /// A property with no position is metadata, not geometry, and must not
    /// stop the flip.
    ///
    /// KiCad writes `(property ki_fp_filters "R_* Resistor_*")` — a bare
    /// token, no `(at …)`, no layer — into every footprint it places from a
    /// library. There are **779** of them across the 19 boards in
    /// `share/kicad/demos`. Requiring exactly one `(at …)` on every property
    /// therefore refused practically every real board with "property must
    /// contain exactly one direct (at ...) block", which is what the first
    /// live run of this tool hit on the stock ecc83 demo.
    ///
    /// Nothing offline could have caught it: the synthetic fixture has only
    /// positioned properties.
    #[test]
    fn a_positionless_property_does_not_block_the_flip() {
        let with_metadata = FLIP_FOOTPRINT.replace(
            "  (pad \"1\"",
            "  (property ki_fp_filters \"R_* Resistor_*\")\n  (pad \"1\"",
        );
        assert!(
            with_metadata.contains("ki_fp_filters"),
            "fixture must carry the metadata property"
        );

        let flipped = flip_footprint_block(&with_metadata, two_layers())
            .expect("a positionless property must not block the flip");

        // Carried through untouched — it has no geometry to mirror.
        assert!(
            flipped.contains("(property ki_fp_filters \"R_* Resistor_*\")"),
            "{flipped}"
        );
        // And the positioned ones still flipped.
        assert!(flipped.contains("(layer \"B.Cu\")"), "{flipped}");
        assert!(copperpour_sexp::parse_sexp(&flipped).is_ok());
    }

    /// A footprint that is not on either copper side has no "other side" to
    /// flip to, so it is refused rather than moved to one.
    ///
    /// Reachable on any board a user hand-edited or an older tool wrote — and
    /// the guard existed with nothing exercising it, which the neuter pass
    /// found.
    #[tokio::test]
    async fn a_footprint_on_neither_side_of_the_board_is_refused() {
        let tmp = tempfile::tempdir().unwrap();
        let board = tmp.path().join("inner.kicad_pcb");
        let stranded = FLIP_FOOTPRINT.replace("(layer \"F.Cu\")", "(layer \"In1.Cu\")");
        let before = flip_board(&[&stranded], "\n");
        std::fs::write(&board, &before).unwrap();

        let refusal = handle_flip_component(
            &json!({
                "board": board.to_string_lossy(),
                "reference": "U1",
                "layer": "B.Cu",
            }),
            &test_ctx(),
        )
        .await
        .unwrap();

        assert!(refusal.is_error, "{:?}", refusal.content);
        let text = result_text(&refusal);
        assert!(text.contains("In1.Cu"), "{text}");
        assert!(text.contains("neither side"), "{text}");
        assert_eq!(std::fs::read_to_string(board).unwrap(), before);
    }

    /// KiCad's own flip transforms a model's Y offset and its X/Y rotation.
    /// This path does not transform models at all, so rather than silently
    /// leave one behind it refuses — the same policy the rest of the flip
    /// applies to geometry it cannot mirror.
    ///
    /// Refusing costs almost nothing. Across all **14,818** footprints in
    /// KiCad 10's standard libraries carrying a `(model …)`, `offset.y` is
    /// non-zero in **3** and `rotate.x`/`rotate.y` in **none**; the worst is
    /// `RaspberryPi_Pico_Common_THT` at -24.13 mm, which would put its model
    /// roughly 48 mm out. The 84 with a non-zero `rotate.z` are unaffected.
    #[test]
    fn a_model_a_flip_would_move_is_refused() {
        for (label, replacement, needle) in [
            ("offset.y", "(offset (xyz 0 -24.13 0))", "offset.y"),
            ("rotate.x", "(rotate (xyz 90 0 0))", "rotate.x"),
            ("rotate.y", "(rotate (xyz 0 90 0))", "rotate.y"),
        ] {
            let source = FLIP_FOOTPRINT
                .replace("(offset (xyz 0 0 0))", replacement)
                .replace("(rotate (xyz 0 0 90))", replacement);
            let error = flip_footprint_block(&source, two_layers())
                .unwrap_err()
                .to_string();
            assert!(error.contains(needle), "{label}: {error}");
            assert!(error.contains("would have to move it"), "{label}: {error}");
        }

        // The fields a flip leaves alone must not trip it.
        for untouched in ["(offset (xyz 8.89 0 0))", "(rotate (xyz 0 0 90))"] {
            let source = FLIP_FOOTPRINT
                .replace("(offset (xyz 0 0 0))", untouched)
                .replace("(rotate (xyz 0 0 90))", untouched);
            assert!(
                flip_footprint_block(&source, two_layers()).is_ok(),
                "{untouched} is not moved by a flip and must be accepted"
            );
        }
    }

    #[tokio::test]
    async fn unreachable_ipc_flips_an_existing_footprint_to_the_requested_side() {
        let tmp = tempfile::tempdir().unwrap();
        let board = tmp.path().join("flip.kicad_pcb");
        std::fs::write(
            &board,
            format!(
                "(kicad_pcb\n  (version 20260206)\n  (generator \"pcbnew\")\n  (net 0 \"\")\n{}\n)\n",
                FLIP_FOOTPRINT
                    .lines()
                    .map(|line| format!("  {line}"))
                    .collect::<Vec<_>>()
                    .join("\n")
            ),
        )
        .unwrap();

        let flipped = handle_flip_component(
            &json!({
                "board": board.to_string_lossy(),
                "reference": "U1",
                "layer": "B.Cu",
            }),
            &test_ctx(),
        )
        .await
        .unwrap();

        assert!(!flipped.is_error, "{:?}", flipped.content);
        let result: serde_json::Value =
            serde_json::from_str(&result_text(&flipped)).expect("flip result must be JSON");
        assert_eq!(result["source"], "file");
        assert_eq!(result["layer"], "B.Cu");
        let written = std::fs::read_to_string(&board).unwrap();
        assert!(written.contains("(layer \"B.Cu\")"), "{written}");
        assert!(written.contains("(layers \"B.Cu\" \"B.Paste\" \"B.Mask\")"));

        let repeated = handle_flip_component(
            &json!({
                "board": board.to_string_lossy(),
                "reference": "U1",
                "layer": "B.Cu",
            }),
            &test_ctx(),
        )
        .await
        .unwrap();
        assert!(!repeated.is_error, "{:?}", repeated.content);
        assert_eq!(std::fs::read_to_string(board).unwrap(), written);
    }

    #[tokio::test]
    async fn a_reachable_kicad_that_rejects_the_flip_never_touches_the_file() {
        let tmp = tempfile::tempdir().unwrap();
        let board = tmp.path().join("flip.kicad_pcb");
        let before = format!(
            "(kicad_pcb\n  (version 20260206)\n  (generator \"pcbnew\")\n  (net 0 \"\")\n{}\n)\n",
            FLIP_FOOTPRINT
                .lines()
                .map(|line| format!("  {line}"))
                .collect::<Vec<_>>()
                .join("\n")
        );
        std::fs::write(&board, &before).unwrap();
        let server = spawn_rejecting_kicad();
        let ctx = ToolContext::new(
            crate::tools::ServerConfig {
                kicad_cli: String::new(),
                kicad_binary: String::new(),
                ipc_address: server.address().to_string(),
                project_dir: None,
                jlcpcb_db_path: None,
                auto_load_toolsets: false,
                eager_toolsets: false,
            },
            std::sync::Arc::new(crate::router::ToolRouter::new()),
        );

        let flipped = handle_flip_component(
            &json!({
                "board": board.to_string_lossy(),
                "reference": "U1",
                "layer": "B.Cu",
            }),
            &ctx,
        )
        .await
        .unwrap();

        // Since #604, flip_component performs the write over IPC via
        // attempt_ipc_write, exactly like set_component_placements: a
        // reachable KiCad that answers but rejects the request may be alive
        // and holding this exact board, so the file must not be touched —
        // "Rejected" fails closed rather than falling back to the file
        // (mirrors `a_reachable_kicad_that_rejects_never_touches_the_file`).
        assert!(
            flipped.is_error,
            "a rejection must not be reported as success"
        );
        let text = result_text(&flipped);
        assert!(
            text.contains("rejected the footprint flip") && text.contains("not modified"),
            "the error must say the file was left alone: {text}"
        );
        assert_eq!(
            std::fs::read_to_string(&board).unwrap(),
            before,
            "a reachable KiCAD that says no must never trigger the file fallback"
        );
    }

    /// A rep0 endpoint playing a KiCad that holds `board` open with one
    /// footprint on it, answering `GetItems`/`BeginCommit`/`EndCommit`/
    /// `GetVersion`, and dispatching `FlipItems` to `flip_items`. Exercises
    /// `handle_flip_component`'s IPC path end-to-end through
    /// `attempt_ipc_write` (issue #604) — not just `flip_footprint` in
    /// isolation, which `flip_footprint_test.rs` already covers.
    fn spawn_flip_capable_kicad(
        board: &std::path::Path,
        footprint: std::sync::Arc<
            std::sync::Mutex<copperpour_ipc::gen::kiapi::board::types::FootprintInstance>,
        >,
        flip_items: impl Fn(
                &copperpour_ipc::gen::kiapi::board::commands::FlipItems,
                &mut copperpour_ipc::gen::kiapi::board::types::FootprintInstance,
            ) -> copperpour_ipc::gen::kiapi::board::commands::FlipItemsResponse
            + Send
            + 'static,
    ) -> crate::test_support::MockIpcServer {
        use copperpour_ipc::gen::kiapi;
        use prost::Message;

        let board = board.to_string_lossy().to_string();
        crate::test_support::MockIpcServer::spawn("flip-capable-kicad", move |request| {
            let command = request.message.expect("a command");
            let body = if command.type_url.ends_with("GetOpenDocuments") {
                Some(copperpour_ipc::builders::pack_any(
                    &kiapi::common::commands::GetOpenDocumentsResponse {
                        documents: vec![kiapi::common::types::DocumentSpecifier {
                            r#type: kiapi::common::types::DocumentType::DoctypePcb as i32,
                            project: None,
                            identifier: Some(
                                kiapi::common::types::document_specifier::Identifier::BoardFilename(
                                    board.clone(),
                                ),
                            ),
                        }],
                    },
                    "kiapi.common.commands.GetOpenDocumentsResponse",
                ))
            } else if command.type_url.ends_with("GetItems") {
                let fp = footprint.lock().unwrap().clone();
                Some(copperpour_ipc::builders::pack_any(
                    &kiapi::common::commands::GetItemsResponse {
                        header: None,
                        status: kiapi::common::types::ItemRequestStatus::IrsOk as i32,
                        items: vec![copperpour_ipc::builders::pack_any(
                            &fp,
                            "kiapi.board.types.FootprintInstance",
                        )],
                    },
                    "kiapi.common.commands.GetItemsResponse",
                ))
            } else if command.type_url.ends_with("BeginCommit") {
                Some(copperpour_ipc::builders::pack_any(
                    &kiapi::common::commands::BeginCommitResponse {
                        id: Some(kiapi::common::types::Kiid {
                            value: "flip-commit".to_string(),
                        }),
                    },
                    "kiapi.common.commands.BeginCommitResponse",
                ))
            } else if command.type_url.ends_with("EndCommit") {
                Some(copperpour_ipc::builders::pack_any(
                    &kiapi::common::commands::EndCommitResponse {},
                    "kiapi.common.commands.EndCommitResponse",
                ))
            } else if command.type_url.ends_with("kiapi.board.commands.FlipItems") {
                let flip =
                    kiapi::board::commands::FlipItems::decode(command.value.as_slice()).unwrap();
                let mut held = footprint.lock().unwrap();
                let response = flip_items(&flip, &mut held);
                Some(copperpour_ipc::builders::pack_any(
                    &response,
                    "kiapi.board.commands.FlipItemsResponse",
                ))
            } else if command.type_url.ends_with("GetVersion") {
                Some(copperpour_ipc::builders::pack_any(
                    &kiapi::common::commands::GetVersionResponse {
                        version: Some(kiapi::common::types::KiCadVersion {
                            major: 10,
                            minor: 0,
                            patch: 5,
                            full_version: "10.0.5".to_string(),
                        }),
                    },
                    "kiapi.common.commands.GetVersionResponse",
                ))
            } else {
                None
            };
            kiapi::common::ApiResponse {
                status: Some(kiapi::common::ApiResponseStatus {
                    status: kiapi::common::ApiStatusCode::AsOk as i32,
                    error_message: String::new(),
                }),
                header: None,
                message: body,
            }
        })
    }

    fn flip_test_footprint(
        kiid: &str,
        layer: copperpour_ipc::gen::kiapi::board::types::BoardLayer,
    ) -> copperpour_ipc::gen::kiapi::board::types::FootprintInstance {
        use copperpour_ipc::gen::kiapi;
        kiapi::board::types::FootprintInstance {
            id: Some(kiapi::common::types::Kiid {
                value: kiid.to_string(),
            }),
            position: Some(copperpour_ipc::builders::vec2(10.0, 20.0)),
            orientation: Some(kiapi::common::types::Angle { value_degrees: 0.0 }),
            layer: layer as i32,
            reference_field: Some(kiapi::board::types::Field {
                name: "Reference".to_string(),
                text: Some(kiapi::board::types::BoardText {
                    text: Some(kiapi::common::types::Text {
                        text: "U1".to_string(),
                        ..Default::default()
                    }),
                    ..Default::default()
                }),
                ..Default::default()
            }),
            definition: Some(kiapi::board::types::Footprint {
                ..Default::default()
            }),
            ..Default::default()
        }
    }

    #[tokio::test]
    async fn flip_over_ipc_succeeds_when_kicad_holds_this_exact_board() {
        use copperpour_ipc::gen::kiapi;

        let tmp = tempfile::tempdir().unwrap();
        let board = tmp.path().join("flip.kicad_pcb");
        std::fs::write(&board, "(kicad_pcb\n  (version 20260206)\n)\n").unwrap();
        let before = std::fs::read_to_string(&board).unwrap();

        let footprint = std::sync::Arc::new(std::sync::Mutex::new(flip_test_footprint(
            "U1-kiid",
            kiapi::board::types::BoardLayer::BlFCu,
        )));
        let server = spawn_flip_capable_kicad(&board, footprint, |flip, held| {
            assert_eq!(
                flip.direction(),
                kiapi::board::commands::BoardFlipDirection::BfdTopBottom
            );
            held.layer = kiapi::board::types::BoardLayer::BlBCu as i32;
            let item =
                copperpour_ipc::builders::pack_any(held, "kiapi.board.types.FootprintInstance");
            kiapi::board::commands::FlipItemsResponse {
                header: None,
                status: kiapi::common::types::ItemRequestStatus::IrsOk as i32,
                flipped_items: vec![kiapi::board::commands::ItemFlipResult {
                    status: Some(kiapi::common::commands::ItemStatus {
                        code: kiapi::common::commands::ItemStatusCode::IscOk as i32,
                        error_message: String::new(),
                    }),
                    item: Some(item),
                }],
            }
        });
        let ctx = crate::tools::pcb_board::board_mock::ctx_talking_to(server.address().to_string());

        let result = handle_flip_component(
            &json!({"board": board, "reference": "U1", "layer": "B.Cu"}),
            &ctx,
        )
        .await
        .unwrap();

        assert!(!result.is_error, "{:?}", result.content);
        let body: serde_json::Value =
            serde_json::from_str(&result_text(&result)).expect("json result");
        assert_eq!(body["source"], "ipc");
        assert_eq!(body["layer"], "B.Cu");
        assert_eq!(body["flipped"], "U1");
        // The IPC path never touches the file.
        assert_eq!(std::fs::read_to_string(&board).unwrap(), before);
    }

    /// `kiid` reaches the live flip: a stale one is refused before any
    /// write, and the footprint's own one flips it (#880).
    #[tokio::test]
    async fn flip_over_ipc_takes_kiid_and_refuses_a_stale_one() {
        use copperpour_ipc::gen::kiapi;

        let tmp = tempfile::tempdir().unwrap();
        let board = tmp.path().join("flip.kicad_pcb");
        std::fs::write(&board, "(kicad_pcb\n  (version 20260206)\n)\n").unwrap();
        let footprint = std::sync::Arc::new(std::sync::Mutex::new(flip_test_footprint(
            "U1-kiid",
            kiapi::board::types::BoardLayer::BlFCu,
        )));
        let server = spawn_flip_capable_kicad(&board, footprint.clone(), |_, held| {
            held.layer = kiapi::board::types::BoardLayer::BlBCu as i32;
            let item =
                copperpour_ipc::builders::pack_any(held, "kiapi.board.types.FootprintInstance");
            kiapi::board::commands::FlipItemsResponse {
                header: None,
                status: kiapi::common::types::ItemRequestStatus::IrsOk as i32,
                flipped_items: vec![kiapi::board::commands::ItemFlipResult {
                    status: Some(kiapi::common::commands::ItemStatus {
                        code: kiapi::common::commands::ItemStatusCode::IscOk as i32,
                        error_message: String::new(),
                    }),
                    item: Some(item),
                }],
            }
        });
        let ctx = crate::tools::pcb_board::board_mock::ctx_talking_to(server.address().to_string());
        let flip =
            |kiid: &str| json!({"board": board, "reference": "U1", "layer": "B.Cu", "kiid": kiid});

        let stale = handle_flip_component(&flip("not-on-the-board"), &ctx)
            .await
            .unwrap();
        assert_eq!(
            crate::mcp::error::extract_error_kind(&stale).as_deref(),
            Some("stale_target"),
            "{}",
            result_text(&stale)
        );
        assert_eq!(
            footprint.lock().unwrap().layer,
            kiapi::board::types::BoardLayer::BlFCu as i32
        );

        let pinned = handle_flip_component(&flip("U1-kiid"), &ctx).await.unwrap();
        assert!(!pinned.is_error, "{}", result_text(&pinned));
        let body: serde_json::Value = serde_json::from_str(&result_text(&pinned)).unwrap();
        assert_eq!(body["kiid"], "U1-kiid");
        assert_eq!(body["layer"], "B.Cu");
    }

    /// A live flip refused as ambiguous names each KIID, for a retry with
    /// `kiid`.
    #[test]
    fn an_ambiguous_live_flip_is_refused_naming_each_kiid() {
        let result = flip_outcome_result(copperpour_ipc::IpcFlipOutcome::AmbiguousTarget {
            target: "REF**".to_string(),
            candidates: vec!["a-kiid".to_string(), "b-kiid".to_string()],
            reason: "Footprint reference 'REF**' matches 2 placed footprints".to_string(),
        });
        assert!(result.is_error);
        let body: serde_json::Value = serde_json::from_str(&result_text(&result)).unwrap();
        assert_eq!(body["error"]["kind"], "ambiguous_target", "{body}");
        assert_eq!(body["error"]["target"], "REF**", "{body}");
        assert_eq!(
            body["error"]["candidates"],
            json!(["a-kiid", "b-kiid"]),
            "{body}"
        );
    }

    #[tokio::test]
    async fn flip_over_ipc_reports_unsupported_capability_on_as_unhandled() {
        use copperpour_ipc::gen::kiapi;

        let tmp = tempfile::tempdir().unwrap();
        let board = tmp.path().join("flip.kicad_pcb");
        std::fs::write(&board, "(kicad_pcb\n  (version 20260206)\n)\n").unwrap();
        let before = std::fs::read_to_string(&board).unwrap();

        let footprint = std::sync::Arc::new(std::sync::Mutex::new(flip_test_footprint(
            "U1-kiid",
            kiapi::board::types::BoardLayer::BlFCu,
        )));
        let board_for_mock = board.clone();
        let server =
            crate::test_support::MockIpcServer::spawn("flip-unsupported", move |request| {
                let command = request.message.expect("a command");
                let body = if command.type_url.ends_with("GetOpenDocuments") {
                    Some(copperpour_ipc::builders::pack_any(
                        &kiapi::common::commands::GetOpenDocumentsResponse {
                            documents: vec![kiapi::common::types::DocumentSpecifier {
                            r#type: kiapi::common::types::DocumentType::DoctypePcb as i32,
                            project: None,
                            identifier: Some(
                                kiapi::common::types::document_specifier::Identifier::BoardFilename(
                                    board_for_mock.to_string_lossy().to_string(),
                                ),
                            ),
                        }],
                        },
                        "kiapi.common.commands.GetOpenDocumentsResponse",
                    ))
                } else if command.type_url.ends_with("GetItems") {
                    let fp = footprint.lock().unwrap().clone();
                    Some(copperpour_ipc::builders::pack_any(
                        &kiapi::common::commands::GetItemsResponse {
                            header: None,
                            status: kiapi::common::types::ItemRequestStatus::IrsOk as i32,
                            items: vec![copperpour_ipc::builders::pack_any(
                                &fp,
                                "kiapi.board.types.FootprintInstance",
                            )],
                        },
                        "kiapi.common.commands.GetItemsResponse",
                    ))
                } else if command.type_url.ends_with("BeginCommit") {
                    Some(copperpour_ipc::builders::pack_any(
                        &kiapi::common::commands::BeginCommitResponse {
                            id: Some(kiapi::common::types::Kiid {
                                value: "flip-commit".to_string(),
                            }),
                        },
                        "kiapi.common.commands.BeginCommitResponse",
                    ))
                } else if command.type_url.ends_with("EndCommit") {
                    Some(copperpour_ipc::builders::pack_any(
                        &kiapi::common::commands::EndCommitResponse {},
                        "kiapi.common.commands.EndCommitResponse",
                    ))
                } else if command.type_url.ends_with("GetVersion") {
                    Some(copperpour_ipc::builders::pack_any(
                        &kiapi::common::commands::GetVersionResponse {
                            version: Some(kiapi::common::types::KiCadVersion {
                                major: 10,
                                minor: 0,
                                patch: 5,
                                full_version: "10.0.5".to_string(),
                            }),
                        },
                        "kiapi.common.commands.GetVersionResponse",
                    ))
                } else if command.type_url.ends_with("kiapi.board.commands.FlipItems") {
                    // AS_UNHANDLED: 10.0.5 predates the native FlipItems handler.
                    return kiapi::common::ApiResponse {
                        status: Some(kiapi::common::ApiResponseStatus {
                            status: kiapi::common::ApiStatusCode::AsUnhandled as i32,
                            error_message: "FlipItems is not handled".to_string(),
                        }),
                        header: None,
                        message: None,
                    };
                } else {
                    None
                };
                kiapi::common::ApiResponse {
                    status: Some(kiapi::common::ApiResponseStatus {
                        status: kiapi::common::ApiStatusCode::AsOk as i32,
                        error_message: String::new(),
                    }),
                    header: None,
                    message: body,
                }
            });
        let ctx = crate::tools::pcb_board::board_mock::ctx_talking_to(server.address().to_string());

        let result = handle_flip_component(
            &json!({"board": board, "reference": "U1", "layer": "B.Cu"}),
            &ctx,
        )
        .await
        .unwrap();

        assert!(
            result.is_error,
            "AS_UNHANDLED must not be reported as success"
        );
        assert_eq!(
            crate::mcp::error::extract_error_kind(&result).as_deref(),
            Some("unsupported_capability")
        );
        let parsed: serde_json::Value =
            serde_json::from_str(&result_text(&result)).expect("json error body");
        assert_eq!(parsed["error"]["kicad_version"], "10.0.5");
        assert_eq!(std::fs::read_to_string(&board).unwrap(), before);
    }

    #[tokio::test]
    async fn flip_over_ipc_reports_uncertain_outcome_when_readback_disagrees() {
        use copperpour_ipc::gen::kiapi;

        let tmp = tempfile::tempdir().unwrap();
        let board = tmp.path().join("flip.kicad_pcb");
        std::fs::write(&board, "(kicad_pcb\n  (version 20260206)\n)\n").unwrap();
        let before = std::fs::read_to_string(&board).unwrap();

        let footprint = std::sync::Arc::new(std::sync::Mutex::new(flip_test_footprint(
            "U1-kiid",
            kiapi::board::types::BoardLayer::BlFCu,
        )));
        // The mutation response claims success and a new layer, but the mock
        // never actually updates `held` — so the fresh readback GetItems that
        // follows still reports F.Cu. This is exactly the divergence the
        // readback guard exists to catch (issue #604 acceptance criteria).
        let server = spawn_flip_capable_kicad(&board, footprint, |_flip, held| {
            let mut echoed = held.clone();
            echoed.layer = kiapi::board::types::BoardLayer::BlBCu as i32;
            let item =
                copperpour_ipc::builders::pack_any(&echoed, "kiapi.board.types.FootprintInstance");
            kiapi::board::commands::FlipItemsResponse {
                header: None,
                status: kiapi::common::types::ItemRequestStatus::IrsOk as i32,
                flipped_items: vec![kiapi::board::commands::ItemFlipResult {
                    status: Some(kiapi::common::commands::ItemStatus {
                        code: kiapi::common::commands::ItemStatusCode::IscOk as i32,
                        error_message: String::new(),
                    }),
                    item: Some(item),
                }],
            }
        });
        let ctx = crate::tools::pcb_board::board_mock::ctx_talking_to(server.address().to_string());

        let result = handle_flip_component(
            &json!({"board": board, "reference": "U1", "layer": "B.Cu"}),
            &ctx,
        )
        .await
        .unwrap();

        assert!(result.is_error, "an unconfirmed mutation is not a success");
        assert_eq!(
            crate::mcp::error::extract_error_kind(&result).as_deref(),
            Some("mutation_outcome_uncertain")
        );
        assert_eq!(std::fs::read_to_string(&board).unwrap(), before);
    }

    #[tokio::test]
    async fn flip_refuses_the_exact_open_board_without_touching_the_file() {
        let tmp = tempfile::tempdir().unwrap();
        let board = tmp.path().join("flip.kicad_pcb");
        let before = format!(
            "(kicad_pcb\n  (version 20260206)\n  (generator \"pcbnew\")\n  (net 0 \"\")\n{}\n)\n",
            FLIP_FOOTPRINT
                .lines()
                .map(|line| format!("  {line}"))
                .collect::<Vec<_>>()
                .join("\n")
        );
        std::fs::write(&board, &before).unwrap();
        let server =
            crate::tools::pcb_board::board_mock::spawn_kicad_holding_board(&board, |_| None);
        let ctx = crate::tools::pcb_board::board_mock::ctx_talking_to(server.address().to_string());

        let result = handle_flip_component(
            &json!({"board": board, "reference": "U1", "layer": "B.Cu"}),
            &ctx,
        )
        .await
        .unwrap();

        assert!(result.is_error);
        assert!(result_text(&result).contains("footprint flip"));
        assert_eq!(std::fs::read_to_string(&board).unwrap(), before);
    }

    #[tokio::test]
    async fn flip_proceeds_when_kicad_holds_a_different_board() {
        let tmp = tempfile::tempdir().unwrap();
        let board = tmp.path().join("flip.kicad_pcb");
        let other = tmp.path().join("other.kicad_pcb");
        let before = format!(
            "(kicad_pcb\n  (version 20260206)\n  (generator \"pcbnew\")\n  (net 0 \"\")\n{}\n)\n",
            FLIP_FOOTPRINT
                .lines()
                .map(|line| format!("  {line}"))
                .collect::<Vec<_>>()
                .join("\n")
        );
        std::fs::write(&board, &before).unwrap();
        std::fs::write(&other, "").unwrap();
        let server =
            crate::tools::pcb_board::board_mock::spawn_kicad_holding_board(&other, |_| None);
        let ctx = crate::tools::pcb_board::board_mock::ctx_talking_to(server.address().to_string());

        let result = handle_flip_component(
            &json!({"board": board, "reference": "U1", "layer": "B.Cu"}),
            &ctx,
        )
        .await
        .unwrap();

        assert!(!result.is_error, "{:?}", result.content);
        assert_ne!(std::fs::read_to_string(&board).unwrap(), before);
    }

    #[test]
    fn flip_refuses_custom_pad_geometry_instead_of_corrupting_it() {
        let custom = FLIP_FOOTPRINT.replace("roundrect (at 2 3 50)", "custom (at 2 3 50)");

        let error = flip_footprint_block(&custom, two_layers()).unwrap_err();

        assert!(error.to_string().contains("custom pads"));
    }

    #[test]
    fn flip_refuses_nested_drill_offsets_instead_of_mirroring_only_part_of_the_padstack() {
        let offset_drill = FLIP_FOOTPRINT.replace(
            "(roundrect_rratio 0.25)",
            "(roundrect_rratio 0.25)\n    (drill oval 0.4 0.8 (offset 0.2 0.1))",
        );

        let error = flip_footprint_block(&offset_drill, two_layers()).unwrap_err();

        assert!(error.to_string().contains("offset"), "{error}");
    }

    #[test]
    fn flip_does_not_mirror_text_on_a_non_side_specific_layer() {
        let user_text = FLIP_FOOTPRINT.replace(
            "(layer \"F.SilkS\")\n    (effects (font (size 1 1)) (justify left))",
            "(layer \"User.Drawings\")\n    (effects (font (size 1 1)) (justify left))",
        );

        let flipped = flip_footprint_block(&user_text, two_layers()).unwrap();

        assert!(flipped.contains("(layer \"User.Drawings\")"), "{flipped}");
        assert!(flipped.contains("(justify left)"), "{flipped}");
        assert!(!flipped.contains("(justify left mirror)"), "{flipped}");
    }

    #[test]
    fn supported_footprint_round_trip_restores_the_original_semantics() {
        let no_justify = FLIP_FOOTPRINT.replace(" (justify left)", "");
        let back = flip_footprint_block(&no_justify, two_layers()).unwrap();
        let front = flip_footprint_block(&back, two_layers()).unwrap();

        assert_eq!(
            copperpour_sexp::parse_sexp(&front).unwrap(),
            copperpour_sexp::parse_sexp(&no_justify).unwrap()
        );
    }

    #[test]
    fn non_cardinal_root_orientation_round_trips_without_drift() {
        let non_cardinal = FLIP_FOOTPRINT.replace("(at 10 20 30)", "(at 10 20 37.5)");

        let back = flip_footprint_block(&non_cardinal, two_layers()).unwrap();
        assert!(back.contains("(at 10 20 -37.5)"), "{back}");
        let front = flip_footprint_block(&back, two_layers()).unwrap();

        assert_eq!(
            copperpour_sexp::parse_sexp(&front).unwrap(),
            copperpour_sexp::parse_sexp(&non_cardinal).unwrap()
        );
    }

    #[test]
    fn through_hole_pad_layers_survive_a_flip_round_trip() {
        let through_hole = FLIP_FOOTPRINT.replace(
            "(pad \"1\" smd roundrect (at 2 3 50) (size 1 2)\n    \
             (layers \"F.Cu\" \"F.Paste\" \"F.Mask\")\n    (roundrect_rratio 0.25)",
            "(pad \"1\" thru_hole oval (at 2 3 50) (size 1 2)\n    \
             (drill oval 0.4 0.8)\n    (layers \"*.Cu\" \"*.Mask\")",
        );

        let back = flip_footprint_block(&through_hole, two_layers()).unwrap();
        assert!(back.contains("(layers \"*.Cu\" \"*.Mask\")"), "{back}");
        assert!(back.contains("(drill oval 0.4 0.8)"), "{back}");
        let front = flip_footprint_block(&back, two_layers()).unwrap();

        assert_eq!(
            copperpour_sexp::parse_sexp(&front).unwrap(),
            copperpour_sexp::parse_sexp(&through_hole).unwrap()
        );
    }

    #[test]
    fn hidden_property_stays_hidden_when_flipped() {
        let hidden = FLIP_FOOTPRINT.replace(
            "(effects (font (size 1 1)) (justify left))",
            "(effects (font (size 1 1)) (justify left))\n    (hide yes)",
        );

        let flipped = flip_footprint_block(&hidden, two_layers()).unwrap();

        assert!(flipped.contains("(hide yes)"), "{flipped}");
        assert!(flipped.contains("(justify left mirror)"), "{flipped}");
    }

    #[test]
    fn legacy_fp_text_reference_flips_and_round_trips() {
        let legacy = FLIP_FOOTPRINT.replace(
            "(property \"Reference\" \"U1\"",
            "(fp_text reference \"U1\"",
        );

        let back = flip_footprint_block(&legacy, two_layers()).unwrap();
        assert!(back.contains("(fp_text reference \"U1\""), "{back}");
        let front = flip_footprint_block(&back, two_layers()).unwrap();

        assert_eq!(
            copperpour_sexp::parse_sexp(&front).unwrap(),
            copperpour_sexp::parse_sexp(&legacy).unwrap()
        );
    }

    #[tokio::test]
    async fn invalid_flip_layer_is_structured_and_leaves_the_board_untouched() {
        let tmp = tempfile::tempdir().unwrap();
        let board = tmp.path().join("invalid-layer.kicad_pcb");
        let before = flip_board(&[FLIP_FOOTPRINT], "\n");
        std::fs::write(&board, &before).unwrap();

        let result = handle_flip_component(
            &json!({
                "board": board.to_string_lossy(),
                "reference": "U1",
                "layer": "In1.Cu",
            }),
            &test_ctx(),
        )
        .await
        .unwrap();

        assert!(result.is_error);
        assert_eq!(
            crate::mcp::error::extract_error_kind(&result).as_deref(),
            Some("invalid_argument")
        );
        assert_eq!(std::fs::read_to_string(board).unwrap(), before);
    }

    #[tokio::test]
    async fn missing_and_duplicate_flip_references_leave_the_board_untouched() {
        let tmp = tempfile::tempdir().unwrap();
        let missing_board = tmp.path().join("missing.kicad_pcb");
        let missing_before = flip_board(&[FLIP_FOOTPRINT], "\n");
        std::fs::write(&missing_board, &missing_before).unwrap();

        let missing = handle_flip_component(
            &json!({
                "board": missing_board.to_string_lossy(),
                "reference": "U404",
                "layer": "B.Cu",
            }),
            &test_ctx(),
        )
        .await
        .unwrap();
        assert!(missing.is_error);
        assert_eq!(
            std::fs::read_to_string(&missing_board).unwrap(),
            missing_before
        );

        let duplicate_board = tmp.path().join("duplicate.kicad_pcb");
        let duplicate_before = flip_board(&[FLIP_FOOTPRINT, FLIP_FOOTPRINT], "\n");
        std::fs::write(&duplicate_board, &duplicate_before).unwrap();
        let duplicate = handle_flip_component(
            &json!({
                "board": duplicate_board.to_string_lossy(),
                "reference": "U1",
                "layer": "B.Cu",
            }),
            &test_ctx(),
        )
        .await
        .unwrap();
        // A structured refusal naming the offending field, not a bubbled
        // `anyhow` the caller has to read prose out of.
        assert!(duplicate.is_error);
        let text = result_text(&duplicate);
        assert!(text.contains("invalid_argument"), "{text}");
        assert!(text.contains("\"field\":\"reference\""), "{text}");
        assert!(text.contains("more than once"), "{text}");
        assert_eq!(
            std::fs::read_to_string(duplicate_board).unwrap(),
            duplicate_before
        );
    }

    /// The board KiCad saved with two `REF**` mounting holes, and their
    /// UUIDs, which KiCad's API reports as their KIIDs; the first is the one
    /// in `board_only_shared_reference.ipc.bin`.
    const SHARED_REFERENCE_BOARD: &str = include_str!(
        "../../../copperpour-ipc/tests/fixtures/board_only_shared_reference.kicad_pcb"
    );
    pub(super) const SHARED_FIRST_UUID: &str = "5445eb98-9048-4c1b-8705-b478ce789fc4";
    pub(super) const SHARED_SECOND_UUID: &str = "df6ca34f-ba1b-40d0-939c-98e09a9466bc";

    /// With KiCad closed, a shared reference is refused as `ambiguous_target`
    /// naming both UUIDs, a stale `kiid` as `stale_target`, and `kiid` flips
    /// that one footprint alone (#880).
    #[tokio::test]
    async fn closed_board_flip_refuses_a_shared_reference_and_flips_one_by_kiid() {
        let tmp = tempfile::tempdir().unwrap();
        let board = tmp.path().join("shared-reference.kicad_pcb");
        std::fs::write(&board, SHARED_REFERENCE_BOARD).unwrap();
        let flip = |kiid: Option<&str>| {
            let mut args = json!({
                "board": board.to_string_lossy(),
                "reference": "REF**",
                "layer": "B.Cu",
            });
            if let Some(kiid) = kiid {
                args["kiid"] = json!(kiid);
            }
            args
        };
        let layer_of = |board: &str, uuid: &str| {
            copperpour_sexp::parse_sexp(board)
                .unwrap()
                .find_all("footprint")
                .into_iter()
                .find(|footprint| footprint.find_str("uuid") == Some(uuid))
                .and_then(|footprint| footprint.find_str("layer"))
                .unwrap()
                .to_string()
        };

        let ambiguous = handle_flip_component(&flip(None), &test_ctx())
            .await
            .unwrap();
        let body: serde_json::Value = serde_json::from_str(&result_text(&ambiguous)).unwrap();
        assert_eq!(body["error"]["kind"], "ambiguous_target", "{body}");
        assert_eq!(
            body["error"]["candidates"],
            json!([SHARED_FIRST_UUID, SHARED_SECOND_UUID]),
            "{body}"
        );
        assert!(result_text(&ambiguous).contains("pass kiid"), "{body}");

        let stale = handle_flip_component(&flip(Some("not-on-the-board")), &test_ctx())
            .await
            .unwrap();
        assert_eq!(
            crate::mcp::error::extract_error_kind(&stale).as_deref(),
            Some("stale_target"),
            "{}",
            result_text(&stale)
        );
        let mut renamed = flip(Some(SHARED_SECOND_UUID));
        renamed["reference"] = json!("MH1");
        let renamed = handle_flip_component(&renamed, &test_ctx()).await.unwrap();
        assert_eq!(
            crate::mcp::error::extract_error_kind(&renamed).as_deref(),
            Some("stale_target"),
            "{}",
            result_text(&renamed)
        );
        assert!(
            result_text(&renamed).contains("REF**"),
            "{}",
            result_text(&renamed)
        );
        assert_eq!(
            std::fs::read_to_string(&board).unwrap(),
            SHARED_REFERENCE_BOARD
        );

        let pinned = handle_flip_component(&flip(Some(SHARED_SECOND_UUID)), &test_ctx())
            .await
            .unwrap();
        assert!(!pinned.is_error, "{}", result_text(&pinned));
        let written = std::fs::read_to_string(&board).unwrap();
        assert_eq!(layer_of(&written, SHARED_SECOND_UUID), "B.Cu");
        assert_eq!(layer_of(&written, SHARED_FIRST_UUID), "F.Cu");
    }

    /// KiCad reads a footprint's `tstamp` as its KIID, so `kiid` finds it in
    /// an older file too. KiCad 10 writes `uuid`, so the board is varied
    /// inline to the older form.
    #[test]
    fn closed_board_flip_reads_a_footprints_tstamp_as_its_kiid() {
        let older = SHARED_REFERENCE_BOARD.replace(
            &format!("(uuid \"{SHARED_SECOND_UUID}\")"),
            &format!("(tstamp \"{SHARED_SECOND_UUID}\")"),
        );
        assert_ne!(older, SHARED_REFERENCE_BOARD);
        let (flipped, changed) =
            prepare_closed_board_footprint_side(&older, "REF**", Some(SHARED_SECOND_UUID), "B.Cu")
                .unwrap();
        assert!(changed);
        let layers: Vec<_> = copperpour_sexp::parse_sexp(&flipped)
            .unwrap()
            .find_all("footprint")
            .into_iter()
            .map(|footprint| footprint.find_str("layer").unwrap().to_string())
            .collect();
        assert_eq!(layers, ["F.Cu", "B.Cu"]);
    }

    /// Two footprints with one UUID, which KiCad never writes, leave `kiid`
    /// nothing to choose by: the board is refused as malformed. The board is
    /// varied inline for it.
    #[test]
    fn closed_board_flip_refuses_a_kiid_two_footprints_carry() {
        let twice = SHARED_REFERENCE_BOARD.replace(SHARED_SECOND_UUID, SHARED_FIRST_UUID);
        assert!(matches!(
            prepare_closed_board_footprint_side(&twice, "REF**", Some(SHARED_FIRST_UUID), "B.Cu"),
            Err(ClosedBoardError::Unusable(reason)) if reason.contains("2 footprints")
        ));
    }

    /// `kiid` must be a non-empty string, on flip and find alike.
    #[tokio::test]
    async fn flip_and_find_refuse_a_kiid_that_is_not_a_non_empty_string() {
        for kiid in [json!(5), json!(" ")] {
            let flip = handle_flip_component(
                &json!({"board": "x.kicad_pcb", "reference": "U1", "layer": "B.Cu", "kiid": kiid}),
                &test_ctx(),
            )
            .await
            .unwrap();
            let find = handle_find_component(
                &json!({"board": "x.kicad_pcb", "reference": "U1", "kiid": kiid}),
                &test_ctx(),
            )
            .await
            .unwrap();
            for result in [flip, find] {
                assert_eq!(
                    crate::mcp::error::extract_error_kind(&result).as_deref(),
                    Some("invalid_argument"),
                    "{kiid}: {}",
                    result_text(&result)
                );
                assert!(result_text(&result).contains("kiid"), "{kiid}");
            }
        }
    }

    #[tokio::test]
    async fn closed_board_flip_preserves_crlf_line_endings() {
        let tmp = tempfile::tempdir().unwrap();
        let board = tmp.path().join("crlf.kicad_pcb");
        let before = flip_board(&[FLIP_FOOTPRINT], "\r\n");
        std::fs::write(&board, &before).unwrap();

        let result = handle_flip_component(
            &json!({
                "board": board.to_string_lossy(),
                "reference": "U1",
                "layer": "B.Cu",
            }),
            &test_ctx(),
        )
        .await
        .unwrap();
        assert!(!result.is_error, "{:?}", result.content);

        let written = std::fs::read_to_string(board).unwrap();
        assert_eq!(
            written
                .match_indices('\n')
                .filter(|(index, _)| *index == 0 || written.as_bytes()[index - 1] != b'\r')
                .count(),
            0,
            "{written:?}"
        );
    }

    #[test]
    fn stale_closed_board_flip_is_rejected_without_overwriting_newer_content() {
        let tmp = tempfile::tempdir().unwrap();
        let board = tmp.path().join("stale-flip.kicad_pcb");
        let expected = flip_board(&[FLIP_FOOTPRINT], "\n");
        let (replacement, changed) =
            prepare_closed_board_footprint_side(&expected, "U1", None, "B.Cu").unwrap();
        assert!(changed);
        let newer = expected.replace("(net 0 \"\")", "(net 0 \"\")\n  (net 1 \"GND\")");
        std::fs::write(&board, &newer).unwrap();

        let error = persist_board_replacement(&board, &expected, &replacement)
            .expect_err("a stale flip source must conflict");

        assert!(matches!(error, copperpour_sexp::SexpError::Conflict { .. }));
        assert_eq!(std::fs::read_to_string(board).unwrap(), newer);
    }

    #[tokio::test]
    async fn unsupported_flip_geometry_returns_zero_writes() {
        let tmp = tempfile::tempdir().unwrap();
        let board = tmp.path().join("unsupported.kicad_pcb");
        let custom = FLIP_FOOTPRINT.replace("roundrect (at 2 3 50)", "custom (at 2 3 50)");
        let before = flip_board(&[&custom], "\n");
        std::fs::write(&board, &before).unwrap();

        let refusal = handle_flip_component(
            &json!({
                "board": board.to_string_lossy(),
                "reference": "U1",
                "layer": "B.Cu",
            }),
            &test_ctx(),
        )
        .await
        .unwrap();

        assert!(
            refusal.is_error,
            "unsupported pad geometry must fail closed"
        );
        let text = result_text(&refusal);
        assert!(text.contains("custom pads"), "{text}");
        assert!(text.contains("not modified"), "{text}");
        assert_eq!(std::fs::read_to_string(board).unwrap(), before);
    }

    const INNER_FRONT: &str = include_str!("../../tests/fixtures/inner_layer_flip_front.kicad_pcb");
    const INNER_BACK: &str = include_str!("../../tests/fixtures/inner_layer_flip_back.kicad_pcb");
    const INNER_MISSING: &str =
        include_str!("../../tests/fixtures/inner_layer_flip_missing.kicad_pcb");

    /// Every `(layer …)`/`(layers …)` in footprint `reference`, keyed by the
    /// uuid of the item that carries it.
    fn layers_by_uuid(board: &str, reference: &str) -> BTreeMap<String, Vec<String>> {
        fn walk(node: &copperpour_sexp::SexpNode, out: &mut BTreeMap<String, Vec<String>>) {
            let children = node.children().unwrap_or_default();
            let uuid = node.find("uuid").and_then(|uuid| uuid.get(1)?.as_str());
            for child in children {
                if matches!(child.head(), Some("layer" | "layers")) {
                    if let Some(uuid) = uuid {
                        out.entry(uuid.to_string()).or_default().extend(
                            child.children().unwrap()[1..]
                                .iter()
                                .filter_map(|name| name.as_str().map(str::to_string)),
                        );
                    }
                } else {
                    walk(child, out);
                }
            }
        }
        let root = copperpour_sexp::parse_sexp(board).unwrap();
        let footprint = root
            .find_all("footprint")
            .into_iter()
            .find(|footprint| footprint_reference(footprint).as_deref() == Some(reference))
            .unwrap();
        let mut out = BTreeMap::new();
        walk(footprint, &mut out);
        out
    }

    fn footprint_text<'a>(board: &'a str, reference: &str) -> &'a str {
        direct_children_with_tags(board, "kicad_pcb")
            .unwrap()
            .into_iter()
            .filter(|(_, _, tag)| tag == "footprint")
            .map(|(start, end, _)| &board[start..end])
            .find(|block| {
                footprint_reference(&copperpour_sexp::parse_sexp(block).unwrap()).as_deref()
                    == Some(reference)
            })
            .unwrap()
    }

    async fn served_flip(board: &Path, layer: &str) -> (bool, String) {
        let handler = crate::mcp::handler::McpHandler::new(crate::tools::ServerConfig {
            eager_toolsets: true,
            ..Default::default()
        })
        .await
        .unwrap();
        let response = handler
            .handle_message(json!({
                "jsonrpc": "2.0",
                "id": 831,
                "method": "tools/call",
                "params": {
                    "name": "flip_component",
                    "arguments": { "board": board.to_string_lossy(), "reference": "NT1", "layer": layer }
                }
            }))
            .await
            .unwrap();
        let result = response.result.unwrap();
        (
            result["isError"] == json!(true),
            result["content"][0]["text"].as_str().unwrap().to_string(),
        )
    }

    /// #831: on eight copper layers an In2.Cu pad lands on In5.Cu, as KiCad's
    /// own flip of the same board puts it, and flipping back restores In2.Cu.
    #[tokio::test]
    async fn flip_mirrors_inner_copper_through_the_board_stack() {
        let (front, kicad_back) = (INNER_FRONT, INNER_BACK);
        let tmp = tempfile::tempdir().unwrap();
        let board = tmp.path().join("inner.kicad_pcb");
        std::fs::write(&board, front).unwrap();

        let (is_error, text) = served_flip(&board, "B.Cu").await;
        assert!(!is_error, "{text}");
        let back = std::fs::read_to_string(&board).unwrap();
        let flipped = layers_by_uuid(&back, "NT1");
        assert_eq!(flipped, layers_by_uuid(kicad_back, "NT1"));
        assert_eq!(
            flipped
                .values()
                .flatten()
                .filter(|l| *l == "In5.Cu")
                .count(),
            4,
            "the copper fill and three pads move to In5.Cu"
        );
        assert_eq!(footprint_text(&back, "R1"), footprint_text(front, "R1"));

        let (is_error, text) = served_flip(&board, "F.Cu").await;
        assert!(!is_error, "{text}");
        // The tree, not the bytes: removing `justify` leaves a stray space.
        assert_eq!(
            copperpour_sexp::parse_sexp(&std::fs::read_to_string(&board).unwrap()).unwrap(),
            copperpour_sexp::parse_sexp(front).unwrap()
        );
    }

    /// KiCad keeps an In5.Cu pad when a board drops to four copper layers.
    /// Its own flip would clamp that pad to In1.Cu; this one refuses.
    #[tokio::test]
    async fn flip_refuses_an_inner_layer_the_board_lacks() {
        let before = INNER_MISSING;
        let tmp = tempfile::tempdir().unwrap();
        let board = tmp.path().join("missing.kicad_pcb");
        std::fs::write(&board, before).unwrap();

        let (is_error, text) = served_flip(&board, "F.Cu").await;

        assert!(is_error, "{text}");
        let body: serde_json::Value = serde_json::from_str(&text).unwrap();
        assert_eq!(body["error"]["kind"], "plan_blocked", "{body:#}");
        assert_eq!(body["error"]["operation"], "flip_component");
        let reason = body["error"]["reasons"][0].as_str().unwrap();
        assert!(
            reason.contains("NT1") && reason.contains("In5.Cu") && reason.contains("4-copper"),
            "{reason}"
        );
        assert_eq!(std::fs::read_to_string(board).unwrap(), before);
    }

    /// KiCad's flip moves a custom padstack's In2.Cu entry to In5.Cu; this
    /// path does not rewrite padstacks, so it refuses.
    #[tokio::test]
    async fn flip_refuses_a_per_layer_padstack() {
        let before = include_str!("../../tests/fixtures/inner_layer_flip_padstack.kicad_pcb");
        let tmp = tempfile::tempdir().unwrap();
        let board = tmp.path().join("padstack.kicad_pcb");
        std::fs::write(&board, before).unwrap();

        let result = handle_flip_component(
            &json!({ "board": board.to_string_lossy(), "reference": "J1", "layer": "B.Cu" }),
            &test_ctx(),
        )
        .await
        .unwrap();

        assert!(result.is_error);
        assert!(result_text(&result).contains("per-layer padstacks"));
        assert_eq!(std::fs::read_to_string(board).unwrap(), before);
    }

    #[test]
    fn flip_refuses_an_irregular_or_repeated_layer_table() {
        let irregular = flip_board(&[FLIP_FOOTPRINT], "\n").replace(
            "(net 0 \"\")",
            "(layers (0 \"F.Cu\" signal) (6 \"In2.Cu\" signal) (2 \"B.Cu\" signal))\n  (net 0 \"\")",
        );
        let error =
            prepare_closed_board_footprint_side(&irregular, "U1", None, "B.Cu").unwrap_err();
        let result = error.into_result(Path::new("board.kicad_pcb")).unwrap();
        assert_eq!(
            crate::mcp::error::extract_error_kind(&result).as_deref(),
            Some("plan_blocked")
        );
        assert!(
            result_text(&result).contains("In2.Cu"),
            "{}",
            result_text(&result)
        );

        let repeated = flip_board(&[FLIP_FOOTPRINT], "\n").replace(
            "(net 0 \"\")",
            "(layers (0 \"F.Cu\" signal) (2 \"B.Cu\" signal))\n  \
             (layers (0 \"F.Cu\" signal) (2 \"B.Cu\" signal))\n  (net 0 \"\")",
        );
        assert!(matches!(
            prepare_closed_board_footprint_side(&repeated, "U1", None, "B.Cu"),
            Err(ClosedBoardError::Unusable(reason)) if reason.contains("more than one")
        ));
    }

    #[test]
    fn stale_board_source_is_rejected_without_overwriting_newer_content() {
        let tmp = tempfile::tempdir().unwrap();
        let board = tmp.path().join("board.kicad_pcb");
        std::fs::write(&board, EMPTY_BOARD).unwrap();
        let replacement = EMPTY_BOARD.replace(
            "\n)\n",
            "\n\t(footprint \"Legacy:Socket\" (layer \"F.Cu\"))\n)\n",
        );
        let newer = EMPTY_BOARD.replace("(net 0 \"\")", "(net 0 \"\")\n\t(net 1 \"GND\")");
        std::fs::write(&board, &newer).unwrap();

        let error = persist_board_replacement(&board, EMPTY_BOARD, &replacement)
            .expect_err("a stale expected board must conflict");

        assert!(matches!(error, copperpour_sexp::SexpError::Conflict { .. }));
        assert_eq!(std::fs::read_to_string(board).unwrap(), newer);
    }

    #[tokio::test]
    async fn stale_closed_board_move_is_rejected_without_overwriting_newer_content() {
        let tmp = tempfile::tempdir().unwrap();
        let board = placed_fallback_fixture(tmp.path()).await;
        let expected = std::fs::read_to_string(&board).unwrap();
        let replacement = prepare_closed_board_footprint_update(
            &expected,
            "R1",
            FootprintPlacementUpdate::Move { x: 40.0, y: 50.0 },
        )
        .unwrap();
        let newer = expected.replace("(net 0 \"\")", "(net 0 \"\")\n\t(net 1 \"GND\")");
        std::fs::write(&board, &newer).unwrap();

        let error = persist_board_replacement(&board, &expected, &replacement)
            .expect_err("a stale move source must conflict");

        assert!(matches!(error, copperpour_sexp::SexpError::Conflict { .. }));
        assert_eq!(std::fs::read_to_string(board).unwrap(), newer);
    }

    #[tokio::test]
    async fn fallback_placement_rotation_reaches_the_pads() {
        // A rotated placement whose pads keep angle 0 trips KiCad's own
        // lib_footprint_mismatch check, so the rotation has to reach them.
        let tmp = tempfile::tempdir().unwrap();
        let board = fallback_fixture(tmp.path());
        let args = json!({
            "board": board.to_string_lossy(),
            "footprint": "Resistor_SMD:R_0805_2012Metric",
            "reference": "R1", "x": 10.0, "y": 20.0, "rotation": -90.0,
        });
        let res = handle_place_component(&args, &test_ctx()).await.unwrap();
        assert!(!res.is_error, "{:?}", res.content);

        let out = std::fs::read_to_string(&board).unwrap();
        assert!(out.contains("(at 10 20 -90)"), "footprint angle:\n{out}");
        assert!(out.contains("(at -0.9125 0 270)"), "pad angle:\n{out}");
        assert!(
            out.contains("(at 0 -1.65 90)"),
            "readable text angle:\n{out}"
        );
    }

    #[tokio::test]
    async fn a_truncated_board_is_refused_rather_than_rewritten() {
        // rfind(')') picks the insert point, so a board that is not one closed
        // (kicad_pcb …) form would silently gain a footprint outside the root
        // expression. Nothing should be written in that case.
        let tmp = tempfile::tempdir().unwrap();
        let board = fallback_fixture(tmp.path());
        let truncated = "(kicad_pcb (version 20241229) (generator \"test\")";
        std::fs::write(&board, truncated).unwrap();

        let args = json!({
            "board": board.to_string_lossy(),
            "footprint": "Resistor_SMD:R_0805_2012Metric",
            "reference": "R1", "x": 1.0, "y": 2.0,
        });
        let err = handle_place_component(&args, &test_ctx())
            .await
            .expect_err("a malformed board must not be written back");
        assert!(
            err.to_string().contains("balanced"),
            "error should explain why: {err}"
        );
        assert_eq!(
            std::fs::read_to_string(&board).unwrap(),
            truncated,
            "board must be left exactly as it was"
        );
    }

    /// Force LF, whatever the checkout did to this source file's literals.
    fn lf(s: &str) -> String {
        s.replace("\r\n", "\n")
    }

    /// Force CRLF, likewise.
    fn crlf(s: &str) -> String {
        lf(s).replace('\n', "\r\n")
    }

    #[tokio::test]
    async fn a_crlf_board_stays_crlf() {
        // KiCad writes these files CRLF on Windows, so placing into a CRLF
        // board must not leave two conventions in it.
        let tmp = tempfile::tempdir().unwrap();
        let board = fallback_fixture(tmp.path());
        std::fs::write(&board, crlf(EMPTY_BOARD)).unwrap();

        let args = json!({
            "board": board.to_string_lossy(),
            "footprint": "Resistor_SMD:R_0805_2012Metric",
            "reference": "R1", "x": 1.0, "y": 2.0,
        });
        let res = handle_place_component(&args, &test_ctx()).await.unwrap();
        assert!(!res.is_error, "handler errored: {:?}", res.content);

        let out = std::fs::read_to_string(&board).unwrap();
        assert!(
            out.contains("(pad \"1\" smd roundrect"),
            "footprint missing"
        );
        let bare_lf = out
            .match_indices('\n')
            .filter(|(i, _)| *i == 0 || out.as_bytes()[i - 1] != b'\r')
            .count();
        assert_eq!(
            bare_lf, 0,
            "a CRLF board gained {bare_lf} bare LF line endings:\n{out:?}"
        );
    }

    #[tokio::test]
    async fn an_lf_board_stays_lf() {
        // The reverse: a CRLF library footprint must not drag \r into an LF
        // board, which is the common case on Linux and macOS.
        let tmp = tempfile::tempdir().unwrap();
        let board = fallback_fixture(tmp.path());
        std::fs::write(&board, lf(EMPTY_BOARD)).unwrap();

        let args = json!({
            "board": board.to_string_lossy(),
            "footprint": "Resistor_SMD:R_0805_2012Metric",
            "reference": "R1", "x": 1.0, "y": 2.0,
        });
        let res = handle_place_component(&args, &test_ctx()).await.unwrap();
        assert!(!res.is_error, "handler errored: {:?}", res.content);

        let out = std::fs::read_to_string(&board).unwrap();
        assert!(
            out.contains("(pad \"1\" smd roundrect"),
            "footprint missing"
        );
        assert!(
            !out.contains('\r'),
            "a CRLF library footprint dragged \\r into an LF board:\n{out:?}"
        );
    }

    /// A rep0 endpoint that completes every round-trip with an error status —
    /// a live KiCAD saying no. Placement must fail closed: error out, and
    /// leave the board file alone.
    fn spawn_rejecting_kicad() -> crate::test_support::MockIpcServer {
        crate::test_support::MockIpcServer::spawn("rejecting-component", |_| {
            copperpour_ipc::gen::kiapi::common::ApiResponse {
                status: Some(copperpour_ipc::gen::kiapi::common::ApiResponseStatus {
                    status: copperpour_ipc::gen::kiapi::common::ApiStatusCode::AsBadRequest as i32,
                    error_message: "mock rejects everything".to_string(),
                }),
                header: None,
                message: None,
            }
        })
    }

    #[tokio::test]
    async fn a_reachable_kicad_that_rejects_never_touches_the_file() {
        let tmp = tempfile::tempdir().unwrap();
        let board = fallback_fixture(tmp.path());
        let board_before = std::fs::read_to_string(&board).unwrap();

        let server = spawn_rejecting_kicad();
        let ctx = ToolContext::new(
            crate::tools::ServerConfig {
                kicad_cli: String::new(),
                kicad_binary: String::new(),
                ipc_address: server.address().to_string(),
                project_dir: None,
                jlcpcb_db_path: None,
                auto_load_toolsets: false,
                eager_toolsets: false,
            },
            std::sync::Arc::new(crate::router::ToolRouter::new()),
        );
        let args = json!({
            "board": board.to_string_lossy(),
            "footprint": "Resistor_SMD:R_0805_2012Metric",
            "reference": "R1", "x": 1.0, "y": 2.0,
        });
        let res = handle_place_component(&args, &ctx).await.unwrap();
        assert!(res.is_error, "a rejection must not be reported as success");
        let text = result_text(&res);
        assert!(
            text.contains("rejected the placement") && text.contains("not modified"),
            "the error must say the file was left alone: {text}"
        );
        assert_eq!(
            std::fs::read_to_string(&board).unwrap(),
            board_before,
            "a reachable KiCAD that says no must never trigger the file fallback"
        );
    }

    // ─── Reading pads: live board first, file second ──────────────────────────

    /// A board file whose R1 is stale — the state of the last save.
    const SAVED_BOARD_WITH_R1: &str = "(kicad_pcb\n\
        \t(version 20260206)\n\
        \t(footprint \"R_0805\"\n\
        \t\t(at 5 5 90)\n\
        \t\t(property \"Reference\" \"R1\" (at 0 -1 0) (layer \"F.SilkS\"))\n\
        \t\t(pad \"1\" smd roundrect (at -0.9 0 105) (size 2.432 1.524)\n\
        \t\t\t(roundrect_rratio 0.25) (layers \"F.Cu\" \"F.Paste\" \"F.Mask\")\n\
        \t\t\t(net \"SAVED\") (uuid \"11111111-1111-1111-1111-111111111111\"))\n\
        \t)\n\
        )\n";

    fn live_pad(number: &str, x: f64, y: f64, net: &str) -> prost_types::Any {
        copperpour_ipc::builders::pack_any(
            &copperpour_ipc::gen::kiapi::board::types::Pad {
                id: Some(copperpour_ipc::gen::kiapi::common::types::Kiid {
                    value: "22222222-2222-2222-2222-222222222222".to_string(),
                }),
                number: number.to_string(),
                r#type: copperpour_ipc::gen::kiapi::board::types::PadType::PtSmd as i32,
                position: Some(copperpour_ipc::builders::vec2(x, y)),
                net: Some(copperpour_ipc::gen::kiapi::board::types::Net {
                    code: None,
                    name: net.to_string(),
                }),
                pad_stack: Some(copperpour_ipc::gen::kiapi::board::types::PadStack {
                    layers: vec![
                        copperpour_ipc::gen::kiapi::board::types::BoardLayer::BlFCu as i32,
                        copperpour_ipc::gen::kiapi::board::types::BoardLayer::BlFPaste as i32,
                        copperpour_ipc::gen::kiapi::board::types::BoardLayer::BlFMask as i32,
                    ],
                    copper_layers: vec![copperpour_ipc::gen::kiapi::board::types::PadStackLayer {
                        layer: copperpour_ipc::gen::kiapi::board::types::BoardLayer::BlFCu as i32,
                        shape: copperpour_ipc::gen::kiapi::board::types::PadStackShape::PssRoundrect
                            as i32,
                        size: Some(copperpour_ipc::builders::vec2(2.432, 1.524)),
                        corner_rounding_ratio: 0.25,
                        ..Default::default()
                    }],
                    angle: Some(copperpour_ipc::gen::kiapi::common::types::Angle {
                        value_degrees: 105.0,
                    }),
                    ..Default::default()
                }),
                ..Default::default()
            },
            "kiapi.board.types.Pad",
        )
    }

    fn live_custom_stack_pad(number: &str, x: f64, y: f64) -> prost_types::Any {
        use copperpour_ipc::gen::kiapi::board::types::{
            BoardLayer, DrillProperties, DrillShape, Pad, PadStack, PadStackLayer, PadStackShape,
            PadType,
        };

        let layer = |layer, shape, width, height| PadStackLayer {
            layer,
            shape,
            size: Some(copperpour_ipc::builders::vec2(width, height)),
            ..Default::default()
        };
        copperpour_ipc::builders::pack_any(
            &Pad {
                number: number.to_string(),
                r#type: PadType::PtPth as i32,
                position: Some(copperpour_ipc::builders::vec2(x, y)),
                pad_stack: Some(PadStack {
                    layers: vec![
                        BoardLayer::BlFCu as i32,
                        BoardLayer::BlIn1Cu as i32,
                        BoardLayer::BlBCu as i32,
                    ],
                    copper_layers: vec![
                        layer(
                            BoardLayer::BlFCu as i32,
                            PadStackShape::PssCustom as i32,
                            3.0,
                            2.0,
                        ),
                        layer(
                            BoardLayer::BlIn1Cu as i32,
                            PadStackShape::PssCircle as i32,
                            1.5,
                            1.5,
                        ),
                        layer(
                            BoardLayer::BlBCu as i32,
                            PadStackShape::PssRectangle as i32,
                            2.0,
                            1.0,
                        ),
                    ],
                    drill: Some(DrillProperties {
                        start_layer: BoardLayer::BlFCu as i32,
                        end_layer: BoardLayer::BlBCu as i32,
                        diameter: Some(copperpour_ipc::builders::vec2(0.8, 1.2)),
                        shape: DrillShape::DsOblong as i32,
                        ..Default::default()
                    }),
                    angle: Some(copperpour_ipc::gen::kiapi::common::types::Angle {
                        value_degrees: 37.0,
                    }),
                    ..Default::default()
                }),
                ..Default::default()
            },
            "kiapi.board.types.Pad",
        )
    }

    fn live_footprint(reference: &str, pads: Vec<prost_types::Any>) -> prost_types::Any {
        use copperpour_ipc::gen::kiapi;
        copperpour_ipc::builders::pack_any(
            &kiapi::board::types::FootprintInstance {
                position: Some(copperpour_ipc::builders::vec2(100.0, 100.0)),
                reference_field: Some(kiapi::board::types::Field {
                    name: "Reference".to_string(),
                    text: Some(kiapi::board::types::BoardText {
                        text: Some(kiapi::common::types::Text {
                            text: reference.to_string(),
                            ..Default::default()
                        }),
                        ..Default::default()
                    }),
                    ..Default::default()
                }),
                definition: Some(kiapi::board::types::Footprint {
                    items: pads,
                    ..Default::default()
                }),
                ..Default::default()
            },
            "kiapi.board.types.FootprintInstance",
        )
    }

    /// A rep0 endpoint playing a KiCad that holds `board` open with `items` on
    /// it — a live board carrying edits the file on disk has never seen.
    fn spawn_kicad_holding(
        board: &Path,
        items: Vec<prost_types::Any>,
    ) -> crate::test_support::MockIpcServer {
        use copperpour_ipc::gen::kiapi;

        let board = board.to_string_lossy().to_string();
        crate::test_support::MockIpcServer::spawn("component-items", move |request| {
            let command = request.message.expect("a command");
            let body = if command.type_url.ends_with("GetOpenDocuments") {
                Some(copperpour_ipc::builders::pack_any(
                    &kiapi::common::commands::GetOpenDocumentsResponse {
                        documents: vec![kiapi::common::types::DocumentSpecifier {
                            r#type: kiapi::common::types::DocumentType::DoctypePcb as i32,
                            project: None,
                            identifier: Some(
                                kiapi::common::types::document_specifier::Identifier::BoardFilename(
                                    board.clone(),
                                ),
                            ),
                        }],
                    },
                    "kiapi.common.commands.GetOpenDocumentsResponse",
                ))
            } else if command.type_url.ends_with("GetItems") {
                Some(copperpour_ipc::builders::pack_any(
                    &kiapi::common::commands::GetItemsResponse {
                        header: None,
                        status: kiapi::common::types::ItemRequestStatus::IrsOk as i32,
                        items: items.clone(),
                    },
                    "kiapi.common.commands.GetItemsResponse",
                ))
            } else {
                None
            };
            kiapi::common::ApiResponse {
                status: Some(kiapi::common::ApiResponseStatus {
                    status: kiapi::common::ApiStatusCode::AsOk as i32,
                    error_message: String::new(),
                }),
                header: None,
                message: body,
            }
        })
    }

    fn ctx_talking_to(address: String) -> ToolContext {
        ToolContext::new(
            crate::tools::ServerConfig {
                kicad_cli: String::new(),
                kicad_binary: String::new(),
                ipc_address: address,
                project_dir: None,
                jlcpcb_db_path: None,
                auto_load_toolsets: false,
                eager_toolsets: false,
            },
            std::sync::Arc::new(crate::router::ToolRouter::new()),
        )
    }

    fn parsed(res: &CallToolResult) -> serde_json::Value {
        serde_json::from_str(&result_text(res)).expect("json result")
    }

    #[tokio::test]
    async fn pads_come_from_the_board_kicad_holds_not_the_last_save() {
        let tmp = tempfile::tempdir().unwrap();
        let board = tmp.path().join("b.kicad_pcb");
        std::fs::write(&board, SAVED_BOARD_WITH_R1).unwrap();
        let server = spawn_kicad_holding(
            &board,
            vec![live_footprint(
                "R1",
                vec![live_pad("1", 101.155, 66.11, "/VBUS")],
            )],
        );

        let res = handle_get_component_pads(
            &json!({ "board": board.to_string_lossy(), "reference": "R1" }),
            &ctx_talking_to(server.address().to_string()),
        )
        .await
        .unwrap();

        assert!(!res.is_error, "{:?}", res.content);
        let body = parsed(&res);
        assert_eq!(body["source"], json!("ipc"));
        assert_eq!(body["pads"][0]["net"], json!("/VBUS"));
        assert_eq!(body["pads"][0]["x"], json!(101.155));
        assert_eq!(
            body["pads"][0]["uuid"],
            "22222222-2222-2222-2222-222222222222"
        );
        assert_eq!(body["pads"][0]["pad_type"], "smd");
        assert_eq!(body["pads"][0]["shape"], "roundrect");
        assert_eq!(body["pads"][0]["size"], json!({"x": 2.432, "y": 1.524}));
        assert_eq!(body["pads"][0]["rotation_deg"], 105.0);
        assert_eq!(body["pads"][0]["copper_layers"][0]["layer"], "F.Cu");
        assert_eq!(
            body["pads"][0]["layers"],
            json!(["F.Cu", "F.Paste", "F.Mask"])
        );
    }

    #[tokio::test]
    async fn a_part_deleted_in_kicad_is_not_answered_from_the_file() {
        let tmp = tempfile::tempdir().unwrap();
        let board = tmp.path().join("b.kicad_pcb");
        std::fs::write(&board, SAVED_BOARD_WITH_R1).unwrap();
        let server = spawn_kicad_holding(&board, vec![]);

        let res = handle_get_component_pads(
            &json!({ "board": board.to_string_lossy(), "reference": "R1" }),
            &ctx_talking_to(server.address().to_string()),
        )
        .await
        .unwrap();

        assert!(res.is_error, "the live board no longer has R1");
        assert_eq!(
            crate::mcp::error::extract_error_kind(&res).as_deref(),
            Some("invalid_argument")
        );
        assert!(result_text(&res).contains("open in KiCad"));
    }

    #[tokio::test]
    async fn no_pads_from_kicad_is_refused_when_the_file_has_some() {
        let tmp = tempfile::tempdir().unwrap();
        let board = tmp.path().join("b.kicad_pcb");
        std::fs::write(&board, SAVED_BOARD_WITH_R1).unwrap();
        // The part is there, its pads are not — the shape a response we
        // failed to read would also take.
        let server = spawn_kicad_holding(&board, vec![live_footprint("R1", vec![])]);

        let res = handle_get_component_pads(
            &json!({ "board": board.to_string_lossy(), "reference": "R1" }),
            &ctx_talking_to(server.address().to_string()),
        )
        .await
        .unwrap();

        assert!(res.is_error, "'no pads' must not pass as an answer here");
        assert_eq!(
            crate::mcp::error::extract_error_kind(&res).as_deref(),
            Some("conflict")
        );
        assert!(result_text(&res).contains("no pads"));
    }

    #[tokio::test]
    async fn a_genuinely_pad_less_footprint_reads_as_zero() {
        let tmp = tempfile::tempdir().unwrap();
        let board = tmp.path().join("b.kicad_pcb");
        std::fs::write(&board, SAVED_BOARD_WITH_R1).unwrap();
        // The saved file has no LOGO1 to disagree, so KiCad's answer stands.
        let server = spawn_kicad_holding(&board, vec![live_footprint("LOGO1", vec![])]);

        let res = handle_get_component_pads(
            &json!({ "board": board.to_string_lossy(), "reference": "LOGO1" }),
            &ctx_talking_to(server.address().to_string()),
        )
        .await
        .unwrap();

        assert!(!res.is_error, "{:?}", res.content);
        let body = parsed(&res);
        assert_eq!(body["source"], json!("ipc"));
        assert_eq!(body["pad_count"], json!(0));
    }

    #[tokio::test]
    async fn pads_fall_back_to_the_file_when_kicad_is_unreachable() {
        let tmp = tempfile::tempdir().unwrap();
        let board = tmp.path().join("b.kicad_pcb");
        std::fs::write(&board, SAVED_BOARD_WITH_R1).unwrap();

        let res = handle_get_component_pads(
            &json!({ "board": board.to_string_lossy(), "reference": "R1" }),
            &test_ctx(),
        )
        .await
        .unwrap();

        assert!(!res.is_error, "{:?}", res.content);
        let body = parsed(&res);
        assert_eq!(body["source"], json!("file"));
        assert_eq!(body["pads"][0]["net"], json!("SAVED"));
        assert_eq!(
            body["pads"][0]["uuid"],
            "11111111-1111-1111-1111-111111111111"
        );
        assert_eq!(body["pads"][0]["pad_type"], "smd");
        assert_eq!(body["pads"][0]["shape"], "roundrect");
        assert_eq!(body["pads"][0]["size"], json!({"x": 2.432, "y": 1.524}));
        assert_eq!(body["pads"][0]["rotation_deg"], 105.0);
        assert_eq!(body["pads"][0]["copper_layers"][0]["layer"], "F.Cu");
        assert_eq!(
            body["pads"][0]["layers"],
            json!(["F.Cu", "F.Paste", "F.Mask"])
        );
    }

    #[tokio::test]
    async fn live_and_file_pad_geometry_use_the_same_response_shape() {
        let tmp = tempfile::tempdir().unwrap();
        let board = tmp.path().join("b.kicad_pcb");
        std::fs::write(&board, SAVED_BOARD_WITH_R1).unwrap();
        let server = spawn_kicad_holding(
            &board,
            vec![live_footprint("R1", vec![live_pad("1", 5.0, 5.9, "SAVED")])],
        );
        let args = json!({ "board": board.to_string_lossy(), "reference": "R1" });

        let live = handle_get_component_pads(&args, &ctx_talking_to(server.address().to_string()))
            .await
            .unwrap();
        let file = handle_get_component_pads(&args, &test_ctx()).await.unwrap();
        let live = parsed(&live);
        let file = parsed(&file);
        let live_keys: std::collections::BTreeSet<_> = live["pads"][0]
            .as_object()
            .unwrap()
            .keys()
            .cloned()
            .collect();
        let file_keys: std::collections::BTreeSet<_> = file["pads"][0]
            .as_object()
            .unwrap()
            .keys()
            .cloned()
            .collect();

        assert_eq!(live_keys, file_keys);
    }

    #[tokio::test]
    async fn live_custom_front_inner_back_stack_keeps_each_copper_geometry() {
        let tmp = tempfile::tempdir().unwrap();
        let board = tmp.path().join("b.kicad_pcb");
        std::fs::write(&board, SAVED_BOARD_WITH_R1).unwrap();
        let server = spawn_kicad_holding(
            &board,
            vec![live_footprint(
                "R1",
                vec![live_custom_stack_pad("1", 12.0, 34.0)],
            )],
        );

        let result = handle_get_component_pads(
            &json!({ "board": board.to_string_lossy(), "reference": "R1" }),
            &ctx_talking_to(server.address().to_string()),
        )
        .await
        .unwrap();

        assert!(!result.is_error, "{:?}", result.content);
        let body = parsed(&result);
        assert_eq!(body["pads"][0]["shape"], "custom");
        assert_eq!(body["pads"][0]["rotation_deg"], 37.0);
        assert_eq!(body["pads"][0]["drill"]["shape"], "oval");
        assert_eq!(
            body["pads"][0]["copper_layers"].as_array().unwrap().len(),
            3
        );
        assert_eq!(body["pads"][0]["copper_layers"][0]["layer"], "F.Cu");
        assert_eq!(body["pads"][0]["copper_layers"][0]["shape"], "custom");
        assert_eq!(body["pads"][0]["copper_layers"][1]["layer"], "In1.Cu");
        assert_eq!(body["pads"][0]["copper_layers"][1]["shape"], "circle");
        assert_eq!(body["pads"][0]["copper_layers"][2]["layer"], "B.Cu");
        assert_eq!(body["pads"][0]["copper_layers"][2]["shape"], "rect");
    }

    #[tokio::test]
    async fn saved_back_side_footprint_keeps_board_space_rotation_and_back_layers() {
        let tmp = tempfile::tempdir().unwrap();
        let board = tmp.path().join("back.kicad_pcb");
        std::fs::write(
            &board,
            "(kicad_pcb\n\
             \t(footprint \"BackSide\"\n\
             \t\t(layer \"B.Cu\")\n\
             \t\t(at 20 30 45)\n\
             \t\t(property \"Reference\" \"U1\")\n\
             \t\t(pad \"1\" smd oval (at 2 -1 225) (size 2 1)\n\
             \t\t\t(layers \"B.Cu\" \"B.Paste\" \"B.Mask\"))\n\
             \t)\n\
             )\n",
        )
        .unwrap();

        let result = handle_get_component_pads(
            &json!({ "board": board.to_string_lossy(), "reference": "U1" }),
            &test_ctx(),
        )
        .await
        .unwrap();

        assert!(!result.is_error, "{:?}", result.content);
        let body = parsed(&result);
        assert_eq!(body["pads"][0]["rotation_deg"], 225.0);
        assert_eq!(body["pads"][0]["shape"], "oval");
        assert_eq!(
            body["pads"][0]["layers"],
            json!(["B.Cu", "B.Paste", "B.Mask"])
        );
        assert_eq!(body["pads"][0]["copper_layers"][0]["layer"], "B.Cu");
    }

    #[tokio::test]
    async fn kicad_authored_board_reports_the_saved_effective_pad_rotation() {
        let board =
            Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/drc_ownership_j1.kicad_pcb");

        let result = handle_get_component_pads(
            &json!({ "board": board.to_string_lossy(), "reference": "J1" }),
            &test_ctx(),
        )
        .await
        .unwrap();

        assert!(!result.is_error, "{:?}", result.content);
        let body = parsed(&result);
        assert_eq!(body["source"], "file");
        assert_eq!(body["pad_count"], 4);
        assert_eq!(body["pads"][0]["shape"], "rect");
        assert_eq!(body["pads"][0]["size"], json!({"x": 1.78, "y": 1.02}));
        assert_eq!(body["pads"][0]["rotation_deg"], 90.0);
        assert_eq!(body["pads"][0]["net"], "GND");
    }

    #[tokio::test]
    async fn file_pad_reads_refuse_to_silently_drop_an_unreadable_pad() {
        let tmp = tempfile::tempdir().unwrap();
        let board = tmp.path().join("b.kicad_pcb");
        std::fs::write(
            &board,
            "(kicad_pcb\n\
             \t(footprint \"Malformed\"\n\
             \t\t(at 5 5 0)\n\
             \t\t(property \"Reference\" \"R1\")\n\
             \t\t(pad \"1\" smd rect (size 1 1) (layers \"F.Cu\"))\n\
             \t)\n\
             )\n",
        )
        .unwrap();

        let result = handle_get_component_pads(
            &json!({ "board": board.to_string_lossy(), "reference": "R1" }),
            &test_ctx(),
        )
        .await
        .unwrap();

        assert!(result.is_error);
        assert!(result_text(&result).contains("Pad '1' on footprint 'R1' has no readable position"));
    }

    #[tokio::test]
    async fn pad_reads_do_not_fall_back_when_reachable_kicad_rejects() {
        let tmp = tempfile::tempdir().unwrap();
        let board = tmp.path().join("b.kicad_pcb");
        std::fs::write(&board, SAVED_BOARD_WITH_R1).unwrap();

        let server = spawn_rejecting_kicad();
        let res = handle_get_component_pads(
            &json!({ "board": board.to_string_lossy(), "reference": "R1" }),
            &ctx_talking_to(server.address().to_string()),
        )
        .await
        .unwrap();

        assert!(
            res.is_error,
            "a live rejection must not return stale file pads"
        );
        assert!(result_text(&res).contains("mock rejects everything"));
    }

    #[tokio::test]
    async fn a_pad_position_carries_the_source_of_the_reading() {
        let tmp = tempfile::tempdir().unwrap();
        let board = tmp.path().join("b.kicad_pcb");
        std::fs::write(&board, SAVED_BOARD_WITH_R1).unwrap();
        let server = spawn_kicad_holding(
            &board,
            vec![live_footprint(
                "R1",
                vec![live_pad("1", 101.155, 66.11, "/VBUS")],
            )],
        );

        let res = handle_get_pad_position(
            &json!({ "board": board.to_string_lossy(), "reference": "R1", "pad_number": "1" }),
            &ctx_talking_to(server.address().to_string()),
        )
        .await
        .unwrap();

        let body = parsed(&res);
        assert_eq!(body["source"], json!("ipc"));
        assert_eq!(body["x"], json!(101.155));
    }

    // ─── board_lib_id / helpers (ported from PR #66) ──────────────────────────

    /// `board_lib_id` for a path, with the library file's declared name.
    fn id_for(path: &str, declared: &str) -> String {
        board_lib_id(path, Path::new(path), declared)
    }

    #[test]
    fn board_lib_id_never_yields_a_filesystem_path() {
        // A Library:Footprint id is already what the board wants.
        assert_eq!(
            board_lib_id("Resistor_SMD:R_0805", Path::new("/ignored"), "R_0805"),
            "Resistor_SMD:R_0805"
        );
        // A path in a .pretty library takes the nickname from its directory.
        assert_eq!(
            id_for(
                "/nonexistent/kicad/footprints/Resistor_SMD.pretty/R_0805.kicad_mod",
                "R_0805"
            ),
            "Resistor_SMD:R_0805"
        );
        // Loose file: no nickname to recover, so it keeps the name the library
        // file declares — unlinked, but a valid name rather than a path.
        assert_eq!(
            id_for("/nonexistent/scratch/R_0805.kicad_mod", "R_0805_2012Metric"),
            "R_0805_2012Metric"
        );
    }

    #[test]
    fn a_path_like_declared_name_falls_back_to_the_file_stem() {
        // A malformed library file naming itself with a path must not smuggle
        // that path into the board through the fallback branch.
        assert_eq!(
            id_for(
                "/nonexistent/scratch/R_0805.kicad_mod",
                "/tmp/other/R.kicad_mod"
            ),
            "R_0805"
        );
        assert_eq!(
            id_for("/nonexistent/scratch/R_0805.kicad_mod", r"C:\x\R.kicad_mod"),
            "R_0805"
        );
        // An empty declared name is no better than a path.
        assert_eq!(
            id_for("/nonexistent/scratch/R_0805.kicad_mod", ""),
            "R_0805"
        );
    }

    #[test]
    fn pretty_suffix_matching_ignores_case() {
        // Windows and macOS filesystems are case-insensitive, so Foo.Pretty and
        // Foo.pretty are the same directory to KiCad.
        assert_eq!(
            pretty_dir_nickname(Path::new("/libs/Resistor_SMD.Pretty")),
            Some("Resistor_SMD".into())
        );
        assert_eq!(
            pretty_dir_nickname(Path::new("/libs/Resistor_SMD.pretty")),
            Some("Resistor_SMD".into())
        );
        // A bare ".pretty" leaves no nickname behind.
        assert_eq!(pretty_dir_nickname(Path::new("/libs/.pretty")), None);
        assert_eq!(pretty_dir_nickname(Path::new("/libs/plain")), None);
    }

    #[test]
    fn a_board_edit_must_stay_one_kicad_pcb_form() {
        assert!(check_single_board_form("(kicad_pcb (version 20241229))").is_ok());
        assert!(check_single_board_form("\n  (kicad_pcb (version 1))\n\n").is_ok());

        // Truncated — the bug this guard exists for.
        assert!(check_single_board_form("(kicad_pcb (version 1)").is_err());
        // Leading garbage would otherwise be skipped by find_balanced_block.
        assert!(check_single_board_form("garbage(kicad_pcb (version 1))").is_err());
        // A second form after the root is not one board.
        assert!(check_single_board_form("(kicad_pcb (version 1))(extra)").is_err());
        // Well-formed, but not a board.
        assert!(check_single_board_form("(not_a_board (version 1))").is_err());
    }

    #[test]
    fn pad_angles_absorb_the_footprint_rotation() {
        // KiCad stores each pad's absolute orientation: a footprint placed at
        // -90 carries 270 on its pads, while pad positions stay in unrotated
        // footprint-local coordinates.
        let out = apply_rotation_to_children(&library_footprint(), -90.0);
        assert!(out.contains("(at -0.9125 0 270)"), "{out}");
        // Position is unchanged; only the angle was added.
        assert!(
            !out.contains("(at 0 -0.9125"),
            "pad position must not rotate"
        );
    }

    #[test]
    fn text_angles_are_kept_readable_in_file_fallback() {
        // A -90 footprint would put text at 270, which reads upside down, so
        // KiCad flips it by 180 to 90 — matching what pcbnew writes.
        let out = apply_rotation_to_children(&library_footprint(), -90.0);
        assert!(out.contains("(at 0 -1.65 90)"), "reference text:\n{out}");
        assert!(out.contains("(at 0 1.65 90)"), "value text:\n{out}");
    }

    #[test]
    fn zero_rotation_is_written_without_an_angle() {
        assert_eq!(format_at(1.5, -2.0, 0.0), "(at 1.5 -2)");
        assert_eq!(format_at(0.0, 0.0, 90.0), "(at 0 0 90)");
    }

    #[test]
    fn rotate_at_block_rejects_non_positional_at() {
        assert!(rotate_at_block("(at)", 90.0, false).is_none());
        assert!(rotate_at_block("(atomic 1 2)", 90.0, false).is_none());
        assert!(rotate_at_block("(at 1 2 3 4)", 90.0, false).is_none());
    }

    #[test]
    fn indent_block_reimposes_one_line_ending() {
        // A CRLF library footprint going into an LF board and the reverse:
        // whichever the destination uses is what comes out.
        assert_eq!(indent_block("a\r\nb", "\t", "\n"), "\ta\n\tb");
        assert_eq!(indent_block("a\nb", "\t", "\r\n"), "\ta\r\n\tb");
    }

    #[test]
    fn sexp_strings_are_escaped_and_quoted() {
        // Input characters:  a " b \ c
        let input = ['a', '"', 'b', '\\', 'c'].iter().collect::<String>();
        let expected = ['"', 'a', '\\', '"', 'b', '\\', '\\', 'c', '"']
            .iter()
            .collect::<String>();
        assert_eq!(quote_sexp_string(&input), expected);
        assert_eq!(quote_sexp_string("plain"), "\"plain\"");
    }

    #[test]
    fn name_span_covers_the_quoted_header_name() {
        let content = library_footprint();
        let span = footprint_name_span(&content).expect("header not found");
        assert_eq!(&content[span], "\"R_0805_2012Metric\"");
    }

    #[test]
    fn reference_substitution_targets_the_reference_property_only() {
        let mut out = library_footprint();
        replace_reference(&mut out, "R42").unwrap();
        assert!(out.contains("(property \"Reference\" \"R42\""), "{out}");
        assert!(
            out.contains("(property \"Value\" \"R_0805_2012Metric\""),
            "Value must be untouched:\n{out}"
        );
        assert!(!out.contains("REF**"));
    }

    /// KiCad 10's own `Capacitor_SMD:C_0603_1608Metric`, byte for byte: a
    /// mounting attribute, description, keywords and a 3D model — everything
    /// `place_component` used to drop on the way to the board.
    const C_0603: &str = include_str!("../../tests/fixtures/c_0603_1608metric_kicad10.kicad_mod");

    /// The same footprint with a non-trivial model transform, to prove the
    /// transform is carried rather than defaulted.
    fn c_0603_with_moved_model() -> String {
        C_0603.replacen("(xyz 0 0 0)", "(xyz 0.1 0 0)", 1).replacen(
            "\t\t(rotate\n\t\t\t(xyz 0 0 0)",
            "\t\t(rotate\n\t\t\t(xyz 0 0 270)",
            1,
        )
    }

    /// A project registering `Capacitor_SMD` with `footprint` as its
    /// `C_0603_1608Metric`, plus an empty board.
    fn capacitor_fixture(dir: &Path, footprint: &str) -> std::path::PathBuf {
        let pretty = dir.join("Capacitor_SMD.pretty");
        std::fs::create_dir_all(&pretty).unwrap();
        std::fs::write(pretty.join("C_0603_1608Metric.kicad_mod"), footprint).unwrap();
        library_fixture(dir, "Capacitor_SMD", &pretty)
    }

    /// A project registering `pretty` as `nickname`, plus an empty board.
    fn library_fixture(dir: &Path, nickname: &str, pretty: &Path) -> std::path::PathBuf {
        std::fs::write(
            dir.join("fp-lib-table"),
            format!(
                "(fp_lib_table\n\t(version 7)\n\t(lib (name \"{nickname}\") (type \"KiCad\") (uri \"{}\") (options \"\") (descr \"\"))\n)\n",
                pretty.to_string_lossy()
            ),
        )
        .unwrap();
        let board = dir.join("b.kicad_pcb");
        std::fs::write(&board, EMPTY_BOARD).unwrap();
        board
    }

    #[test]
    fn the_ipc_item_carries_the_librarys_models_attributes_and_description() {
        use copperpour_ipc::gen::kiapi::board::types as board;
        let tmp = tempfile::tempdir().unwrap();
        let source = c_0603_with_moved_model();
        capacitor_fixture(tmp.path(), &source);
        let prepared = PreparedFootprintPlacement::from_library(
            &source,
            "Capacitor_SMD:C_0603_1608Metric",
            "C1",
            None,
            10.0,
            20.0,
            0.0,
            "F.Cu",
            Some(tmp.path()),
        )
        .unwrap();
        let item = prepared
            .ipc_item(
                "Capacitor_SMD:C_0603_1608Metric",
                "C1",
                10.0,
                20.0,
                crate::tools::library_footprint::CarriedByKiCad::ALL,
            )
            .unwrap();
        let footprint = board::FootprintInstance::decode(item.value.as_slice()).unwrap();
        assert_eq!(
            footprint.attributes.unwrap().mounting_style,
            board::FootprintMountingStyle::FmsSmd as i32
        );
        let definition = footprint.definition.unwrap();
        let described = definition.attributes.unwrap();
        assert!(described.description.starts_with("Capacitor SMD 0603"));
        assert_eq!(described.keywords, "capacitor");
        let models: Vec<board::Footprint3DModel> = definition
            .items
            .iter()
            .filter(|item| {
                copperpour_ipc::builders::any_is(item, "kiapi.board.types.Footprint3DModel")
            })
            .map(|item| board::Footprint3DModel::decode(item.value.as_slice()).unwrap())
            .collect();
        assert_eq!(models.len(), 1, "the library's model must reach the board");
        assert_eq!(
            models[0].filename,
            "${KICAD10_3DMODEL_DIR}/Capacitor_SMD.3dshapes/C_0603_1608Metric.step"
        );
        assert_eq!(models[0].offset.unwrap().x_nm, 0.1);
        assert_eq!(models[0].rotation.unwrap().z_nm, 270.0);
    }

    #[test]
    fn a_back_side_part_is_built_on_the_front_at_the_negated_rotation() {
        use copperpour_ipc::gen::kiapi::board::types as board;
        let tmp = tempfile::tempdir().unwrap();
        capacitor_fixture(tmp.path(), C_0603);
        let prepared = PreparedFootprintPlacement::from_library(
            C_0603,
            "Capacitor_SMD:C_0603_1608Metric",
            "C1",
            None,
            10.0,
            20.0,
            30.0,
            "B.Cu",
            Some(tmp.path()),
        )
        .unwrap();
        // KiCad's top-bottom flip negates the orientation about the anchor,
        // so the F.Cu copy is built at -30 to land at 30 on B.Cu.
        let item = prepared
            .ipc_item(
                "Capacitor_SMD:C_0603_1608Metric",
                "C1",
                10.0,
                20.0,
                crate::tools::library_footprint::CarriedByKiCad::ALL,
            )
            .unwrap();
        let footprint = board::FootprintInstance::decode(item.value.as_slice()).unwrap();
        assert_eq!(footprint.layer, board::BoardLayer::BlFCu as i32);
        assert_eq!(footprint.orientation.unwrap().value_degrees, -30.0);
        // The closed-board copy is mirrored by the flip_component code path.
        let block = prepared.file_block().unwrap();
        assert!(block.contains("(layer \"B.Cu\")"), "{block}");
        assert!(block.contains("(at 10 20 30)"), "{block}");
    }

    /// The Reference and Value the IPC item carries are the ones KiCad 10.0.6
    /// gives the stock C_0603 placed from its library at 0° and 180° on F.Cu
    /// (`c_0603_ipc.README.md`): the library's layer, visibility, font,
    /// stroke, line spacing and keep-upright, placed as KiCad places them
    /// (#157). Only what KiCad assigns itself is left out of the comparison.
    #[test]
    fn the_ipc_item_carries_the_reference_and_value_kicad_places() {
        use copperpour_ipc::gen::kiapi::board::types as board;
        let tmp = tempfile::tempdir().unwrap();
        capacitor_fixture(tmp.path(), C_0603);
        let comparable = |field: &Option<board::Field>| {
            let mut field = field.clone().expect("the footprint has the field");
            field.id = None;
            let text = field.text.as_mut().unwrap();
            text.id = None;
            text.parent = None;
            field
        };
        let text_of = |field: &Option<board::Field>| {
            field
                .as_ref()
                .unwrap()
                .text
                .as_ref()
                .unwrap()
                .text
                .as_ref()
                .unwrap()
                .text
                .clone()
        };
        for capture in [
            &include_bytes!("../../tests/fixtures/c_0603_loaded_front_0.ipc.bin")[..],
            &include_bytes!("../../tests/fixtures/c_0603_loaded_front_180.ipc.bin")[..],
        ] {
            let kicad = board::FootprintInstance::decode(capture).unwrap();
            let (reference, value) = (text_of(&kicad.reference_field), text_of(&kicad.value_field));
            let position = kicad.position.unwrap();
            let x = copperpour_ipc::builders::nm_to_mm(position.x_nm);
            let y = copperpour_ipc::builders::nm_to_mm(position.y_nm);
            let rotation = kicad.orientation.unwrap().value_degrees;
            let prepared = PreparedFootprintPlacement::from_library(
                C_0603,
                "Capacitor_SMD:C_0603_1608Metric",
                &reference,
                Some(&value),
                x,
                y,
                rotation,
                "F.Cu",
                Some(tmp.path()),
            )
            .unwrap();
            let item = prepared
                .ipc_item(
                    "Capacitor_SMD:C_0603_1608Metric",
                    &reference,
                    x,
                    y,
                    crate::tools::library_footprint::CarriedByKiCad::ALL,
                )
                .unwrap();
            let built = board::FootprintInstance::decode(item.value.as_slice()).unwrap();
            assert_eq!(
                comparable(&built.reference_field),
                comparable(&kicad.reference_field),
                "Reference at {rotation}°"
            );
            assert_eq!(
                comparable(&built.value_field),
                comparable(&kicad.value_field),
                "Value at {rotation}°"
            );
            let definition = built.definition.unwrap();
            assert_eq!(definition.reference_field, built.reference_field);
            assert_eq!(definition.value_field, built.value_field);
        }
    }

    /// The library's custom property (`KiLib_Generator` on the stock C_0603)
    /// reaches the IPC item as KiCad 10.0.6 places it from the library at 0°
    /// and 180° on F.Cu (`c_0603_ipc.README.md`). Without it, a library
    /// refresh right after placing reports `metadata` changed.
    #[test]
    fn the_ipc_item_carries_the_custom_properties_kicad_places() {
        use copperpour_ipc::gen::kiapi::board::types as board;
        let tmp = tempfile::tempdir().unwrap();
        capacitor_fixture(tmp.path(), C_0603);
        let custom_fields = |definition: &board::Footprint| {
            definition
                .items
                .iter()
                .filter(|item| copperpour_ipc::builders::any_is(item, "kiapi.board.types.Field"))
                .map(|item| {
                    let mut field = board::Field::decode(item.value.as_slice()).unwrap();
                    field.id = None;
                    let text = field.text.as_mut().unwrap();
                    text.id = None;
                    text.parent = None;
                    field
                })
                .collect::<Vec<_>>()
        };
        for capture in [
            &include_bytes!("../../tests/fixtures/c_0603_loaded_front_0.ipc.bin")[..],
            &include_bytes!("../../tests/fixtures/c_0603_loaded_front_180.ipc.bin")[..],
        ] {
            let kicad = board::FootprintInstance::decode(capture).unwrap();
            let position = kicad.position.unwrap();
            let x = copperpour_ipc::builders::nm_to_mm(position.x_nm);
            let y = copperpour_ipc::builders::nm_to_mm(position.y_nm);
            let rotation = kicad.orientation.unwrap().value_degrees;
            let expected = custom_fields(kicad.definition.as_ref().unwrap());
            assert_eq!(
                expected
                    .iter()
                    .map(|field| field.name.as_str())
                    .collect::<Vec<_>>(),
                ["KiLib_Generator"],
                "the capture holds the library's one custom property"
            );
            let prepared = PreparedFootprintPlacement::from_library(
                C_0603,
                "Capacitor_SMD:C_0603_1608Metric",
                "C1",
                None,
                x,
                y,
                rotation,
                "F.Cu",
                Some(tmp.path()),
            )
            .unwrap();
            let item = prepared
                .ipc_item(
                    "Capacitor_SMD:C_0603_1608Metric",
                    "C1",
                    x,
                    y,
                    crate::tools::library_footprint::CarriedByKiCad::ALL,
                )
                .unwrap();
            let built = board::FootprintInstance::decode(item.value.as_slice()).unwrap();
            assert_eq!(
                custom_fields(built.definition.as_ref().unwrap()),
                expected,
                "custom properties at {rotation}°"
            );
        }
    }

    /// A library Datasheet reaches the IPC item, on the instance and its
    /// definition, as the sync gives it to the footprints it adds. Read from
    /// the installed library (CC BY-SA), whose
    /// `Package_SO:Texas_S-PDSO-G8_3x3mm_P0.65mm` has a Datasheet URL; skips
    /// without one.
    #[test]
    fn the_ipc_item_carries_the_librarys_datasheet() {
        use copperpour_ipc::gen::kiapi::board::types as board;
        const NAME: &str = "Texas_S-PDSO-G8_3x3mm_P0.65mm";
        let Some(pretty) = crate::tools::find_kicad_library_dirs("footprints")
            .into_iter()
            .map(|dir| dir.join("Package_SO.pretty"))
            .find(|pretty| pretty.join(format!("{NAME}.kicad_mod")).is_file())
        else {
            eprintln!("SKIP: no installed Package_SO.pretty/{NAME}.kicad_mod");
            return;
        };
        let source = std::fs::read_to_string(pretty.join(format!("{NAME}.kicad_mod"))).unwrap();
        let Some(url) = source
            .split("(property \"Datasheet\" \"")
            .nth(1)
            .and_then(|rest| rest.split('"').next())
            .filter(|url| !url.is_empty())
            .map(str::to_string)
        else {
            eprintln!("SKIP: the installed {NAME} has no Datasheet");
            return;
        };
        let tmp = tempfile::tempdir().unwrap();
        library_fixture(tmp.path(), "Package_SO", &pretty);
        let lib_id = format!("Package_SO:{NAME}");
        let prepared = PreparedFootprintPlacement::from_library(
            &source,
            &lib_id,
            "U1",
            None,
            10.0,
            20.0,
            0.0,
            "F.Cu",
            Some(tmp.path()),
        )
        .unwrap();
        let item = prepared
            .ipc_item(
                &lib_id,
                "U1",
                10.0,
                20.0,
                crate::tools::library_footprint::CarriedByKiCad::ALL,
            )
            .unwrap();
        let built = board::FootprintInstance::decode(item.value.as_slice()).unwrap();
        let shown = |field: &Option<board::Field>| {
            field
                .as_ref()
                .and_then(|field| field.text.as_ref())
                .and_then(|text| text.text.as_ref())
                .map(|text| text.text.clone())
        };
        assert_eq!(shown(&built.datasheet_field), Some(url));
        assert_eq!(
            built.definition.as_ref().unwrap().datasheet_field,
            built.datasheet_field
        );
        assert_eq!(shown(&built.reference_field).as_deref(), Some("U1"));
    }

    /// Custom pads reach the IPC item as KiCad 10.0.8 holds them after
    /// placing the same stock footprints itself (`custom_pads_ipc.README.md`):
    /// shape, anchor, size, position, angle and every primitive, pad-local and
    /// unrotated, at 0° and 90° and for both anchor shapes (#679). Only what
    /// KiCad assigns itself (each primitive's id, parent and empty net) is
    /// left out of the comparison.
    #[test]
    fn the_ipc_item_carries_custom_pads_as_kicad_places_them() {
        use copperpour_ipc::gen::kiapi::board::types as board;
        const VQFN: &str = "VQFN-20-1EP_3x3mm_P0.45mm_EP1.55x1.55mm";
        const VQFN_SOURCE: &str =
            include_str!("../../tests/fixtures/pad_primitives_vqfn20_kicad10.kicad_mod");
        let pads = |footprint: &board::FootprintInstance| {
            footprint
                .definition
                .as_ref()
                .unwrap()
                .items
                .iter()
                .filter(|item| copperpour_ipc::builders::any_is(item, "kiapi.board.types.Pad"))
                .map(|item| board::Pad::decode(item.value.as_slice()).unwrap())
                .collect::<Vec<_>>()
        };
        let comparable = |shapes: &[board::BoardGraphicShape]| {
            shapes
                .iter()
                .cloned()
                .map(|mut shape| {
                    shape.id = None;
                    shape.parent = None;
                    shape.net = None;
                    shape
                })
                .collect::<Vec<_>>()
        };
        let mut custom = 0;
        for (capture, library, name, source) in [
            (
                &include_bytes!("../../tests/fixtures/custom_pads_vqfn20_0_ipc.bin")[..],
                "Package_DFN_QFN",
                VQFN,
                VQFN_SOURCE,
            ),
            (
                &include_bytes!("../../tests/fixtures/custom_pads_vqfn20_90_ipc.bin")[..],
                "Package_DFN_QFN",
                VQFN,
                VQFN_SOURCE,
            ),
            (
                &include_bytes!("../../tests/fixtures/custom_pads_microsd_ipc.bin")[..],
                "Connector_Card",
                "SD-SIM_microSD-microSIM_Molex_104168-1620",
                include_str!("../../tests/fixtures/pad_primitives_microsd_kicad10.kicad_mod"),
            ),
            (
                &include_bytes!("../../tests/fixtures/custom_pads_wuerth_ipc.bin")[..],
                "Mounting_Wuerth",
                "Mounting_Wuerth_WA-SMSE-ExternalM3_H10mm_9771100360",
                include_str!("../../tests/fixtures/pad_primitives_wuerth_kicad10.kicad_mod"),
            ),
            (
                &include_bytes!("../../tests/fixtures/custom_pads_fiducial_ipc.bin")[..],
                "Fiducial",
                "Fiducial_Cross_1.5mm_Mask2mm",
                include_str!("../../tests/fixtures/pad_primitives_fiducial_kicad10.kicad_mod"),
            ),
            (
                &include_bytes!("../../tests/fixtures/custom_pads_sfh2201_0_ipc.bin")[..],
                "OptoDevice",
                "Osram_SFH2201",
                include_str!("../../tests/fixtures/custom_pads_sfh2201_kicad10.kicad_mod"),
            ),
            (
                &include_bytes!("../../tests/fixtures/custom_pads_sfh2201_270_ipc.bin")[..],
                "OptoDevice",
                "Osram_SFH2201",
                include_str!("../../tests/fixtures/custom_pads_sfh2201_kicad10.kicad_mod"),
            ),
        ] {
            let kicad = board::FootprintInstance::decode(capture).unwrap();
            let reference = kicad
                .reference_field
                .as_ref()
                .and_then(|field| field.text.as_ref())
                .and_then(|text| text.text.as_ref())
                .map(|text| text.text.clone())
                .unwrap();
            let position = kicad.position.unwrap();
            let x = copperpour_ipc::builders::nm_to_mm(position.x_nm);
            let y = copperpour_ipc::builders::nm_to_mm(position.y_nm);
            let rotation = kicad.orientation.unwrap().value_degrees;
            let tmp = tempfile::tempdir().unwrap();
            let pretty = tmp.path().join(format!("{library}.pretty"));
            std::fs::create_dir(&pretty).unwrap();
            std::fs::write(pretty.join(format!("{name}.kicad_mod")), source).unwrap();
            library_fixture(tmp.path(), library, &pretty);
            let lib_id = format!("{library}:{name}");
            let prepared = PreparedFootprintPlacement::from_library(
                source,
                &lib_id,
                &reference,
                None,
                x,
                y,
                rotation,
                "F.Cu",
                Some(tmp.path()),
            )
            .unwrap();
            let item = prepared
                .ipc_item(
                    &lib_id,
                    &reference,
                    x,
                    y,
                    crate::tools::library_footprint::CarriedByKiCad::ALL,
                )
                .unwrap();
            let built = board::FootprintInstance::decode(item.value.as_slice()).unwrap();
            let (kicad_pads, built_pads) = (pads(&kicad), pads(&built));
            assert_eq!(kicad_pads.len(), built_pads.len(), "{reference}");
            for (kicad_pad, built_pad) in kicad_pads.iter().zip(&built_pads) {
                let kicad_stack = kicad_pad.pad_stack.as_ref().unwrap();
                let kicad_layer = &kicad_stack.copper_layers[0];
                if kicad_layer.shape != board::PadStackShape::PssCustom as i32 {
                    continue;
                }
                custom += 1;
                let pad = format!("{reference} pad '{}'", kicad_pad.number);
                let built_stack = built_pad.pad_stack.as_ref().unwrap();
                let built_layer = &built_stack.copper_layers[0];
                assert_eq!(built_pad.number, kicad_pad.number, "{pad}");
                assert_eq!(built_pad.position, kicad_pad.position, "{pad}");
                // The same angle, within one turn: the builder sends -90° where
                // KiCad reports 270°.
                let turn = |stack: &board::PadStack| {
                    stack
                        .angle
                        .as_ref()
                        .map(|angle| angle.value_degrees.rem_euclid(360.0))
                };
                assert_eq!(turn(built_stack), turn(kicad_stack), "{pad}");
                assert_eq!(built_layer.shape, kicad_layer.shape, "{pad}");
                assert_eq!(
                    built_layer.custom_anchor_shape, kicad_layer.custom_anchor_shape,
                    "{pad}"
                );
                assert_eq!(built_layer.size, kicad_layer.size, "{pad}");
                // The zone connection and spoke angle KiCad holds after its
                // parser loaded the pad: 90° for a rect anchor, 45° for a
                // circle one.
                assert_eq!(
                    built_stack.zone_settings, kicad_stack.zone_settings,
                    "{pad}"
                );
                assert_eq!(
                    comparable(&built_layer.custom_shapes),
                    comparable(&kicad_layer.custom_shapes),
                    "{pad}"
                );
            }
        }
        assert_eq!(
            custom,
            8 + 8 + 2 + 3 + 1 + 2 + 2,
            "every captured custom pad"
        );
    }

    /// Pad solder margins reach the IPC item as KiCad 10.0.8 holds them after
    /// placing the same stock footprints itself: the PQFN-8's paste margin on
    /// four of its pads (`issue_300_ipc.README.md`) and the fiducial's mask
    /// margin (`custom_pads_ipc.README.md`), on both outer layers, and nothing
    /// on the pads without one. Neither footprint has margins of its own.
    #[test]
    fn the_ipc_item_carries_pad_solder_margins_as_kicad_places_them() {
        use copperpour_ipc::gen::kiapi::board::types as board;
        let margins = |outer: Option<&board::PadStackOuterLayer>| {
            let mask = outer.and_then(|outer| outer.solder_mask_settings.as_ref());
            let paste = outer.and_then(|outer| outer.solder_paste_settings.as_ref());
            (
                mask.and_then(|mask| mask.solder_mask_margin),
                paste.and_then(|paste| paste.solder_paste_margin),
                paste.and_then(|paste| paste.solder_paste_margin_ratio),
            )
        };
        let summary = |footprint: &board::FootprintInstance| {
            let overrides = footprint.overrides.unwrap_or_default();
            let pads = footprint
                .definition
                .as_ref()
                .unwrap()
                .items
                .iter()
                .filter(|item| copperpour_ipc::builders::any_is(item, "kiapi.board.types.Pad"))
                .map(|item| {
                    let pad = board::Pad::decode(item.value.as_slice()).unwrap();
                    let stack = pad.pad_stack.unwrap();
                    (
                        pad.number,
                        margins(stack.front_outer_layers.as_ref()),
                        margins(stack.back_outer_layers.as_ref()),
                    )
                })
                .collect::<Vec<_>>();
            (
                overrides
                    .solder_mask
                    .and_then(|mask| mask.solder_mask_margin),
                overrides.solder_paste,
                pads,
            )
        };
        let mut with_margins = 0;
        for (capture, library, name, source) in [
            (
                &include_bytes!("../../tests/fixtures/issue_300_u6.ipc.bin")[..],
                "Package_DFN_QFN",
                "PQFN-8-EP_6x5mm_P1.27mm_Generic",
                include_str!("../../tests/fixtures/issue_300_pqfn8_kicad10.kicad_mod"),
            ),
            (
                &include_bytes!("../../tests/fixtures/custom_pads_fiducial_ipc.bin")[..],
                "Fiducial",
                "Fiducial_Cross_1.5mm_Mask2mm",
                include_str!("../../tests/fixtures/pad_primitives_fiducial_kicad10.kicad_mod"),
            ),
        ] {
            let kicad = board::FootprintInstance::decode(capture).unwrap();
            let reference = kicad
                .reference_field
                .as_ref()
                .and_then(|field| field.text.as_ref())
                .and_then(|text| text.text.as_ref())
                .map(|text| text.text.clone())
                .unwrap();
            let position = kicad.position.unwrap();
            let x = copperpour_ipc::builders::nm_to_mm(position.x_nm);
            let y = copperpour_ipc::builders::nm_to_mm(position.y_nm);
            let rotation = kicad.orientation.unwrap().value_degrees;
            let tmp = tempfile::tempdir().unwrap();
            let pretty = tmp.path().join(format!("{library}.pretty"));
            std::fs::create_dir(&pretty).unwrap();
            std::fs::write(pretty.join(format!("{name}.kicad_mod")), source).unwrap();
            library_fixture(tmp.path(), library, &pretty);
            let lib_id = format!("{library}:{name}");
            let prepared = PreparedFootprintPlacement::from_library(
                source,
                &lib_id,
                &reference,
                None,
                x,
                y,
                rotation,
                "F.Cu",
                Some(tmp.path()),
            )
            .unwrap();
            let item = prepared
                .ipc_item(
                    &lib_id,
                    &reference,
                    x,
                    y,
                    crate::tools::library_footprint::CarriedByKiCad::ALL,
                )
                .unwrap();
            let built = board::FootprintInstance::decode(item.value.as_slice()).unwrap();
            let kicad = summary(&kicad);
            assert_eq!(summary(&built), kicad, "{reference}");
            with_margins += kicad
                .2
                .iter()
                .filter(|(_, front, _)| *front != (None, None, None))
                .count();
        }
        assert_eq!(with_margins, 4 + 1, "every captured pad with a margin");
    }

    /// A custom pad with `(clearance convexhull)` is refused by name: KiCad's
    /// pad message has no field for it, and placing it with the outline
    /// clearance would give the board a different zone clearance from the
    /// library (#679).
    #[test]
    fn a_convexhull_custom_pad_is_refused_with_the_pad_named() {
        let source = include_str!("../../tests/fixtures/pad_primitives_vqfn20_kicad10.kicad_mod")
            .replace("(clearance outline)", "(clearance convexhull)");
        let error = extract_pad_definitions(&source).unwrap_err().to_string();
        assert!(
            error.contains("custom pad '1'") && error.contains("convexhull"),
            "{error}"
        );
    }

    /// The issue's reproduction, served: a stock C_0603 placed live at 180°
    /// reaches KiCad with the library's Reference on F.SilkS and its Value
    /// visible on F.Fab, both with a 0.15 mm stroke and kept upright, and the
    /// response carries no warning (#157).
    #[tokio::test]
    async fn place_component_sends_the_librarys_reference_and_value() {
        use crate::tools::pcb_board::board_mock::{
            ctx_talking_to, spawn_kicad_holding_items, ItemStore,
        };
        use copperpour_ipc::gen::kiapi::board::types as board;
        use std::sync::{Arc, Mutex};

        let tmp = tempfile::tempdir().unwrap();
        let board_path = capacitor_fixture(tmp.path(), C_0603);
        let store = Arc::new(Mutex::new(ItemStore::default()));
        let server =
            spawn_kicad_holding_items(&board_path, vec![], store.clone(), |_| {}, |_, _| None);
        let args = json!({
            "board": board_path.to_string_lossy(),
            "footprint": "Capacitor_SMD:C_0603_1608Metric",
            "reference": "C11",
            "x": 20.0, "y": 30.0, "rotation": 180.0,
        });
        let res = handle_place_component(&args, &ctx_talking_to(server.address().to_string()))
            .await
            .unwrap();
        assert!(!res.is_error, "handler errored: {:?}", res.content);
        let out: serde_json::Value = serde_json::from_str(&result_text(&res)).unwrap();
        assert_eq!(out["source"], "ipc");
        assert!(out.get("warning").is_none(), "{out}");

        let placed =
            board::FootprintInstance::decode(store.lock().unwrap().items[0].value.as_slice())
                .unwrap();
        for (field, text, layer) in [
            (&placed.reference_field, "C11", board::BoardLayer::BlFSilkS),
            (
                &placed.value_field,
                "C_0603_1608Metric",
                board::BoardLayer::BlFFab,
            ),
        ] {
            let field = field.as_ref().unwrap();
            assert!(field.visible, "{} is visible in the library", field.name);
            let board_text = field.text.as_ref().unwrap();
            assert_eq!(board_text.layer, layer as i32, "{}", field.name);
            let shown = board_text.text.as_ref().unwrap();
            assert_eq!(shown.text, text);
            let attributes = shown.attributes.as_ref().unwrap();
            assert_eq!(attributes.stroke_width.unwrap().value_nm, 150_000);
            assert_eq!(attributes.line_spacing, 1.0);
            assert!(attributes.keep_upright, "{} keeps upright", field.name);
        }
    }

    #[tokio::test]
    async fn served_placement_reports_uncertainty_for_a_missing_live_angle() {
        use crate::tools::pcb_board::board_mock::{spawn_kicad_holding_items, ItemStore};
        use copperpour_ipc::{builders, gen::kiapi};
        use std::sync::{Arc, Mutex};

        let dir = tempfile::tempdir().unwrap();
        let board = capacitor_fixture(dir.path(), C_0603);
        let store = Arc::new(Mutex::new(ItemStore::default()));
        let server = spawn_kicad_holding_items(
            &board,
            vec![],
            store.clone(),
            |_| {},
            |command, held| {
                if !command.type_url.ends_with("GetItems") || held.creates == 0 {
                    return None;
                }
                let mut footprint =
                    kiapi::board::types::FootprintInstance::decode(held.items[0].value.as_slice())
                        .unwrap();
                footprint.orientation = None;
                Some(builders::pack_any(
                    &kiapi::common::commands::GetItemsResponse {
                        status: kiapi::common::types::ItemRequestStatus::IrsOk as i32,
                        items: vec![builders::pack_any(
                            &footprint,
                            "kiapi.board.types.FootprintInstance",
                        )],
                        ..Default::default()
                    },
                    "kiapi.common.commands.GetItemsResponse",
                ))
            },
        );
        let handler = crate::mcp::handler::McpHandler::new(crate::tools::ServerConfig {
            ipc_address: server.address().to_string(),
            eager_toolsets: true,
            ..Default::default()
        })
        .await
        .unwrap();
        let reply = handler
            .handle_message(json!({
                "jsonrpc": "2.0", "id": 1, "method": "tools/call",
                "params": { "name": "place_component", "arguments": {
                    "board": board, "footprint": "Capacitor_SMD:C_0603_1608Metric",
                    "reference": "C11", "x": 20.0, "y": 30.0
                }}
            }))
            .await
            .unwrap()
            .result
            .unwrap();
        let body: serde_json::Value =
            serde_json::from_str(reply["content"][0]["text"].as_str().unwrap()).unwrap();
        assert_eq!(body["error"]["kind"], "ipc_outcome_unknown", "{body}");
        let reason = body["error"]["reason"].as_str().unwrap();
        assert!(
            reason.contains("C11") && reason.contains("no readable orientation"),
            "{body}"
        );
        let held = store.lock().unwrap();
        assert_eq!((held.creates, held.updates, held.deletes), (1, 0, 0));
    }

    /// KiCad 10's stock M.2 edge connectors write Reference and Value twice
    /// each, and which copy KiCad keeps is not to be guessed. Such a part is
    /// placed with the fields every footprint gets, and the response says so,
    /// rather than the part being refused over its text (#157). Read from the
    /// installed library, which is CC BY-SA; skips without one.
    #[tokio::test]
    async fn place_component_says_when_the_librarys_fields_cannot_be_read() {
        use crate::tools::pcb_board::board_mock::{
            ctx_talking_to, spawn_kicad_holding_items, ItemStore,
        };
        use copperpour_ipc::gen::kiapi::board::types as board;
        use std::sync::{Arc, Mutex};

        let Some(pretty) = crate::tools::find_kicad_library_dirs("footprints")
            .into_iter()
            .map(|dir| dir.join("Connector_PCBEdge.pretty"))
            .find(|pretty| pretty.join("M.2_2230-xx-E.kicad_mod").is_file())
        else {
            eprintln!("SKIP: no installed Connector_PCBEdge.pretty/M.2_2230-xx-E.kicad_mod");
            return;
        };
        let source = std::fs::read_to_string(pretty.join("M.2_2230-xx-E.kicad_mod")).unwrap();
        if source.matches("(property \"Reference\"").count() < 2 {
            eprintln!("SKIP: the installed M.2_2230-xx-E writes Reference once");
            return;
        }
        let tmp = tempfile::tempdir().unwrap();
        let board_path = library_fixture(tmp.path(), "Connector_PCBEdge", &pretty);
        let store = Arc::new(Mutex::new(ItemStore::default()));
        let server =
            spawn_kicad_holding_items(&board_path, vec![], store.clone(), |_| {}, |_, _| None);
        let args = json!({
            "board": board_path.to_string_lossy(),
            "footprint": "Connector_PCBEdge:M.2_2230-xx-E",
            "reference": "J1",
            "x": 20.0, "y": 30.0,
        });
        let res = handle_place_component(&args, &ctx_talking_to(server.address().to_string()))
            .await
            .unwrap();
        assert!(!res.is_error, "handler errored: {:?}", res.content);
        let out: serde_json::Value = serde_json::from_str(&result_text(&res)).unwrap();
        assert_eq!(out["source"], "ipc");
        let warning = out["warning"].as_str().expect("a warning");
        assert!(
            warning.starts_with("Connector_PCBEdge:M.2_2230-xx-E's library properties could not be read, so J1 was placed with default Reference and Value fields"),
            "{warning}"
        );
        assert!(warning.contains("appears more than once"), "{warning}");

        let placed =
            board::FootprintInstance::decode(store.lock().unwrap().items[0].value.as_slice())
                .unwrap();
        let value = placed.value_field.unwrap();
        assert!(!value.visible, "the default Value is hidden");
        assert_eq!(
            value.text.unwrap().layer,
            board::BoardLayer::BlFSilkS as i32,
            "the default Value is on the silkscreen"
        );
    }

    #[tokio::test]
    async fn place_component_mirrors_a_back_side_part_into_a_closed_board() {
        let tmp = tempfile::tempdir().unwrap();
        let board = capacitor_fixture(tmp.path(), C_0603);

        let args = json!({
            "board": board.to_string_lossy(),
            "footprint": "Capacitor_SMD:C_0603_1608Metric",
            "reference": "C1",
            "x": 50.0, "y": 60.0, "rotation": 90.0,
            "layer": "B.Cu",
        });
        let res = handle_place_component(&args, &test_ctx()).await.unwrap();
        assert!(!res.is_error, "handler errored: {:?}", res.content);
        let out: serde_json::Value = serde_json::from_str(&result_text(&res)).unwrap();
        assert_eq!(out["source"], "file");
        assert_eq!(out["layer"], "B.Cu");

        let written = std::fs::read_to_string(&board).unwrap();
        let block = &written[written.find("(footprint \"").unwrap()..];
        assert!(
            block.contains("(at 50 60 90)"),
            "placement missing:\n{block}"
        );
        assert!(
            block.contains("(layers \"B.Cu\" \"B.Mask\" \"B.Paste\")"),
            "pads must be on the back:\n{block}"
        );
        assert!(
            !block.contains("\"F.Cu\""),
            "nothing may stay on the front:\n{block}"
        );
        assert!(
            block.contains("(model \""),
            "the model must be carried:\n{block}"
        );
        assert_eq!(count_parens(&written), 0, "board is no longer balanced");
    }

    #[tokio::test]
    async fn a_back_side_mirror_the_closed_board_cannot_make_writes_nothing() {
        let tmp = tempfile::tempdir().unwrap();
        // offset.y is negated by a real flip; the closed-board mirror does not
        // reproduce KiCad's model transform, so it must refuse, not guess.
        let source = C_0603.replacen("(xyz 0 0 0)", "(xyz 0 0.2 0)", 1);
        let board = capacitor_fixture(tmp.path(), &source);

        let args = json!({
            "board": board.to_string_lossy(),
            "footprint": "Capacitor_SMD:C_0603_1608Metric",
            "reference": "C1",
            "x": 50.0, "y": 60.0,
            "layer": "B.Cu",
        });
        let res = handle_place_component(&args, &test_ctx()).await.unwrap();
        assert!(res.is_error, "the mirror must be refused");
        let text = result_text(&res);
        assert!(text.contains("not modified"), "{text}");
        assert_eq!(std::fs::read_to_string(&board).unwrap(), EMPTY_BOARD);
    }

    #[tokio::test]
    async fn place_component_refuses_a_side_that_is_not_f_cu_or_b_cu() {
        let tmp = tempfile::tempdir().unwrap();
        let board = fallback_fixture(tmp.path());
        let before = std::fs::read_to_string(&board).unwrap();

        let args = json!({
            "board": board.to_string_lossy(),
            "footprint": "Resistor_SMD:R_0805_2012Metric",
            "reference": "R1",
            "x": 10.0, "y": 20.0,
            "layer": "In1.Cu",
        });
        let res = handle_place_component(&args, &test_ctx()).await.unwrap();
        assert_eq!(
            crate::mcp::error::extract_error_kind(&res).as_deref(),
            Some("invalid_argument")
        );
        assert_eq!(std::fs::read_to_string(&board).unwrap(), before);
    }

    #[tokio::test]
    async fn place_component_reaches_b_cu_through_kicads_native_flip() {
        use crate::tools::pcb_board::board_mock::{
            ctx_talking_to, item_kiid, spawn_kicad_holding_items, ItemStore,
        };
        use copperpour_ipc::gen::kiapi;
        use std::sync::{Arc, Mutex};

        let tmp = tempfile::tempdir().unwrap();
        // A model a flip would have to move: the closed-board mirror refuses
        // it, and the live path must not care, because KiCad flips natively.
        let source = C_0603.replacen("(xyz 0 0 0)", "(xyz 0 0.2 0)", 1);
        let board = capacitor_fixture(tmp.path(), &source);
        let store = Arc::new(Mutex::new(ItemStore::default()));
        let flips = Arc::new(Mutex::new(0usize));
        let flips_in_mock = flips.clone();
        // KiCad's FOOTPRINT::Flip about its own anchor, reduced to what the
        // read-back checks: the side and the negated orientation.
        let server = spawn_kicad_holding_items(
            &board,
            vec![],
            store.clone(),
            |_| {},
            move |command, held| {
                if !command.type_url.ends_with("kiapi.board.commands.FlipItems") {
                    return None;
                }
                *flips_in_mock.lock().unwrap() += 1;
                let request =
                    kiapi::board::commands::FlipItems::decode(command.value.as_slice()).unwrap();
                let mut flipped_items = Vec::new();
                for id in request.items {
                    let slot = held
                        .items
                        .iter_mut()
                        .find(|item| item_kiid(item).as_deref() == Some(id.value.as_str()))
                        .unwrap();
                    let mut fp =
                        kiapi::board::types::FootprintInstance::decode(slot.value.as_slice())
                            .unwrap();
                    fp.layer = kiapi::board::types::BoardLayer::BlBCu as i32;
                    let angle = fp.orientation.unwrap_or_default().value_degrees;
                    fp.orientation = Some(kiapi::common::types::Angle {
                        value_degrees: -angle,
                    });
                    *slot = copperpour_ipc::builders::pack_any(
                        &fp,
                        "kiapi.board.types.FootprintInstance",
                    );
                    flipped_items.push(kiapi::board::commands::ItemFlipResult {
                        status: Some(kiapi::common::commands::ItemStatus {
                            code: kiapi::common::commands::ItemStatusCode::IscOk as i32,
                            error_message: String::new(),
                        }),
                        item: Some(slot.clone()),
                    });
                }
                Some(copperpour_ipc::builders::pack_any(
                    &kiapi::board::commands::FlipItemsResponse {
                        header: None,
                        status: kiapi::common::types::ItemRequestStatus::IrsOk as i32,
                        flipped_items,
                    },
                    "kiapi.board.commands.FlipItemsResponse",
                ))
            },
        );

        let args = json!({
            "board": board.to_string_lossy(),
            "footprint": "Capacitor_SMD:C_0603_1608Metric",
            "reference": "C1",
            "x": 50.0, "y": 60.0, "rotation": 90.0,
            "layer": "B.Cu",
        });
        let res = handle_place_component(&args, &ctx_talking_to(server.address().to_string()))
            .await
            .unwrap();
        assert!(!res.is_error, "handler errored: {:?}", res.content);
        let out: serde_json::Value = serde_json::from_str(&result_text(&res)).unwrap();
        assert_eq!(out["source"], "ipc");
        assert_eq!(out["layer"], "B.Cu");
        assert_eq!(out["rotation"], 90.0);
        assert_eq!(out["models"].as_array().map(Vec::len), Some(1));
        assert_eq!(*flips.lock().unwrap(), 1, "exactly one native flip");
        assert_eq!(store.lock().unwrap().creates, 1);
        assert_eq!(
            std::fs::read_to_string(&board).unwrap(),
            EMPTY_BOARD,
            "a live placement must never touch the board file"
        );
    }

    #[test]
    fn a_pre_6_0_module_footprint_is_recognised() {
        // easyeda2kicad output, verbatim apart from its model path.
        let legacy = "(module easyeda2kicad:C0603 (layer F.Cu) (tedit 5DC5F6A4)\n\
                      \t(attr smd)\n\
                      \t(fp_text reference REF** (at 0.000 -4.000) (layer F.SilkS)\n\
                      \t\t(effects (font (size 1 1) (thickness 0.15)))\n\
                      \t)\n\
                      \t(pad 1 smd rect (at -0.70 0.00 0.00) (size 0.800 0.900) (layers F.Cu F.Paste F.Mask))\n\
                      )";
        assert!(is_legacy_module_footprint(legacy));
        assert!(!is_legacy_module_footprint(&library_footprint()));
        assert!(!is_legacy_module_footprint("not a footprint"));
    }

    /// Live: needs `kicad-cli` on PATH. Run with
    /// `cargo test -p copperpour-core -- --ignored legacy_footprint_is_upgraded_by_kicad_cli`.
    #[tokio::test]
    #[ignore = "needs kicad-cli on PATH"]
    async fn legacy_footprint_is_upgraded_by_kicad_cli() {
        let legacy = "(module easyeda2kicad:C0603 (layer F.Cu) (tedit 5DC5F6A4)\n\
                      \t(attr smd)\n\
                      \t(fp_text reference REF** (at 0.000 -4.000) (layer F.SilkS)\n\
                      \t\t(effects (font (size 1 1) (thickness 0.15)))\n\
                      \t)\n\
                      \t(fp_text value C0603 (at 0.000 4.000) (layer F.Fab)\n\
                      \t\t(effects (font (size 1 1) (thickness 0.15)))\n\
                      \t)\n\
                      \t(pad 1 smd rect (at -0.70 0.00 0.00) (size 0.800 0.900) (layers F.Cu F.Paste F.Mask))\n\
                      \t(fp_arc (start 1.08 -0.40) (end 1.39 -0.40) (angle -90.00) (layer F.SilkS) (width 0.25))\n\
                      )";
        let upgraded = super::super::cli::upgrade_legacy_footprint("kicad-cli", legacy, "C0603")
            .await
            .unwrap();
        assert!(upgraded.starts_with("(footprint "), "{upgraded}");
        assert!(
            upgraded.contains("(mid "),
            "the legacy arc must be converted:\n{upgraded}"
        );
        let pads = extract_pad_definitions(&upgraded).unwrap();
        assert_eq!(pads.len(), 1);
        assert!(!extract_graphic_definitions(&upgraded).unwrap().is_empty());
    }
}

#[cfg(test)]
mod field_placement_tests {
    use super::*;

    #[test]
    fn field_anchors_come_from_the_library_footprint() {
        // R_0603-style: Reference above the silk at -1.43, Value below at 1.43.
        let source = "(footprint \"R_0603\"
	(property \"Reference\" \"REF**\"
		(at 0 -1.43 0)
		(layer \"F.SilkS\")
	)
	(property \"Value\" \"R_0603\"
		(at 0 1.43 0)
		(layer \"F.Fab\")
	)
)";
        let placement = extract_field_placement(source);
        assert_eq!(placement.reference_at, Some((0.0, -1.43, 0.0)));
        assert_eq!(placement.value_at, Some((0.0, 1.43, 0.0)));
    }

    #[test]
    fn missing_fields_leave_defaults() {
        let placement = extract_field_placement("(footprint \"bare\")");
        assert_eq!(placement.reference_at, None);
        assert_eq!(placement.value_at, None);
    }
}

#[cfg(test)]
mod pad_net_shape_tests {
    use super::*;
    use crate::router::ToolRouter;
    use crate::tools::ServerConfig;
    use std::sync::Arc;

    fn test_ctx() -> ToolContext {
        ToolContext::new(
            ServerConfig {
                kicad_cli: String::new(),
                kicad_binary: String::new(),
                ipc_address: String::new(),
                project_dir: None,
                jlcpcb_db_path: None,
                auto_load_toolsets: false,
                eager_toolsets: false,
            },
            Arc::new(ToolRouter::new()),
        )
    }

    async fn pads_of(board: &str) -> serde_json::Value {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("board.kicad_pcb");
        std::fs::write(&path, board).unwrap();
        let result = handle_get_component_pads(
            &json!({ "board": path.to_str().unwrap(), "reference": "R1" }),
            &test_ctx(),
        )
        .await
        .expect("handler should succeed");
        let body = match &result.content[0] {
            crate::mcp::protocol::ToolContent::Text { text } => text.clone(),
            _ => panic!("expected text content"),
        };
        serde_json::from_str(&body).unwrap()
    }

    fn board_with_pads(pads: &str) -> String {
        format!(
            "(kicad_pcb\n\t(version 20260206)\n\t(footprint \"TestPad\"\n\t\t(layer \"F.Cu\")\n\t\t(at 100 100)\n\
             \t\t(property \"Reference\" \"R1\" (at 0 0 0) (layer \"F.SilkS\"))\n{pads}\t)\n)\n"
        )
    }

    /// The reported bug: KiCad 10 puts the name at index 1, the old reader
    /// took index 2, and `.unwrap_or("")` turned that miss into a plausible
    /// empty string — so a fully connected pad looked exactly like an
    /// unconnected one.
    #[tokio::test]
    async fn kicad_10_pads_report_their_net_names() {
        let pads = pads_of(&board_with_pads(
            "\t\t(pad \"1\" smd rect (at -1 0) (size 1 1) (net \"GND\"))\n\
             \t\t(pad \"2\" smd rect (at 1 0) (size 1 1) (net \"VCC\"))\n",
        ))
        .await;
        assert_eq!(pads["pads"][0]["net"], json!("GND"));
        assert_eq!(pads["pads"][1]["net"], json!("VCC"));
    }

    #[tokio::test]
    async fn legacy_pads_still_report_their_net_names() {
        let pads = pads_of(&board_with_pads(
            "\t\t(pad \"1\" smd rect (at -1 0) (size 1 1) (net 1 \"GND\"))\n\
             \t\t(pad \"2\" smd rect (at 1 0) (size 1 1) (net 2 \"VCC\"))\n",
        ))
        .await;
        assert_eq!(pads["pads"][0]["net"], json!("GND"));
        assert_eq!(pads["pads"][1]["net"], json!("VCC"));
    }

    /// A pad with no net node is genuinely unconnected, and "" says so. This
    /// is the one case where the empty string is the right answer, which is
    /// why the unreadable case must not share it.
    #[tokio::test]
    async fn a_pad_with_no_net_node_reports_an_empty_name() {
        let pads = pads_of(&board_with_pads(
            "\t\t(pad \"1\" smd rect (at -1 0) (size 1 1))\n",
        ))
        .await;
        assert_eq!(pads["pads"][0]["net"], json!(""));
    }

    /// Present but unreadable is now loud. A bare `(net 1)` reference names no
    /// net, and null forces a caller to notice rather than concluding the pad
    /// is floating.
    #[tokio::test]
    async fn a_net_node_we_cannot_read_reports_null_not_empty() {
        let pads = pads_of(&board_with_pads(
            "\t\t(pad \"1\" smd rect (at -1 0) (size 1 1) (net 1))\n\
             \t\t(pad \"2\" smd rect (at 1 0) (size 1 1) (net))\n",
        ))
        .await;
        assert_eq!(pads["pads"][0]["net"], serde_json::Value::Null);
        assert_eq!(pads["pads"][1]["net"], serde_json::Value::Null);
    }

    /// The unconnected pseudo-net is named, and its name is empty — that is a
    /// real answer, not a failure, so it must not become null.
    #[tokio::test]
    async fn the_unconnected_pseudo_net_reports_an_empty_name() {
        let pads = pads_of(&board_with_pads(
            "\t\t(pad \"1\" smd rect (at -1 0) (size 1 1) (net 0 \"\"))\n",
        ))
        .await;
        assert_eq!(pads["pads"][0]["net"], json!(""));
    }
}

#[cfg(test)]
mod board_footprint_graphics_tests {
    use super::*;
    use serde_json::json;

    /// A malformed `points` is a caller mistake, so it must come back as a
    /// structured `invalid_argument` naming the field — not as an
    /// `anyhow`-flavoured handler error with the reason flattened into prose.
    /// The handler used to bubble `?` here, which is the class #194 tracks.
    #[test]
    fn a_malformed_points_argument_is_a_structured_invalid_argument() {
        let cases = [
            (json!("not an array"), "not an array"),
            (json!([{ "x": 1.0 }]), "point 0 has no numeric 'y'"),
            (json!([{ "y": 1.0 }]), "point 0 has no numeric 'x'"),
            (
                json!([{ "x": 0.0, "y": 0.0 }, { "x": 1.0, "y": "two" }]),
                "point 1 has no numeric 'y'",
            ),
        ];
        for (value, expected) in cases {
            let err = parse_points(&value).expect_err("must be refused");
            assert!(err.is_error, "{value} should be an error result");
            let text = format!("{:?}", err);
            assert!(
                text.contains("InvalidArgument") || text.contains("invalid_argument"),
                "must be structured, got: {text}"
            );
            assert!(
                text.contains(expected),
                "the message must say which point is wrong.\nwant: {expected}\ngot:  {text}"
            );
        }
    }

    #[test]
    fn well_formed_points_parse_in_order() {
        let points = parse_points(&json!([
            { "x": 0.0, "y": 0.0 },
            { "x": 2.5, "y": -1.5 },
            { "x": 2.5, "y": 2.5 }
        ]))
        .expect("valid");
        assert_eq!(points, vec![(0.0, 0.0), (2.5, -1.5), (2.5, 2.5)]);
    }

    /// An integer in JSON is a number; rejecting it would refuse
    /// `{"x": 0, "y": 0}`, which is what a caller writes for the origin.
    #[test]
    fn integer_coordinates_are_accepted() {
        assert_eq!(
            parse_points(&json!([{ "x": 0, "y": 0 }, { "x": 3, "y": 0 }, { "x": 3, "y": 3 }]))
                .expect("valid"),
            vec![(0.0, 0.0), (3.0, 0.0), (3.0, 3.0)]
        );
    }

    /// The two tools are registered, sit in `pcb_components`, and require the
    /// arguments the handlers read. A schema that drifts from its handler is
    /// the defect #217 was about, one layer up.
    #[test]
    fn both_board_graphics_tools_are_registered_with_the_arguments_they_read() {
        let tools = tools();
        for (name, required) in [
            ("list_board_footprint_graphics", vec!["board", "reference"]),
            (
                "edit_board_footprint_graphic",
                vec!["board", "reference", "uuid", "points"],
            ),
        ] {
            let def = tools
                .iter()
                .find(|t| t.name == name)
                .unwrap_or_else(|| panic!("{name} is not registered"));
            let got: Vec<&str> = def.input_schema["required"]
                .as_array()
                .unwrap_or_else(|| panic!("{name} declares no required list"))
                .iter()
                .filter_map(|v| v.as_str())
                .collect();
            assert_eq!(got, required, "{name} required arguments");
            for key in &required {
                assert!(
                    def.input_schema["properties"][key].is_object(),
                    "{name} requires '{key}' but does not declare it"
                );
            }
        }
    }
}

/// `place_component_array` declares `count_x` required and defaulted it to 1.
/// A caller who meant a 10x10 grid and mistyped the key got a single column of
/// real footprints committed to the live board over IPC, and `{"placed_count":
/// N}` back. The `count_x == 0` guard below it could never fire on that path
/// either, since the default was 1 (#218).
#[cfg(test)]
mod required_count_tests {
    use super::*;
    use serde_json::json;

    /// No IPC address configured, so any handler that reaches the IPC layer
    /// fails with the socket-path error — a different error therefore proves
    /// the handler refused before attempting anything.
    fn test_ctx_arc() -> std::sync::Arc<ToolContext> {
        std::sync::Arc::new(ToolContext::new(
            crate::tools::ServerConfig {
                kicad_cli: String::new(),
                kicad_binary: String::new(),
                ipc_address: String::new(),
                project_dir: None,
                jlcpcb_db_path: None,
                auto_load_toolsets: false,
                eager_toolsets: false,
            },
            std::sync::Arc::new(crate::router::ToolRouter::new()),
        ))
    }

    fn def(name: &str) -> ToolDef {
        tools()
            .into_iter()
            .find(|t| t.name == name)
            .unwrap_or_else(|| panic!("{name} is registered"))
    }

    fn error_of(result: &CallToolResult) -> serde_json::Value {
        let text = match result.content.first() {
            Some(crate::mcp::protocol::ToolContent::Text { text }) => text.clone(),
            other => panic!("expected text, got {other:?}"),
        };
        serde_json::from_str(&text).unwrap_or_else(|e| panic!("{e}: {text}"))
    }

    #[tokio::test]
    async fn place_component_array_refuses_a_missing_count_x() {
        let dir = tempfile::tempdir().unwrap();
        let board = dir.path().join("b.kicad_pcb");
        std::fs::write(&board, "(kicad_pcb\n  (version 20250610)\n)\n").unwrap();

        let result = (def("place_component_array").handler)(
            &json!({
                "board": board.display().to_string(),
                "footprint": "Resistor_SMD:R_0402_1005Metric",
                "start_x": 10.0,
                "start_y": 10.0,
                "count_y": 10,
                "spacing_x": 2.0,
            }),
            test_ctx_arc(),
        )
        .await
        .expect("no anyhow");

        assert!(result.is_error, "a missing count_x must be refused");
        let parsed = error_of(&result);
        assert_eq!(parsed["error"]["kind"], "invalid_argument");
        assert_eq!(parsed["error"]["field"], "count_x");
    }

    /// The zero guard still applies to an argument that is present and zero —
    /// it was only ever unreachable via the default.
    #[tokio::test]
    async fn an_explicit_zero_count_is_still_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let board = dir.path().join("b.kicad_pcb");
        std::fs::write(&board, "(kicad_pcb\n  (version 20250610)\n)\n").unwrap();

        let result = (def("place_component_array").handler)(
            &json!({
                "board": board.display().to_string(),
                "footprint": "Resistor_SMD:R_0402_1005Metric",
                "start_x": 10.0, "start_y": 10.0,
                "count_x": 0, "count_y": 10, "spacing_x": 2.0,
            }),
            test_ctx_arc(),
        )
        .await
        .expect("no anyhow");
        assert!(result.is_error, "an explicit zero must still be rejected");
    }
}

#[cfg(test)]
mod off_axis_rotation_tests {
    use crate::tools::pcb_board::board_mock::{spawn_kicad_holding_items, ItemStore};
    use copperpour_ipc::builders::{self, pack_any};
    use copperpour_ipc::gen::kiapi;
    use kiapi::common::types::graphic_shape::Geometry;
    use prost::Message;
    use serde_json::json;
    use std::sync::{Arc, Mutex};

    /// A courtyard rectangle as KiCad 10 sends it: `corner_radius` is always
    /// set, to 0 for square corners.
    fn courtyard(corner_radius_nm: i64) -> prost_types::Any {
        let mut rect = builders::board_rectangle("F.CrtYd", 0.05, 99.0, 99.0, 101.0, 101.0, false);
        if let Some(Geometry::Rectangle(r)) = rect
            .shape
            .as_mut()
            .and_then(|shape| shape.geometry.as_mut())
        {
            r.corner_radius = Some(kiapi::common::types::Distance {
                value_nm: corner_radius_nm,
            });
        }
        pack_any(&rect, "kiapi.board.types.BoardGraphicShape")
    }

    fn footprint(id: &str, reference: &str, items: Vec<prost_types::Any>) -> prost_types::Any {
        pack_any(
            &kiapi::board::types::FootprintInstance {
                id: Some(kiapi::common::types::Kiid {
                    value: id.to_string(),
                }),
                position: Some(builders::vec2(100.0, 100.0)),
                reference_field: Some(kiapi::board::types::Field {
                    text: Some(kiapi::board::types::BoardText {
                        text: Some(kiapi::common::types::Text {
                            text: reference.to_string(),
                            ..Default::default()
                        }),
                        ..Default::default()
                    }),
                    ..Default::default()
                }),
                definition: Some(kiapi::board::types::Footprint {
                    items,
                    ..Default::default()
                }),
                ..Default::default()
            },
            "kiapi.board.types.FootprintInstance",
        )
    }

    #[tokio::test]
    async fn served_delete_refuses_ambiguous_or_unreadable_inventory_without_a_write() {
        use crate::tools::pcb_board::board_mock::board_document;
        use kiapi::common::{commands, types};

        let dir = tempfile::tempdir().unwrap();
        let board = dir.path().join("target.kicad_pcb");
        let original = b"(kicad_pcb (version 20260206))";
        std::fs::write(&board, original).unwrap();
        let target = footprint("target-kiid", "R1", vec![]);
        for (items, wrong_board, status, reason) in [
            (vec![], false, types::ItemRequestStatus::IrsOk, "not found"),
            (
                vec![target.clone(), footprint("other-kiid", "R1", vec![])],
                false,
                types::ItemRequestStatus::IrsOk,
                "ambiguous",
            ),
            (
                vec![target.clone(), footprint("other-kiid", "", vec![])],
                false,
                types::ItemRequestStatus::IrsOk,
                "unreadable reference",
            ),
            (
                vec![target.clone(), footprint("", "R2", vec![])],
                false,
                types::ItemRequestStatus::IrsOk,
                "no readable KIID",
            ),
            (
                vec![target.clone(), footprint("target-kiid", "R2", vec![])],
                false,
                types::ItemRequestStatus::IrsOk,
                "duplicate KIID",
            ),
            (
                vec![target.clone()],
                true,
                types::ItemRequestStatus::IrsOk,
                "different board",
            ),
            (
                vec![target.clone()],
                false,
                types::ItemRequestStatus::IrsDocumentNotFound,
                "IRS_DOCUMENT_NOT_FOUND",
            ),
        ] {
            let store = Arc::new(Mutex::new(ItemStore {
                items,
                ..Default::default()
            }));
            let wrong_document =
                board_document(&dir.path().join("other.kicad_pcb").to_string_lossy());
            let server = spawn_kicad_holding_items(
                &board,
                vec![],
                store.clone(),
                |_| {},
                move |command, held| {
                    command.type_url.ends_with("GetItems").then(|| {
                        pack_any(
                            &commands::GetItemsResponse {
                                header: wrong_board.then(|| types::ItemHeader {
                                    document: Some(wrong_document.clone()),
                                    ..Default::default()
                                }),
                                status: status as i32,
                                items: held.items.clone(),
                            },
                            "kiapi.common.commands.GetItemsResponse",
                        )
                    })
                },
            );
            let handler = crate::mcp::handler::McpHandler::new(crate::tools::ServerConfig {
                ipc_address: server.address().to_string(),
                eager_toolsets: true,
                ..Default::default()
            })
            .await
            .unwrap();
            let result = handler
                .handle_message(json!({
                    "jsonrpc": "2.0", "id": 1, "method": "tools/call",
                    "params": { "name": "delete_component", "arguments": {
                        "board": board, "reference": "R1"
                    }}
                }))
                .await
                .unwrap()
                .result
                .unwrap();
            let body: serde_json::Value =
                serde_json::from_str(result["content"][0]["text"].as_str().unwrap()).unwrap();
            assert_eq!(store.lock().unwrap().deletes, 0, "{reason}: {body}");
            assert_eq!(result["isError"], true, "{reason}: {body}");
            assert_eq!(body["recovery"]["mutation_outcome"], "none", "{body}");
            assert!(body["message"].as_str().unwrap().contains(reason), "{body}");
            if status != types::ItemRequestStatus::IrsOk {
                assert_eq!(body["recovery"]["evidence"]["status"], reason, "{body}");
            }
            assert_eq!(std::fs::read(&board).unwrap(), original);
        }
    }

    #[tokio::test]
    async fn served_move_reports_uncertainty_when_kiid_stays_at_the_old_anchor() {
        let dir = tempfile::tempdir().unwrap();
        let board = dir.path().join("target.kicad_pcb");
        let original = b"(kicad_pcb (version 20260206))";
        std::fs::write(&board, original).unwrap();
        let fixed = footprint("target-kiid", "R1", vec![]);
        let store = Arc::new(Mutex::new(ItemStore {
            items: vec![fixed.clone()],
            ..Default::default()
        }));
        let server = spawn_kicad_holding_items(
            &board,
            vec![],
            store.clone(),
            move |item| *item = fixed.clone(),
            |_, _| None,
        );
        let handler = crate::mcp::handler::McpHandler::new(crate::tools::ServerConfig {
            ipc_address: server.address().to_string(),
            eager_toolsets: true,
            ..Default::default()
        })
        .await
        .unwrap();
        let reply = handler
            .handle_message(json!({
                "jsonrpc": "2.0", "id": 1, "method": "tools/call",
                "params": { "name": "move_component", "arguments": {
                    "board": board, "reference": "R1", "x": 120.0, "y": 130.0
                }}
            }))
            .await
            .unwrap()
            .result
            .unwrap();
        let body: serde_json::Value =
            serde_json::from_str(reply["content"][0]["text"].as_str().unwrap()).unwrap();
        assert_eq!(body["error"]["kind"], "ipc_outcome_unknown", "{body}");
        let reason = body["error"]["reason"].as_str().unwrap();
        assert!(
            reason.contains("target-kiid") && reason.contains("target.kicad_pcb"),
            "{body}"
        );
        assert_eq!(store.lock().unwrap().updates, 1);
        assert_eq!(std::fs::read(&board).unwrap(), original);
    }

    /// Serves the issue's batch, Y1 at -45° and U1 at 45°, to a KiCad
    /// holding Y1 with a courtyard of `corner_radius_nm` and U1 with none.
    async fn served_placements(corner_radius_nm: i64) -> (bool, String, Arc<Mutex<ItemStore>>) {
        let dir = tempfile::tempdir().unwrap();
        let board = dir.path().join("target.kicad_pcb");
        std::fs::write(&board, "(kicad_pcb (version 20260206))").unwrap();
        let store = Arc::new(Mutex::new(ItemStore {
            items: vec![
                footprint("y1", "Y1", vec![courtyard(corner_radius_nm)]),
                footprint("u1", "U1", vec![]),
            ],
            ..Default::default()
        }));
        let server = spawn_kicad_holding_items(&board, vec![], store.clone(), |_| {}, |_, _| None);
        let handler = crate::mcp::handler::McpHandler::new(crate::tools::ServerConfig {
            ipc_address: server.address().to_string(),
            eager_toolsets: true,
            ..Default::default()
        })
        .await
        .unwrap();
        let response = handler
            .handle_message(json!({
                "jsonrpc": "2.0",
                "id": 224,
                "method": "tools/call",
                "params": {
                    "name": "set_component_placements",
                    "arguments": {
                        "board": board.to_string_lossy(),
                        "placements": [
                            {"reference": "Y1", "x": 100.0, "y": 100.0, "rotation": -45.0},
                            {"reference": "U1", "x": 100.0, "y": 100.0, "rotation": 45.0}
                        ]
                    }
                }
            }))
            .await
            .unwrap();
        let result = response.result.unwrap();
        (
            result["isError"] == json!(true),
            result["content"][0]["text"].as_str().unwrap().to_string(),
            store,
        )
    }

    fn held_shape(store: &ItemStore) -> Option<Geometry> {
        let fp = kiapi::board::types::FootprintInstance::decode(store.items[0].value.as_slice())
            .unwrap();
        let item = &fp.definition.unwrap().items[0];
        kiapi::board::types::BoardGraphicShape::decode(item.value.as_slice())
            .unwrap()
            .shape
            .unwrap()
            .geometry
    }

    /// #224: a square-cornered rectangle rotates off-axis as a polygon, and
    /// the rest of the batch lands with it.
    #[tokio::test]
    async fn square_cornered_rectangle_rotates_off_axis_in_a_batch() {
        let (is_error, text, store) = served_placements(0).await;

        assert!(!is_error, "{text}");
        let held = store.lock().unwrap();
        assert_eq!(held.updates, 1);
        let shape = held_shape(&held);
        assert!(matches!(shape, Some(Geometry::Polygon(_))), "{shape:?}");
        let response: serde_json::Value = serde_json::from_str(&text).unwrap();
        assert_eq!(response["placements"][0]["rotation"], -45.0, "{response}");
        assert_eq!(response["placements"][1]["rotation"], 45.0, "{response}");
    }

    /// #224: a really rounded rectangle is still refused, the refusal names
    /// the footprint and the layer, and nothing in the batch is applied.
    #[tokio::test]
    async fn rounded_rectangle_refusal_names_the_part_and_applies_nothing() {
        let (is_error, text, store) = served_placements(250_000).await;

        assert!(is_error, "{text}");
        assert!(text.contains("'Y1'") && text.contains("F.CrtYd"), "{text}");
        let held = store.lock().unwrap();
        assert_eq!(held.updates, 0, "no footprint may be updated");
        assert!(matches!(held_shape(&held), Some(Geometry::Rectangle(_))));
    }
}

/// #84: a millimetre value reaches KiCad on the nanometre it names. Each test
/// drives served `tools/call` against the item-holding KiCad double and
/// decodes the protobuf KiCad would store. The expected nanometres are
/// literals, never `mm_to_nm`, so truncation inside the conversion fails them.
#[cfg(test)]
mod nanometre_rounding_tests {
    use crate::tools::library_footprint::test_margins::{
        expected_margins, placed_margins, NO_MARGINS, SOLDER_MARGIN_FOOTPRINTS,
    };
    use crate::tools::pcb_board::board_mock::{spawn_kicad_holding_items, ItemStore};
    use copperpour_ipc::builders::{any_is, pack_any};
    use copperpour_ipc::gen::kiapi;
    use kiapi::board::types::{FootprintInstance, Pad};
    use prost::Message;
    use serde_json::{json, Value};
    use std::path::Path;
    use std::sync::{Arc, Mutex};

    /// `tool` called through served dispatch on a KiCad holding `board` and
    /// the items in `store`.
    async fn served(
        board: &Path,
        store: Arc<Mutex<ItemStore>>,
        tool: &str,
        arguments: Value,
    ) -> (bool, String) {
        let server = spawn_kicad_holding_items(board, vec![], store, |_| {}, |_, _| None);
        let handler = crate::mcp::handler::McpHandler::new(crate::tools::ServerConfig {
            ipc_address: server.address().to_string(),
            eager_toolsets: true,
            ..Default::default()
        })
        .await
        .unwrap();
        let response = handler
            .handle_message(json!({
                "jsonrpc": "2.0", "id": 84, "method": "tools/call",
                "params": { "name": tool, "arguments": arguments }
            }))
            .await
            .unwrap();
        let result = response.result.unwrap();
        (
            result["isError"] == json!(true),
            result["content"][0]["text"].as_str().unwrap().to_string(),
        )
    }

    fn held_footprint(store: &Mutex<ItemStore>) -> FootprintInstance {
        let held = store.lock().unwrap();
        assert_eq!(held.items.len(), 1, "KiCad holds one footprint");
        FootprintInstance::decode(held.items[0].value.as_slice()).unwrap()
    }

    /// R1 anchored at (100, 100) mm, as KiCad lists it.
    fn r1_at_100_100() -> prost_types::Any {
        pack_any(
            &FootprintInstance {
                id: Some(kiapi::common::types::Kiid {
                    value: "r1".to_string(),
                }),
                position: Some(kiapi::common::types::Vector2 {
                    x_nm: 100_000_000,
                    y_nm: 100_000_000,
                }),
                reference_field: Some(kiapi::board::types::Field {
                    text: Some(kiapi::board::types::BoardText {
                        text: Some(kiapi::common::types::Text {
                            text: "R1".to_string(),
                            ..Default::default()
                        }),
                        ..Default::default()
                    }),
                    ..Default::default()
                }),
                definition: Some(kiapi::board::types::Footprint::default()),
                ..Default::default()
            },
            "kiapi.board.types.FootprintInstance",
        )
    }

    /// The issue's coordinate reached by arithmetic, `128.2 - 5.9`
    /// (122.29999999999998), and `-4.1` (−4099999.9999999995 nm) arrive as
    /// 122.3 and −4.1 mm exactly, through both tools that move a footprint.
    /// Truncation sent 122299999 and −4099999.
    #[tokio::test]
    async fn a_moved_footprint_reaches_kicad_on_the_nanometre_asked_for() {
        let x = 128.2 - 5.9;
        let y = -4.1;
        for (tool, arguments) in [
            (
                "move_component",
                json!({ "reference": "R1", "x": x, "y": y }),
            ),
            (
                "set_component_placements",
                json!({ "placements": [{ "reference": "R1", "x": x, "y": y, "rotation": 0.0 }] }),
            ),
        ] {
            let dir = tempfile::tempdir().unwrap();
            let board = dir.path().join("target.kicad_pcb");
            std::fs::write(&board, "(kicad_pcb (version 20260206))").unwrap();
            let store = Arc::new(Mutex::new(ItemStore {
                items: vec![r1_at_100_100()],
                ..Default::default()
            }));
            let mut arguments = arguments;
            arguments["board"] = json!(board.to_string_lossy());

            let (is_error, text) = served(&board, store.clone(), tool, arguments).await;

            assert!(!is_error, "{tool}: {text}");
            assert_eq!(store.lock().unwrap().updates, 1, "{tool}");
            let position = held_footprint(&store).position.unwrap();
            assert_eq!(
                (position.x_nm, position.y_nm),
                (122_300_000, -4_100_000),
                "{tool}"
            );
        }
    }

    /// #238: placing `Resistor_SMD:R_0805_2012Metric`, whose pad is
    /// `(at -0.9125 0) (size 1.025 1.4)`, at x = 25.4 mm. KiCad must receive
    /// the library's 1.025 mm as 1025000 nm (truncation sent 1024999, which
    /// DRC reports as `lib_footprint_mismatch`), and the pad at 25.4 − 0.9125
    /// = 24.4875 mm (24.487499999999997 in f64) as 24487500 nm.
    #[tokio::test]
    async fn a_placed_library_pad_reaches_kicad_at_the_library_size() {
        let dir = tempfile::tempdir().unwrap();
        let board = super::tests::fallback_fixture(dir.path());
        let store = Arc::new(Mutex::new(ItemStore::default()));

        let (is_error, text) = served(
            &board,
            store.clone(),
            "place_component",
            json!({
                "board": board.to_string_lossy(),
                "footprint": "Resistor_SMD:R_0805_2012Metric",
                "reference": "R1",
                "x": 25.4,
                "y": 20.0,
            }),
        )
        .await;

        assert!(!is_error, "{text}");
        assert_eq!(store.lock().unwrap().creates, 1);
        let footprint = held_footprint(&store);
        let anchor = footprint.position.unwrap();
        assert_eq!((anchor.x_nm, anchor.y_nm), (25_400_000, 20_000_000));
        let pads: Vec<Pad> = footprint
            .definition
            .unwrap()
            .items
            .iter()
            .filter(|item| any_is(item, "kiapi.board.types.Pad"))
            .map(|item| Pad::decode(item.value.as_slice()).unwrap())
            .collect();
        assert_eq!(pads.len(), 1, "the fixture's one pad");
        let size = pads[0].pad_stack.as_ref().unwrap().copper_layers[0]
            .size
            .unwrap();
        assert_eq!((size.x_nm, size.y_nm), (1_025_000, 1_400_000));
        let position = pads[0].position.unwrap();
        assert_eq!((position.x_nm, position.y_nm), (24_487_500, 20_000_000));
    }

    /// Helper: write a single-footprint `.pretty` library + `fp-lib-table` + empty
    /// `.kicad_pcb` in `dir`, returning `(board_path, lib_id)`.
    fn issue_318_fixture(
        dir: &std::path::Path,
        nickname: &str,
        entry: &str,
        mod_source: &str,
    ) -> (std::path::PathBuf, String) {
        let pretty = dir.join(format!("{nickname}.pretty"));
        std::fs::create_dir_all(&pretty).unwrap();
        std::fs::write(pretty.join(format!("{entry}.kicad_mod")), mod_source).unwrap();
        std::fs::write(
            dir.join("fp-lib-table"),
            format!(
                "(fp_lib_table\n  (version 7)\n  (lib (name \"{nickname}\")(type \"KiCad\")(uri \"${{KIPRJMOD}}/{nickname}.pretty\")(options \"\")(descr \"\")))\n"
            ),
        )
        .unwrap();
        let board = dir.join("board.kicad_pcb");
        std::fs::write(
            &board,
            "(kicad_pcb (version 20241229) (generator \"pcbnew\"))\n",
        )
        .unwrap();
        (board, format!("{nickname}:{entry}"))
    }

    fn kicad_version_responder(
        major: u32,
        minor: u32,
        patch: u32,
    ) -> impl Fn(
        &prost_types::Any,
        &mut crate::tools::pcb_board::board_mock::ItemStore,
    ) -> Option<prost_types::Any>
           + Send
           + 'static {
        use copperpour_ipc::builders::{any_is, pack_any};
        use copperpour_ipc::gen::kiapi::common::{commands, types::KiCadVersion};
        move |cmd, _| {
            if any_is(cmd, "kiapi.common.commands.GetVersion") {
                Some(pack_any(
                    &commands::GetVersionResponse {
                        version: Some(KiCadVersion {
                            major,
                            minor,
                            patch,
                            full_version: format!("{major}.{minor}.{patch}"),
                        }),
                    },
                    "kiapi.common.commands.GetVersionResponse",
                ))
            } else {
                None
            }
        }
    }

    async fn served_with_version(
        board: &std::path::Path,
        store: std::sync::Arc<std::sync::Mutex<crate::tools::pcb_board::board_mock::ItemStore>>,
        version: (u32, u32, u32),
        tool: &str,
        arguments: serde_json::Value,
    ) -> (bool, String) {
        use crate::tools::pcb_board::board_mock::spawn_kicad_holding_items;
        let server = spawn_kicad_holding_items(
            board,
            vec![],
            store,
            |_| {},
            kicad_version_responder(version.0, version.1, version.2),
        );
        let handler = crate::mcp::handler::McpHandler::new(crate::tools::ServerConfig {
            ipc_address: server.address().to_string(),
            eager_toolsets: true,
            ..Default::default()
        })
        .await
        .unwrap();
        let response = handler
            .handle_message(json!({
                "jsonrpc": "2.0",
                "id": 1,
                "method": "tools/call",
                "params": { "name": tool, "arguments": arguments }
            }))
            .await
            .unwrap();
        let result = response.result.as_ref().unwrap();
        (
            result["isError"].as_bool().unwrap_or(false),
            result["content"][0]["text"]
                .as_str()
                .unwrap_or("")
                .to_string(),
        )
    }

    /// Oracle test (#318): placing stock WSON-6 through `place_component` on
    /// KiCad 10.0.8 carries the exact `fab_property` and `zone_settings`
    /// recorded in `issue_300_u4.ipc.bin`.
    #[tokio::test]
    async fn place_component_carries_pad_properties_and_zone_settings_matching_kicad_oracle() {
        use crate::tools::pcb_board::board_mock::ItemStore;
        use copperpour_ipc::builders::any_is;
        use copperpour_ipc::gen::kiapi::board::types::{FootprintInstance, Pad};
        use std::sync::{Arc, Mutex};

        let dir = tempfile::tempdir().unwrap();
        let (board, lib_id) = issue_318_fixture(
            dir.path(),
            "Package_SON",
            "WSON-6-1EP_2x2mm_P0.65mm_EP1x1.6mm_ThermalVias",
            include_str!("../../tests/fixtures/issue_300_wson6_thermalvias_kicad10.kicad_mod"),
        );
        let store = Arc::new(Mutex::new(ItemStore::default()));
        let (is_error, text) = served_with_version(
            &board,
            store.clone(),
            (10, 0, 8),
            "place_component",
            json!({
                "board": board.to_string_lossy(),
                "footprint": lib_id,
                "reference": "U4",
                "x": 20.0,
                "y": 30.0,
            }),
        )
        .await;
        assert!(!is_error, "{text}");
        let out: serde_json::Value = serde_json::from_str(&text).unwrap();
        assert!(out.get("not_carried").is_none(), "{out}");

        let oracle = FootprintInstance::decode(
            include_bytes!("../../tests/fixtures/issue_300_u2.ipc.bin").as_slice(),
        )
        .unwrap();
        let placed = held_footprint(&store);

        let pad_summary = |fp: &FootprintInstance| {
            let mut v: Vec<(String, i32, Option<i32>)> = fp
                .definition
                .as_ref()
                .unwrap()
                .items
                .iter()
                .filter(|item| any_is(item, "kiapi.board.types.Pad"))
                .map(|item| {
                    let pad = Pad::decode(item.value.as_slice()).unwrap();
                    let zone = pad
                        .pad_stack
                        .as_ref()
                        .and_then(|s| s.zone_settings.as_ref())
                        .map(|z| z.zone_connection);
                    (pad.number, pad.fab_property, zone)
                })
                .collect();
            v.sort();
            v
        };
        assert_eq!(pad_summary(&placed), pad_summary(&oracle));
    }

    /// Oracle test (#318): placing stock DIP-8 through `place_component` on
    /// KiCad 10.0.8 carries the `ReferencePoint` recorded in `issue_283_u2.ipc.bin`.
    #[tokio::test]
    async fn place_component_carries_reference_point_matching_kicad_oracle() {
        use crate::tools::pcb_board::board_mock::ItemStore;
        use copperpour_ipc::builders::any_is;
        use copperpour_ipc::gen::kiapi::board::types::{FootprintInstance, ReferencePoint};
        use std::sync::{Arc, Mutex};

        let dir = tempfile::tempdir().unwrap();
        let (board, lib_id) = issue_318_fixture(
            dir.path(),
            "Package_DIP",
            "DIP-8_W7.62mm",
            include_str!("../../tests/fixtures/issue_283_dip8_kicad10.kicad_mod"),
        );
        let oracle = FootprintInstance::decode(
            include_bytes!("../../tests/fixtures/issue_283_u2.ipc.bin").as_slice(),
        )
        .unwrap();
        let oracle_pos = oracle.position.unwrap();
        let x = copperpour_ipc::builders::nm_to_mm(oracle_pos.x_nm);
        let y = copperpour_ipc::builders::nm_to_mm(oracle_pos.y_nm);

        let store = Arc::new(Mutex::new(ItemStore::default()));
        let (is_error, text) = served_with_version(
            &board,
            store.clone(),
            (10, 0, 8),
            "place_component",
            json!({
                "board": board.to_string_lossy(),
                "footprint": lib_id,
                "reference": "U5",
                "x": x,
                "y": y,
            }),
        )
        .await;
        assert!(!is_error, "{text}");
        let out: serde_json::Value = serde_json::from_str(&text).unwrap();
        assert!(out.get("not_carried").is_none(), "{out}");

        let placed = held_footprint(&store);
        let ref_points = |fp: &FootprintInstance| {
            fp.definition
                .as_ref()
                .unwrap()
                .items
                .iter()
                .filter(|item| any_is(item, "kiapi.board.types.ReferencePoint"))
                .map(|item| {
                    let rp = ReferencePoint::decode(item.value.as_slice()).unwrap();
                    (rp.layer, rp.position, rp.size)
                })
                .collect::<Vec<_>>()
        };
        assert_eq!(ref_points(&placed).len(), 1);
        assert_eq!(ref_points(&placed), ref_points(&oracle));
    }

    /// On KiCad before 10.0.7, `place_component` still places the footprint
    /// (with `zone_settings` carried) and names pad properties / reference
    /// points in `not_carried` with `isError: false`.
    #[tokio::test]
    async fn place_component_before_10_0_7_places_and_reports_not_carried() {
        use crate::tools::pcb_board::board_mock::ItemStore;
        use copperpour_ipc::builders::any_is;
        use copperpour_ipc::gen::kiapi::board::types::{Pad, ZoneConnectionStyle};
        use std::sync::{Arc, Mutex};

        let dir = tempfile::tempdir().unwrap();
        let (board, lib_id) = issue_318_fixture(
            dir.path(),
            "Package_SON",
            "WSON-6-1EP_2x2mm_P0.65mm_EP1x1.6mm_ThermalVias",
            include_str!("../../tests/fixtures/issue_300_wson6_thermalvias_kicad10.kicad_mod"),
        );
        let store = Arc::new(Mutex::new(ItemStore::default()));
        let (is_error, text) = served_with_version(
            &board,
            store.clone(),
            (10, 0, 6),
            "place_component",
            json!({
                "board": board.to_string_lossy(),
                "footprint": lib_id,
                "reference": "U4",
                "x": 10.0,
                "y": 15.0,
            }),
        )
        .await;
        assert!(!is_error, "{text}");
        let out: serde_json::Value = serde_json::from_str(&text).unwrap();
        let not_carried = out["not_carried"].as_array().unwrap();
        assert_eq!(not_carried.len(), 1, "{out}");
        let msg = not_carried[0].as_str().unwrap();
        assert!(msg.contains("10.0.7") && msg.contains("10.0.6"), "{msg}");

        let placed = held_footprint(&store);
        let pads: Vec<Pad> = placed
            .definition
            .unwrap()
            .items
            .iter()
            .filter(|item| any_is(item, "kiapi.board.types.Pad"))
            .map(|item| Pad::decode(item.value.as_slice()).unwrap())
            .collect();
        // Fab properties are omitted on 10.0.6 (`PfpUnknown` = 0), but
        // zone_settings are still carried.
        assert!(pads.iter().all(|p| p.fab_property == 0));
        assert!(pads.iter().any(|p| p
            .pad_stack
            .as_ref()
            .and_then(|s| s.zone_settings.as_ref())
            .is_some_and(|z| z.zone_connection == ZoneConnectionStyle::ZcsFull as i32)));
    }

    /// A footprint with features typed placement cannot carry (footprint zone,
    /// `fp_text user` with a font face) is still placed (`isError: false`) and
    /// every uncarried feature is listed in `not_carried`.
    #[tokio::test]
    async fn place_component_with_unsupported_clauses_places_and_reports_not_carried() {
        use crate::tools::pcb_board::board_mock::ItemStore;
        use std::sync::{Arc, Mutex};

        let source = r#"(footprint "Test_Part"
  (version 20241229)
  (generator "pcbnew")
  (layer "F.Cu")
  (property "Reference" "REF**" (at 0 -2 0) (layer "F.SilkS") (effects (font (size 1 1) (thickness 0.15))))
  (property "Value" "Test_Part" (at 0 2 0) (layer "F.Fab") (effects (font (size 1 1) (thickness 0.15))))
  (fp_text user "NOTE" (at 0 0 0) (layer "F.SilkS") (effects (font (face "Arial") (size 1 1) (thickness 0.15)) (justify left)))
  (pad "1" smd rect (at -1 0) (size 1 1) (layers "F.Cu" "F.Mask"))
  (zone (net 0) (net_name "") (layer "F.Cu"))
)"#;
        let dir = tempfile::tempdir().unwrap();
        let (board, lib_id) = issue_318_fixture(dir.path(), "Test", "Test_Part", source);
        let store = Arc::new(Mutex::new(ItemStore::default()));
        let (is_error, text) = served_with_version(
            &board,
            store.clone(),
            (10, 0, 8),
            "place_component",
            json!({
                "board": board.to_string_lossy(),
                "footprint": lib_id,
                "reference": "U1",
                "x": 5.0,
                "y": 5.0,
            }),
        )
        .await;
        assert!(!is_error, "{text}");
        let out: serde_json::Value = serde_json::from_str(&text).unwrap();
        let not_carried: Vec<&str> = out["not_carried"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_str().unwrap())
            .collect();
        let joined = not_carried.join(" | ");
        assert!(joined.contains("font clause 'face'"), "{joined}");
        assert!(!joined.contains("justify"), "{joined}");
        assert!(joined.contains("zone"), "{joined}");
    }

    /// `place_component` gives the footprint the library's own solder mask
    /// and paste margins, and each pad its own on both outer layers, as
    /// KiCad's parser sets them; a margin of 0 is an override, and a pad
    /// without one gets none. KiCad reads both since 10.0.0, unlike pad
    /// fabrication properties, so the margins go to 10.0.6 too.
    #[tokio::test]
    async fn place_component_carries_pad_and_footprint_solder_margins() {
        for version in [(10, 0, 6), (10, 0, 8)] {
            for (name, source, own, pad_9) in SOLDER_MARGIN_FOOTPRINTS {
                let name = format!("{name} on {version:?}");
                let dir = tempfile::tempdir().unwrap();
                let (board, lib_id) = issue_318_fixture(dir.path(), "Margins", "Margins", source);
                let store = Arc::new(Mutex::new(ItemStore::default()));
                let (is_error, text) = served_with_version(
                    &board,
                    store.clone(),
                    version,
                    "place_component",
                    json!({
                        "board": board.to_string_lossy(),
                        "footprint": lib_id,
                        "reference": "U1",
                        "x": 10.0,
                        "y": 10.0,
                    }),
                )
                .await;
                assert!(!is_error, "{name}: {text}");
                let out: serde_json::Value = serde_json::from_str(&text).unwrap();
                assert!(
                    !out["not_carried"].to_string().contains("solder"),
                    "{name}: {out}"
                );
                // On 10.0.6, pad 9's heatsink property, which needs 10.0.7,
                // is listed.
                if version == (10, 0, 8) {
                    assert!(out.get("not_carried").is_none(), "{name}: {out}");
                }
                assert_eq!(
                    placed_margins(&held_footprint(&store)),
                    expected_margins(own, pad_9),
                    "{name}"
                );
            }
        }
    }

    /// `place_component_array` and `duplicate_component` build their
    /// footprints as `place_component` does, margins included.
    #[tokio::test]
    async fn array_and_duplicate_carry_solder_margins() {
        for (name, source, own, pad_9) in SOLDER_MARGIN_FOOTPRINTS {
            let dir = tempfile::tempdir().unwrap();
            let (board, lib_id) = issue_318_fixture(dir.path(), "Margins", "Margins", source);
            let store = Arc::new(Mutex::new(ItemStore::default()));
            for (tool, arguments) in [
                (
                    "place_component_array",
                    json!({
                        "board": board.to_string_lossy(),
                        "footprint": lib_id,
                        "start_x": 10.0,
                        "start_y": 10.0,
                        "count_x": 2,
                        "count_y": 1,
                        "spacing_x": 5.0,
                        "ref_prefix": "U",
                        "ref_start": 1,
                    }),
                ),
                (
                    "duplicate_component",
                    json!({
                        "board": board.to_string_lossy(),
                        "reference": "U1",
                        "new_reference": "U9",
                        "x": 30.0,
                        "y": 10.0,
                    }),
                ),
            ] {
                let (is_error, text) =
                    served_with_version(&board, store.clone(), (10, 0, 8), tool, arguments).await;
                assert!(!is_error, "{name} {tool}: {text}");
                let out: serde_json::Value = serde_json::from_str(&text).unwrap();
                assert!(out.get("not_carried").is_none(), "{name} {tool}: {out}");
            }
            let items = store.lock().unwrap().items.clone();
            assert_eq!(items.len(), 3, "{name}");
            for item in &items {
                let footprint = FootprintInstance::decode(item.value.as_slice()).unwrap();
                assert_eq!(
                    placed_margins(&footprint),
                    expected_margins(own, pad_9),
                    "{name}"
                );
            }
        }
    }

    /// A part for B.Cu is built on F.Cu with the library's margins and then
    /// turned over by KiCad's own `FlipItems`. The stand-in flip changes only
    /// the side, so what this shows is that the margins are built before the
    /// flip and nothing after it replaces them.
    #[tokio::test]
    async fn place_component_on_b_cu_carries_solder_margins() {
        for (name, source, own, pad_9) in SOLDER_MARGIN_FOOTPRINTS {
            let dir = tempfile::tempdir().unwrap();
            let (board, lib_id) = issue_318_fixture(dir.path(), "Margins", "Margins", source);
            let store = Arc::new(Mutex::new(ItemStore::default()));
            let flips = Arc::new(Mutex::new(0usize));
            let flips_in_mock = flips.clone();
            let version = kicad_version_responder(10, 0, 8);
            // KiCad's flip reduced to the side, which is all this checks.
            let server = spawn_kicad_holding_items(
                &board,
                vec![],
                store.clone(),
                |_| {},
                move |command, held| {
                    if let Some(reply) = version(command, held) {
                        return Some(reply);
                    }
                    if !command.type_url.ends_with("kiapi.board.commands.FlipItems") {
                        return None;
                    }
                    *flips_in_mock.lock().unwrap() += 1;
                    let request =
                        kiapi::board::commands::FlipItems::decode(command.value.as_slice())
                            .unwrap();
                    let mut flipped_items = Vec::new();
                    for id in request.items {
                        let slot = held
                            .items
                            .iter_mut()
                            .find(|item| {
                                crate::tools::pcb_board::board_mock::item_kiid(item).as_deref()
                                    == Some(id.value.as_str())
                            })
                            .unwrap();
                        let mut footprint =
                            FootprintInstance::decode(slot.value.as_slice()).unwrap();
                        footprint.layer = kiapi::board::types::BoardLayer::BlBCu as i32;
                        *slot = pack_any(&footprint, "kiapi.board.types.FootprintInstance");
                        flipped_items.push(kiapi::board::commands::ItemFlipResult {
                            status: Some(kiapi::common::commands::ItemStatus {
                                code: kiapi::common::commands::ItemStatusCode::IscOk as i32,
                                error_message: String::new(),
                            }),
                            item: Some(slot.clone()),
                        });
                    }
                    Some(pack_any(
                        &kiapi::board::commands::FlipItemsResponse {
                            header: None,
                            status: kiapi::common::types::ItemRequestStatus::IrsOk as i32,
                            flipped_items,
                        },
                        "kiapi.board.commands.FlipItemsResponse",
                    ))
                },
            );
            let handler = crate::mcp::handler::McpHandler::new(crate::tools::ServerConfig {
                ipc_address: server.address().to_string(),
                eager_toolsets: true,
                ..Default::default()
            })
            .await
            .unwrap();
            let response = handler
                .handle_message(json!({
                    "jsonrpc": "2.0",
                    "id": 1,
                    "method": "tools/call",
                    "params": {
                        "name": "place_component",
                        "arguments": {
                            "board": board.to_string_lossy(),
                            "footprint": lib_id,
                            "reference": "U1",
                            "x": 10.0,
                            "y": 10.0,
                            "rotation": 90.0,
                            "layer": "B.Cu"
                        }
                    }
                }))
                .await
                .unwrap();
            let result = response.result.unwrap();
            assert!(
                !result["isError"].as_bool().unwrap_or(false),
                "{name}: {result}"
            );
            assert_eq!(*flips.lock().unwrap(), 1, "{name}");
            let placed = held_footprint(&store);
            assert_eq!(
                placed.layer,
                kiapi::board::types::BoardLayer::BlBCu as i32,
                "{name}"
            );
            assert_eq!(
                placed_margins(&placed),
                expected_margins(own, pad_9),
                "{name}"
            );
        }
    }

    /// A footprint with no margins of its own or on its pads is sent as
    /// before: no overrides, and no pad outer layers.
    #[tokio::test]
    async fn a_footprint_without_solder_margins_is_placed_as_before() {
        let dir = tempfile::tempdir().unwrap();
        let (board, lib_id) = issue_318_fixture(
            dir.path(),
            "Margins",
            "Margins",
            include_str!("../../tests/fixtures/solder_margins_none_kicad10.kicad_mod"),
        );
        let store = Arc::new(Mutex::new(ItemStore::default()));
        let (is_error, text) = served_with_version(
            &board,
            store.clone(),
            (10, 0, 8),
            "place_component",
            json!({
                "board": board.to_string_lossy(),
                "footprint": lib_id,
                "reference": "U1",
                "x": 10.0,
                "y": 10.0,
            }),
        )
        .await;
        assert!(!is_error, "{text}");
        let placed = held_footprint(&store);
        assert_eq!(placed.overrides, None);
        for item in &placed.definition.unwrap().items {
            if any_is(item, "kiapi.board.types.Pad") {
                let pad = Pad::decode(item.value.as_slice()).unwrap();
                let stack = pad.pad_stack.unwrap();
                assert_eq!(stack.front_outer_layers, None, "pad '{}'", pad.number);
                assert_eq!(stack.back_outer_layers, None, "pad '{}'", pad.number);
            }
        }
    }

    /// Inline because KiCad writes no such clause: a pad's or a footprint's
    /// margin that is not one number refuses the footprint, named, before
    /// anything is sent to KiCad.
    #[tokio::test]
    async fn place_component_refuses_a_malformed_solder_margin() {
        for (owner, source, clause) in [
            (
                "pad",
                SOLDER_MARGIN_FOOTPRINTS[0].1,
                "(solder_mask_margin -0.05)",
            ),
            (
                "footprint",
                SOLDER_MARGIN_FOOTPRINTS[2].1,
                "(solder_mask_margin 0.05)",
            ),
        ] {
            assert_eq!(source.matches(clause).count(), 1, "{owner}");
            let source = source.replace(clause, "(solder_mask_margin x)");
            let dir = tempfile::tempdir().unwrap();
            let (board, lib_id) = issue_318_fixture(dir.path(), "Margins", "Margins", &source);
            let store = Arc::new(Mutex::new(ItemStore::default()));
            let (is_error, text) = served_with_version(
                &board,
                store.clone(),
                (10, 0, 8),
                "place_component",
                json!({
                    "board": board.to_string_lossy(),
                    "footprint": lib_id,
                    "reference": "U1",
                    "x": 10.0,
                    "y": 10.0,
                }),
            )
            .await;
            assert!(is_error, "{owner}: {text}");
            assert!(
                text.contains(&format!(
                    "{owner} clause 'solder_mask_margin' has an invalid value 'x'"
                )),
                "{owner}: {text}"
            );
            let held = store.lock().unwrap();
            assert_eq!((held.creates, held.items.len()), (0, 0), "{owner}");
        }
    }

    /// Sibling routes (#318): `place_component_array` and `duplicate_component`
    /// carry pad properties, zone settings and reference points, and report
    /// `not_carried` when connected to KiCad < 10.0.7.
    #[tokio::test]
    async fn array_and_duplicate_carry_pad_properties_and_zone_settings() {
        use crate::tools::pcb_board::board_mock::ItemStore;
        use copperpour_ipc::builders::any_is;
        use copperpour_ipc::gen::kiapi::board::types::{
            FootprintInstance, Pad, PadFabricationProperty, ZoneConnectionStyle,
        };
        use std::sync::{Arc, Mutex};

        let dir = tempfile::tempdir().unwrap();
        let (board, lib_id) = issue_318_fixture(
            dir.path(),
            "Package_SON",
            "WSON-6-1EP_2x2mm_P0.65mm_EP1x1.6mm_ThermalVias",
            include_str!("../../tests/fixtures/issue_300_wson6_thermalvias_kicad10.kicad_mod"),
        );
        let store = Arc::new(Mutex::new(ItemStore::default()));
        let (is_error, text) = served_with_version(
            &board,
            store.clone(),
            (10, 0, 8),
            "place_component_array",
            json!({
                "board": board.to_string_lossy(),
                "footprint": lib_id,
                "start_x": 10.0,
                "start_y": 10.0,
                "count_x": 2,
                "count_y": 1,
                "spacing_x": 5.0,
                "ref_prefix": "U",
                "ref_start": 1,
            }),
        )
        .await;
        assert!(!is_error, "{text}");
        let out: serde_json::Value = serde_json::from_str(&text).unwrap();
        assert_eq!(out["placed_count"], 2);
        assert!(out.get("not_carried").is_none(), "{out}");

        // Now duplicate U1 as U9.
        let (is_error, text) = served_with_version(
            &board,
            store.clone(),
            (10, 0, 8),
            "duplicate_component",
            json!({
                "board": board.to_string_lossy(),
                "reference": "U1",
                "new_reference": "U9",
                "x": 30.0,
                "y": 10.0,
            }),
        )
        .await;
        assert!(!is_error, "{text}");
        let out: serde_json::Value = serde_json::from_str(&text).unwrap();
        assert!(out.get("not_carried").is_none(), "{out}");

        let items = store.lock().unwrap().items.clone();
        assert_eq!(items.len(), 3);
        for item in &items {
            let fp = FootprintInstance::decode(item.value.as_slice()).unwrap();
            let pads: Vec<Pad> = fp
                .definition
                .unwrap()
                .items
                .iter()
                .filter(|child| any_is(child, "kiapi.board.types.Pad"))
                .map(|child| Pad::decode(child.value.as_slice()).unwrap())
                .collect();
            assert_eq!(
                pads.iter()
                    .filter(|p| p.fab_property == PadFabricationProperty::PfpHeatsink as i32)
                    .count(),
                4
            );
            assert_eq!(
                pads.iter()
                    .filter(|p| p
                        .pad_stack
                        .as_ref()
                        .and_then(|s| s.zone_settings.as_ref())
                        .is_some_and(|z| z.zone_connection == ZoneConnectionStyle::ZcsFull as i32))
                    .count(),
                2
            );
        }
    }

    /// Sibling route (#318): `repair_corrupted_footprints` preserves/restores
    /// pad fabrication properties and pad zone settings from the library.
    /// A KiCad holding a WSON-6 footprint with issue #244's corruption, and
    /// the handler talking to it. `saved` builds, from the footprint's
    /// reference, the board KiCad answers `SaveDocumentToString` with.
    async fn corrupted_footprint_kicad(
        saved: Option<fn(&str) -> String>,
    ) -> (
        tempfile::TempDir,
        std::path::PathBuf,
        std::sync::Arc<std::sync::Mutex<crate::tools::pcb_board::board_mock::ItemStore>>,
        crate::test_support::MockIpcServer,
        crate::mcp::handler::McpHandler,
    ) {
        use crate::tools::pcb_board::board_mock::{spawn_kicad_holding_items, ItemStore};
        use copperpour_ipc::builders::{any_is, pack_any};
        use copperpour_ipc::gen::kiapi;
        use copperpour_ipc::gen::kiapi::board::types::{FootprintInstance, Pad};
        use std::sync::{Arc, Mutex};

        let dir = tempfile::tempdir().unwrap();
        let (board, lib_id) = issue_318_fixture(
            dir.path(),
            "Package_SON",
            "WSON-6-1EP_2x2mm_P0.65mm_EP1x1.6mm_ThermalVias",
            include_str!("../../tests/fixtures/issue_300_wson6_thermalvias_kicad10.kicad_mod"),
        );
        // Build a corrupted U4 from the oracle by turning one BoardGraphicShape
        // into an anonymous layerless Pad (issue #244 signature).
        let mut corrupted = FootprintInstance::decode(
            include_bytes!("../../tests/fixtures/issue_300_u2.ipc.bin").as_slice(),
        )
        .unwrap();
        corrupted.id = Some(copperpour_ipc::gen::kiapi::common::types::Kiid {
            value: "fp-u4".to_string(),
        });
        let def = corrupted.definition.as_mut().unwrap();
        let shape_idx = def
            .items
            .iter()
            .position(|item| any_is(item, "kiapi.board.types.BoardGraphicShape"))
            .unwrap();
        def.items.remove(shape_idx);
        let phantom = Pad {
            id: Some(copperpour_ipc::gen::kiapi::common::types::Kiid {
                value: "phantom-1".to_string(),
            }),
            number: String::new(),
            pad_stack: Some(copperpour_ipc::gen::kiapi::board::types::PadStack {
                copper_layers: vec![],
                ..Default::default()
            }),
            ..Default::default()
        };
        def.items.push(pack_any(&phantom, "kiapi.board.types.Pad"));

        let store = Arc::new(Mutex::new(ItemStore {
            items: vec![pack_any(&corrupted, "kiapi.board.types.FootprintInstance")],
            ..Default::default()
        }));
        let server = spawn_kicad_holding_items(&board, vec![], store.clone(), |_| {}, {
            let version = kicad_version_responder(10, 0, 8);
            let saved = saved.map(|saved| saved(&super::footprint_instance_reference(&corrupted)));
            move |command: &prost_types::Any, store: &mut ItemStore| {
                version(command, store).or_else(|| {
                    let saved = saved
                        .as_ref()
                        .filter(|_| command.type_url.ends_with("SaveDocumentToString"))?;
                    let request = kiapi::common::commands::SaveDocumentToString::decode(
                        command.value.as_slice(),
                    )
                    .unwrap();
                    Some(pack_any(
                        &kiapi::common::commands::SavedDocumentResponse {
                            document: request.document,
                            contents: saved.clone(),
                        },
                        "kiapi.common.commands.SavedDocumentResponse",
                    ))
                })
            }
        });
        let handler = crate::mcp::handler::McpHandler::new(crate::tools::ServerConfig {
            ipc_address: server.address().to_string(),
            eager_toolsets: true,
            ..Default::default()
        })
        .await
        .unwrap();
        let _ = lib_id;
        (dir, board, store, server, handler)
    }

    #[tokio::test]
    async fn repair_corrupted_footprints_carries_pad_properties_and_zone_settings() {
        use copperpour_ipc::builders::any_is;
        use copperpour_ipc::gen::kiapi::board::types::{
            Pad, PadFabricationProperty, ZoneConnectionStyle,
        };

        let (_dir, board, store, _server, handler) = corrupted_footprint_kicad(None).await;
        let dry = handler
            .handle_message(json!({
                "jsonrpc": "2.0",
                "id": 1,
                "method": "tools/call",
                "params": {
                    "name": "repair_corrupted_footprints",
                    "arguments": { "board": board.to_string_lossy(), "dry_run": true }
                }
            }))
            .await
            .unwrap();
        let dry_out: serde_json::Value = serde_json::from_str(
            dry.result.as_ref().unwrap()["content"][0]["text"]
                .as_str()
                .unwrap(),
        )
        .unwrap();
        assert_eq!(dry_out["status"], "ready", "{dry_out}");
        let rev = dry_out["plan_revision"].as_str().unwrap();

        let applied = handler
            .handle_message(json!({
                "jsonrpc": "2.0",
                "id": 2,
                "method": "tools/call",
                "params": {
                    "name": "repair_corrupted_footprints",
                    "arguments": {
                        "board": board.to_string_lossy(),
                        "dry_run": false,
                        "expected_plan_revision": rev
                    }
                }
            }))
            .await
            .unwrap();
        let applied_out: serde_json::Value = serde_json::from_str(
            applied.result.as_ref().unwrap()["content"][0]["text"]
                .as_str()
                .unwrap(),
        )
        .unwrap();
        assert_eq!(applied_out["status"], "applied", "{applied_out}");

        let repaired = held_footprint(&store);
        let pads: Vec<Pad> = repaired
            .definition
            .unwrap()
            .items
            .iter()
            .filter(|child| any_is(child, "kiapi.board.types.Pad"))
            .map(|child| Pad::decode(child.value.as_slice()).unwrap())
            .collect();
        assert_eq!(
            pads.iter()
                .filter(|p| p.fab_property == PadFabricationProperty::PfpHeatsink as i32)
                .count(),
            4
        );
        assert_eq!(
            pads.iter()
                .filter(|p| p
                    .pad_stack
                    .as_ref()
                    .and_then(|s| s.zone_settings.as_ref())
                    .is_some_and(|z| z.zone_connection == ZoneConnectionStyle::ZcsFull as i32))
                .count(),
            2
        );
    }

    /// `repair_corrupted_footprints` gives the pads it rebuilds their
    /// library's solder margins: the stock PQFN-8's paste margin on pads 1 to
    /// 4, on both outer layers (`issue_300_ipc.README.md`), on KiCad 10.0.6
    /// and 10.0.8. The board's pads are stripped of theirs first, so the
    /// margins can only come from the library. Inline because KiCad writes no
    /// such clause: a malformed margin in the library ends the call before
    /// anything is sent, as a malformed `zone_connect` does.
    #[tokio::test]
    async fn repair_corrupted_footprints_carries_pad_solder_margins() {
        use crate::tools::pcb_board::board_mock::{spawn_kicad_holding_items, ItemStore};
        use copperpour_ipc::builders::{any_is, pack_any};
        use copperpour_ipc::gen::kiapi::board::types::{FootprintInstance, Pad, PadStack};
        use copperpour_ipc::gen::kiapi::common::types::Kiid;
        use std::sync::{Arc, Mutex};

        const PQFN8: &str = include_str!("../../tests/fixtures/issue_300_pqfn8_kicad10.kicad_mod");

        /// The repair's dry run and, when it is ready, its apply, against
        /// `library`, on KiCad `version` holding its own U6 with the #244
        /// signature, as the test above builds U4, and its pads' margins
        /// removed.
        async fn repair(
            library: &str,
            version: (u32, u32, u32),
        ) -> (Arc<Mutex<ItemStore>>, Vec<(bool, String)>) {
            let dir = tempfile::tempdir().unwrap();
            let (board, _) = issue_318_fixture(
                dir.path(),
                "Package_DFN_QFN",
                "PQFN-8-EP_6x5mm_P1.27mm_Generic",
                library,
            );
            let mut corrupted = FootprintInstance::decode(
                include_bytes!("../../tests/fixtures/issue_300_u6.ipc.bin").as_slice(),
            )
            .unwrap();
            let definition = corrupted.definition.as_mut().unwrap();
            for item in &mut definition.items {
                if any_is(item, "kiapi.board.types.Pad") {
                    let mut pad = Pad::decode(item.value.as_slice()).unwrap();
                    let stack = pad.pad_stack.as_mut().unwrap();
                    stack.front_outer_layers = None;
                    stack.back_outer_layers = None;
                    *item = pack_any(&pad, "kiapi.board.types.Pad");
                }
            }
            let shape = definition
                .items
                .iter()
                .position(|item| any_is(item, "kiapi.board.types.BoardGraphicShape"))
                .unwrap();
            definition.items.remove(shape);
            let phantom = Pad {
                id: Some(Kiid {
                    value: "phantom-1".to_string(),
                }),
                pad_stack: Some(PadStack::default()),
                ..Default::default()
            };
            definition
                .items
                .push(pack_any(&phantom, "kiapi.board.types.Pad"));

            let store = Arc::new(Mutex::new(ItemStore {
                items: vec![pack_any(&corrupted, "kiapi.board.types.FootprintInstance")],
                ..Default::default()
            }));
            let server = spawn_kicad_holding_items(
                &board,
                vec![],
                store.clone(),
                |_| {},
                kicad_version_responder(version.0, version.1, version.2),
            );
            let handler = crate::mcp::handler::McpHandler::new(crate::tools::ServerConfig {
                ipc_address: server.address().to_string(),
                eager_toolsets: true,
                ..Default::default()
            })
            .await
            .unwrap();
            let mut replies = Vec::new();
            let mut revision = None;
            for id in [1, 2] {
                let mut arguments = json!({ "board": board.to_string_lossy(), "dry_run": id == 1 });
                if let Some(revision) = &revision {
                    arguments["expected_plan_revision"] = json!(revision);
                }
                let response = handler
                    .handle_message(json!({
                        "jsonrpc": "2.0",
                        "id": id,
                        "method": "tools/call",
                        "params": { "name": "repair_corrupted_footprints", "arguments": arguments }
                    }))
                    .await
                    .unwrap();
                let result = response.result.unwrap();
                let is_error = result["isError"].as_bool().unwrap_or(false);
                let text = result["content"][0]["text"].as_str().unwrap().to_string();
                replies.push((is_error, text.clone()));
                if is_error {
                    break;
                }
                let out: serde_json::Value = serde_json::from_str(&text).unwrap();
                revision = out["plan_revision"].as_str().map(str::to_string);
            }
            (store, replies)
        }

        for version in [(10, 0, 6), (10, 0, 8)] {
            let (store, replies) = repair(PQFN8, version).await;
            assert_eq!(replies.len(), 2, "{version:?}: {replies:?}");
            for ((is_error, text), status) in replies.iter().zip(["ready", "applied"]) {
                assert!(!is_error, "{version:?}: {text}");
                let out: serde_json::Value = serde_json::from_str(text).unwrap();
                assert_eq!(out["status"], status, "{version:?}: {out}");
                assert!(
                    !out["not_carried"].to_string().contains("solder"),
                    "{version:?}: {out}"
                );
            }
            assert_eq!(
                placed_margins(&held_footprint(&store)),
                (
                    NO_MARGINS,
                    ["1", "2", "3", "4"]
                        .map(|number| (number.to_string(), (None, Some(-50_000), None)))
                        .to_vec()
                ),
                "{version:?}"
            );
        }

        let clause = "(solder_paste_margin -0.05)";
        assert_eq!(PQFN8.matches(clause).count(), 4);
        let malformed = PQFN8.replacen(clause, "(solder_paste_margin x)", 1);
        let (store, replies) = repair(&malformed, (10, 0, 8)).await;
        assert_eq!(replies.len(), 1, "{replies:?}");
        let (is_error, text) = &replies[0];
        assert!(is_error, "{text}");
        assert!(
            text.contains("pad clause 'solder_paste_margin' has an invalid value 'x'"),
            "{text}"
        );
        assert_eq!(store.lock().unwrap().updates, 0);
    }

    /// #701: a footprint to repair that owns a group is a conflict, at the dry
    /// run, and nothing is sent: the repair sends the whole footprint back,
    /// which KiCad 10.0.8 rebuilds without its groups.
    #[tokio::test]
    async fn repair_corrupted_footprints_refuses_a_footprint_that_owns_a_group() {
        fn grouped(reference: &str) -> String {
            format!(
                "(kicad_pcb\n\t(version 20260206)\n\t(generator \"pcbnew\")\n\t(footprint \
                 \"Package_SON:WSON-6\"\n\t\t(layer \"F.Cu\")\n\t\t(uuid \"fp-u4\")\n\t\t\
                 (property \"Reference\" \"{reference}\")\n\t\t(group \"\" (uuid \"g-701\") \
                 (members))\n\t)\n)\n"
            )
        }
        let (_dir, board, store, _server, handler) = corrupted_footprint_kicad(Some(grouped)).await;
        let dry = handler
            .handle_message(json!({
                "jsonrpc": "2.0",
                "id": 1,
                "method": "tools/call",
                "params": {
                    "name": "repair_corrupted_footprints",
                    "arguments": { "board": board.to_string_lossy(), "dry_run": true }
                }
            }))
            .await
            .unwrap();
        let dry_out: serde_json::Value = serde_json::from_str(
            dry.result.as_ref().unwrap()["content"][0]["text"]
                .as_str()
                .unwrap(),
        )
        .unwrap();
        assert_eq!(dry_out["status"], "conflict", "{dry_out}");
        let diagnostic = &dry_out["diagnostics"][0];
        assert_eq!(diagnostic["code"], "footprint_children_not_carried");
        let reference = diagnostic["reference"].as_str().unwrap();
        assert_eq!(
            diagnostic["message"],
            format!(
                "{reference} owns 1 group, which KiCad drops when its API updates a footprint; \
                 update {reference} in KiCad's PCB editor instead"
            )
        );
        let store = store.lock().unwrap();
        assert_eq!((store.updates, store.creates, store.deletes), (0, 0, 0));
    }

    // ─── #85: net-tie pad groups ────────────────────────────────────────────

    /// KiCad 10.0.8's stock net ties (`net_tie_kicad10.README.md`): each one's
    /// library name, text and one group.
    const NET_TIE_FOOTPRINTS: [(&str, &str, &[&str]); 3] = [
        (
            "NetTie-2_SMD_Pad0.5mm",
            include_str!("../../tests/fixtures/net_tie_2_smd_pad0.5mm_kicad10.kicad_mod"),
            &["1", "2"],
        ),
        (
            "NetTie-3_THT_Pad0.3mm",
            include_str!("../../tests/fixtures/net_tie_3_tht_pad0.3mm_kicad10.kicad_mod"),
            &["1", "2", "3"],
        ),
        (
            "NetTie-4_SMD_Pad0.5mm",
            include_str!("../../tests/fixtures/net_tie_4_smd_pad0.5mm_kicad10.kicad_mod"),
            &["1", "2", "3", "4"],
        ),
    ];

    /// Each net-tie group `footprint` holds, as pad numbers.
    fn tie_groups(footprint: &FootprintInstance) -> Vec<Vec<String>> {
        footprint
            .definition
            .as_ref()
            .unwrap()
            .net_ties
            .iter()
            .map(|group| group.pad_number.clone())
            .collect()
    }

    /// `place_component` gives each stock net tie its library group on KiCad
    /// 10.0.1 and later, and lists nothing as not carried.
    #[tokio::test]
    async fn place_component_carries_net_tie_pad_groups() {
        for version in [(10, 0, 1), (10, 0, 8)] {
            for (entry, source, group) in NET_TIE_FOOTPRINTS {
                let dir = tempfile::tempdir().unwrap();
                let (board, lib_id) = issue_318_fixture(dir.path(), "NetTie", entry, source);
                let store = Arc::new(Mutex::new(ItemStore::default()));
                let (is_error, text) = served_with_version(
                    &board,
                    store.clone(),
                    version,
                    "place_component",
                    json!({
                        "board": board.to_string_lossy(),
                        "footprint": lib_id,
                        "reference": "NT1",
                        "x": 10.0,
                        "y": 10.0,
                    }),
                )
                .await;
                assert!(!is_error, "{entry} on {version:?}: {text}");
                let out: Value = serde_json::from_str(&text).unwrap();
                assert!(
                    out.get("not_carried").is_none(),
                    "{entry} on {version:?}: {out}"
                );
                assert_eq!(
                    tie_groups(&held_footprint(&store)),
                    [group.to_vec()],
                    "{entry} on {version:?}"
                );
            }
        }
    }

    /// Oracle test (#85): placing the stock Antenova antenna on KiCad 10.0.8
    /// sends the net-tie group KiCad reported for it in `issue_300_u8.ipc.bin`.
    /// Its keepout zone is still left out, and listed; its group is not.
    #[tokio::test]
    async fn place_component_carries_the_net_tie_kicad_reports() {
        let dir = tempfile::tempdir().unwrap();
        let (board, lib_id) = issue_318_fixture(
            dir.path(),
            "RF_Antenna",
            "Antenova_SR4G013_GPS",
            include_str!("../../tests/fixtures/issue_300_antenova_sr4g013_kicad10.kicad_mod"),
        );
        let store = Arc::new(Mutex::new(ItemStore::default()));
        let (is_error, text) = served_with_version(
            &board,
            store.clone(),
            (10, 0, 8),
            "place_component",
            json!({
                "board": board.to_string_lossy(),
                "footprint": lib_id,
                "reference": "U8",
                "x": 170.0,
                "y": 115.0,
            }),
        )
        .await;
        assert!(!is_error, "{text}");
        let out: Value = serde_json::from_str(&text).unwrap();
        assert!(out["not_carried"].to_string().contains("zone"), "{out}");
        assert!(!out["not_carried"].to_string().contains("net_tie"), "{out}");
        let oracle = FootprintInstance::decode(
            include_bytes!("../../tests/fixtures/issue_300_u8.ipc.bin").as_slice(),
        )
        .unwrap();
        assert_eq!(
            held_footprint(&store).definition.unwrap().net_ties,
            oracle.definition.unwrap().net_ties
        );
    }

    /// On KiCad 10.0.0, which would keep each group as one pad,
    /// `place_component` places a net tie without its group and names the
    /// group in `not_carried`, as it does pad properties before 10.0.7.
    #[tokio::test]
    async fn place_component_on_kicad_10_0_0_reports_net_ties_not_carried() {
        let (entry, source, _) = NET_TIE_FOOTPRINTS[1];
        let dir = tempfile::tempdir().unwrap();
        let (board, lib_id) = issue_318_fixture(dir.path(), "NetTie", entry, source);
        let store = Arc::new(Mutex::new(ItemStore::default()));
        let (is_error, text) = served_with_version(
            &board,
            store.clone(),
            (10, 0, 0),
            "place_component",
            json!({
                "board": board.to_string_lossy(),
                "footprint": lib_id,
                "reference": "NT1",
                "x": 10.0,
                "y": 10.0,
            }),
        )
        .await;
        assert!(!is_error, "{text}");
        let out: Value = serde_json::from_str(&text).unwrap();
        assert_eq!(
            out["not_carried"],
            json!([
                "net-tie pad groups (\"1, 2, 3\") require KiCad 10.0.1 or newer (connected KiCad \
                 is 10.0.0)"
            ]),
            "{out}"
        );
        assert!(tie_groups(&held_footprint(&store)).is_empty());
    }

    /// `place_component_array` and `duplicate_component` build their
    /// footprints as `place_component` does, net ties included.
    #[tokio::test]
    async fn array_and_duplicate_carry_net_tie_pad_groups() {
        let (entry, source, group) = NET_TIE_FOOTPRINTS[1];
        let dir = tempfile::tempdir().unwrap();
        let (board, lib_id) = issue_318_fixture(dir.path(), "NetTie", entry, source);
        let store = Arc::new(Mutex::new(ItemStore::default()));
        for (tool, arguments) in [
            (
                "place_component_array",
                json!({
                    "board": board.to_string_lossy(),
                    "footprint": lib_id,
                    "start_x": 10.0,
                    "start_y": 10.0,
                    "count_x": 2,
                    "count_y": 1,
                    "spacing_x": 5.0,
                    "ref_prefix": "NT",
                    "ref_start": 1,
                }),
            ),
            (
                "duplicate_component",
                json!({
                    "board": board.to_string_lossy(),
                    "reference": "NT1",
                    "new_reference": "NT9",
                    "x": 30.0,
                    "y": 10.0,
                }),
            ),
        ] {
            let (is_error, text) =
                served_with_version(&board, store.clone(), (10, 0, 8), tool, arguments).await;
            assert!(!is_error, "{tool}: {text}");
            let out: Value = serde_json::from_str(&text).unwrap();
            assert!(out.get("not_carried").is_none(), "{tool}: {out}");
        }
        let items = store.lock().unwrap().items.clone();
        assert_eq!(items.len(), 3);
        for item in &items {
            let footprint = FootprintInstance::decode(item.value.as_slice()).unwrap();
            assert_eq!(tie_groups(&footprint), [group.to_vec()]);
        }
    }

    /// A net-tie group placement could not send as the library writes it
    /// refuses the part before anything reaches KiCad, naming why.
    #[tokio::test]
    async fn place_component_refuses_a_malformed_net_tie_group() {
        let (entry, source, _) = NET_TIE_FOOTPRINTS[1];
        let malformed = source.replace(
            "(net_tie_pad_groups \"1, 2, 3\")",
            "(net_tie_pad_groups \"1, 4\")",
        );
        assert_ne!(malformed, source, "the edit must land");
        let dir = tempfile::tempdir().unwrap();
        let (board, lib_id) = issue_318_fixture(dir.path(), "NetTie", entry, &malformed);
        let store = Arc::new(Mutex::new(ItemStore::default()));
        let (is_error, text) = served_with_version(
            &board,
            store.clone(),
            (10, 0, 8),
            "place_component",
            json!({
                "board": board.to_string_lossy(),
                "footprint": lib_id,
                "reference": "NT1",
                "x": 10.0,
                "y": 10.0,
            }),
        )
        .await;
        assert!(is_error, "{text}");
        assert!(
            text.contains("names pad '4', which the footprint does not have"),
            "{text}"
        );
        assert!(store.lock().unwrap().items.is_empty());
    }
}

/// `edit_component` against a mock KiCad holding KiCad's own report of a
/// stock `C_0603_1608Metric` (`c_0603_loaded_front_0.ipc.bin`).
#[cfg(test)]
mod edit_component_tests {
    use super::*;
    use copperpour_ipc::gen::kiapi;
    use std::sync::{Arc, Mutex};

    const C_0603: &[u8] = include_bytes!("../../tests/fixtures/c_0603_loaded_front_0.ipc.bin");

    #[test]
    fn edit_component_describes_the_unit_mapping_risk() {
        let tool = tools()
            .into_iter()
            .find(|tool| tool.name == "edit_component")
            .unwrap();
        assert!(tool
            .description
            .contains("drop symbol-unit mappings on KiCad 10.0–10.0.7"));
        assert!(tool.description.contains("Troubleshooting"));
    }

    /// What the mock does with an `UpdateItems`.
    #[derive(Clone, Copy, PartialEq)]
    enum Kicad {
        /// Stores the footprint it was sent.
        Keeps,
        /// Answers OK but keeps the flags it held, as a KiCad that ignored
        /// the attributes would.
        IgnoresFlags,
        /// Answers OK but keeps the lock it held.
        IgnoresLock,
        /// Answers OK but keeps the reference it held.
        IgnoresReference,
        /// Refuses the update and stores nothing.
        Refuses,
        /// Lists the footprint twice, as a board with a repeated reference.
        ListsTwice,
        MissingItemsResponse,
        WrongItemsResponse,
        UnreadableFootprint,
        RefusesItemsRequest,
        RefusesItemUpdate,
    }

    #[derive(Default)]
    struct Sent {
        commands: Vec<String>,
        begins: usize,
        updates: Vec<kiapi::board::types::FootprintInstance>,
        /// Each board command in order, with the document its header named.
        named: Vec<(
            &'static str,
            Option<kiapi::common::types::DocumentSpecifier>,
        )>,
    }

    /// The board the mock holds open, as its `GetOpenDocuments` reports it.
    fn open_board(board: &str) -> kiapi::common::types::DocumentSpecifier {
        kiapi::common::types::DocumentSpecifier {
            r#type: kiapi::common::types::DocumentType::DoctypePcb as i32,
            project: None,
            identifier: Some(
                kiapi::common::types::document_specifier::Identifier::BoardFilename(
                    board.to_string(),
                ),
            ),
        }
    }

    /// The command and the document named by a request that carries an
    /// `ItemHeader`, or `None` for one that names no document.
    fn named_document(
        command: &prost_types::Any,
    ) -> Option<(
        &'static str,
        Option<kiapi::common::types::DocumentSpecifier>,
    )> {
        use kiapi::common::commands as c;
        let bytes = command.value.as_slice();
        let url = command.type_url.as_str();
        let (name, header) = if url.ends_with(".GetItems") {
            ("GetItems", c::GetItems::decode(bytes).unwrap().header)
        } else if url.ends_with(".BeginCommit") {
            ("BeginCommit", c::BeginCommit::decode(bytes).unwrap().header)
        } else if url.ends_with(".UpdateItems") {
            ("UpdateItems", c::UpdateItems::decode(bytes).unwrap().header)
        } else if url.ends_with(".EndCommit") {
            ("EndCommit", c::EndCommit::decode(bytes).unwrap().header)
        } else {
            return None;
        };
        Some((name, header.and_then(|header| header.document)))
    }

    /// Every board command named the board the call passed.
    fn assert_named_the_board(sent: &Sent, board: &str) {
        let open = open_board(board);
        for (command, document) in &sent.named {
            assert_eq!(
                document.as_ref(),
                Some(&open),
                "{command} named another document than the board"
            );
        }
    }

    fn c_0603(linked: bool) -> kiapi::board::types::FootprintInstance {
        let mut footprint = kiapi::board::types::FootprintInstance::decode(C_0603).unwrap();
        if linked {
            footprint.symbol_path = Some(kiapi::common::types::SheetPath {
                path: vec![kiapi::common::types::Kiid {
                    value: "11111111-1111-1111-1111-111111111111".to_string(),
                }],
                path_human_readable: String::new(),
            });
        }
        footprint
    }

    fn ok(message: Option<prost_types::Any>) -> kiapi::common::ApiResponse {
        kiapi::common::ApiResponse {
            status: Some(kiapi::common::ApiResponseStatus {
                status: kiapi::common::ApiStatusCode::AsOk as i32,
                error_message: String::new(),
            }),
            header: None,
            message,
        }
    }

    fn bad_request(error_message: String) -> kiapi::common::ApiResponse {
        kiapi::common::ApiResponse {
            status: Some(kiapi::common::ApiResponseStatus {
                status: kiapi::common::ApiStatusCode::AsBadRequest as i32,
                error_message,
            }),
            header: None,
            message: None,
        }
    }

    fn spawn(
        board: &Path,
        footprint: kiapi::board::types::FootprintInstance,
        kicad: Kicad,
    ) -> (crate::test_support::MockIpcServer, Arc<Mutex<Sent>>) {
        spawn_many(board, vec![footprint], kicad)
    }

    fn spawn_many(
        board: &Path,
        footprints: Vec<kiapi::board::types::FootprintInstance>,
        kicad: Kicad,
    ) -> (crate::test_support::MockIpcServer, Arc<Mutex<Sent>>) {
        use copperpour_ipc::builders::pack_any;

        let open = open_board(&board.to_string_lossy());
        let held = Mutex::new(footprints);
        let sent = Arc::new(Mutex::new(Sent::default()));
        let record = sent.clone();
        let server = crate::test_support::MockIpcServer::spawn("edit-component", move |request| {
            let command = request.message.expect("a command");
            let url = command.type_url.as_str();
            record
                .lock()
                .unwrap()
                .commands
                .push(url.rsplit('.').next().unwrap_or(url).to_owned());
            // KiCad 10.0.8 refuses a request for a board it does not have open
            // (`API_HANDLER_EDITOR::validateItemHeaderDocument`).
            if let Some((name, document)) = named_document(&command) {
                record.lock().unwrap().named.push((name, document.clone()));
                if document.as_ref() != Some(&open) {
                    return bad_request(format!("the requested document {document:?} is not open"));
                }
            }
            if url.ends_with("GetOpenDocuments") {
                ok(Some(pack_any(
                    &kiapi::common::commands::GetOpenDocumentsResponse {
                        documents: vec![open.clone()],
                    },
                    "kiapi.common.commands.GetOpenDocumentsResponse",
                )))
            } else if url.ends_with("GetItems") {
                if kicad == Kicad::MissingItemsResponse {
                    return ok(None);
                }
                if kicad == Kicad::WrongItemsResponse {
                    return ok(Some(pack_any(
                        &kiapi::common::commands::GetItemsResponse {
                            status: kiapi::common::types::ItemRequestStatus::IrsOk as i32,
                            ..Default::default()
                        },
                        "kiapi.common.commands.CreateItemsResponse",
                    )));
                }
                let items = if kicad == Kicad::UnreadableFootprint {
                    vec![prost_types::Any {
                        type_url: "type.googleapis.com/kiapi.board.types.FootprintInstance".into(),
                        value: vec![0xff],
                    }]
                } else {
                    let held = held.lock().unwrap();
                    let packed: Vec<_> = held
                        .iter()
                        .map(|fp| pack_any(fp, "kiapi.board.types.FootprintInstance"))
                        .collect();
                    if kicad == Kicad::ListsTwice {
                        let mut doubled = packed.clone();
                        doubled.extend(packed);
                        doubled
                    } else {
                        packed
                    }
                };
                ok(Some(pack_any(
                    &kiapi::common::commands::GetItemsResponse {
                        header: None,
                        status: if kicad == Kicad::RefusesItemsRequest {
                            kiapi::common::types::ItemRequestStatus::IrsDocumentNotFound as i32
                        } else {
                            kiapi::common::types::ItemRequestStatus::IrsOk as i32
                        },
                        items,
                    },
                    "kiapi.common.commands.GetItemsResponse",
                )))
            } else if url.ends_with("BeginCommit") {
                record.lock().unwrap().begins += 1;
                ok(Some(pack_any(
                    &kiapi::common::commands::BeginCommitResponse {
                        id: Some(kiapi::common::types::Kiid {
                            value: "commit-1".to_string(),
                        }),
                    },
                    "kiapi.common.commands.BeginCommitResponse",
                )))
            } else if url.ends_with("UpdateItems") {
                let update =
                    kiapi::common::commands::UpdateItems::decode(command.value.as_slice()).unwrap();
                let mut footprint = kiapi::board::types::FootprintInstance::decode(
                    update.items[0].value.as_slice(),
                )
                .unwrap();
                record.lock().unwrap().updates.push(footprint.clone());
                if kicad == Kicad::Refuses {
                    return bad_request("mock refusal".to_string());
                }
                if kicad == Kicad::RefusesItemUpdate {
                    return ok(Some(pack_any(
                        &kiapi::common::commands::UpdateItemsResponse {
                            status: kiapi::common::types::ItemRequestStatus::IrsOk as i32,
                            updated_items: vec![kiapi::common::commands::ItemUpdateResult {
                                status: Some(kiapi::common::commands::ItemStatus {
                                    code: kiapi::common::commands::ItemStatusCode::IscInvalidData
                                        as i32,
                                    error_message: "mock item refusal".into(),
                                }),
                                item: None,
                            }],
                            ..Default::default()
                        },
                        "kiapi.common.commands.UpdateItemsResponse",
                    )));
                }
                let mut held = held.lock().unwrap();
                let target_id = footprint.id.as_ref().map(|id| id.value.as_str());
                let slot = held
                    .iter_mut()
                    .find(|existing| existing.id.as_ref().map(|id| id.value.as_str()) == target_id)
                    .expect("mock holds updated footprint");
                if kicad == Kicad::IgnoresFlags {
                    footprint.attributes = slot.attributes.clone();
                }
                if kicad == Kicad::IgnoresLock {
                    footprint.locked = slot.locked;
                }
                if kicad == Kicad::IgnoresReference {
                    footprint.reference_field = slot.reference_field.clone();
                    if let (Some(def), Some(old_def)) =
                        (footprint.definition.as_mut(), slot.definition.as_ref())
                    {
                        def.reference_field = old_def.reference_field.clone();
                    }
                }
                *slot = footprint.clone();
                ok(Some(pack_any(
                    &kiapi::common::commands::UpdateItemsResponse {
                        header: None,
                        status: kiapi::common::types::ItemRequestStatus::IrsOk as i32,
                        updated_items: vec![kiapi::common::commands::ItemUpdateResult {
                            status: Some(kiapi::common::commands::ItemStatus {
                                code: kiapi::common::commands::ItemStatusCode::IscOk as i32,
                                error_message: String::new(),
                            }),
                            item: Some(pack_any(&footprint, "kiapi.board.types.FootprintInstance")),
                        }],
                    },
                    "kiapi.common.commands.UpdateItemsResponse",
                )))
            } else if url.ends_with("EndCommit") {
                ok(Some(pack_any(
                    &kiapi::common::commands::EndCommitResponse {},
                    "kiapi.common.commands.EndCommitResponse",
                )))
            } else {
                ok(crate::tools::pcb_board::board_mock::empty_saved_board(
                    &command,
                ))
            }
        });
        (server, sent)
    }

    fn context(address: String) -> ToolContext {
        ToolContext::new(
            crate::tools::ServerConfig {
                kicad_cli: String::new(),
                kicad_binary: String::new(),
                ipc_address: address,
                project_dir: None,
                jlcpcb_db_path: None,
                auto_load_toolsets: false,
                eager_toolsets: false,
            },
            std::sync::Arc::new(crate::router::ToolRouter::new()),
        )
    }

    fn text(result: &CallToolResult) -> String {
        result
            .content
            .iter()
            .filter_map(|content| match content {
                crate::mcp::protocol::ToolContent::Text { text } => Some(text.as_str()),
                _ => None,
            })
            .collect()
    }

    /// The commands a refused edit sends: the groups check (#701) reads the
    /// board before the footprint is looked up.
    const EDIT_READS: [&str; 3] = ["GetOpenDocuments", "SaveDocumentToString", "GetItems"];
    /// The commands a refused call without an edit sends.
    const NO_EDIT_READS: [&str; 2] = ["GetOpenDocuments", "GetItems"];

    fn assert_preflight(
        body: &serde_json::Value,
        reason: &str,
        sent: &Mutex<Sent>,
        commands: &[&str],
    ) {
        assert_eq!(sent.lock().unwrap().commands, commands);
        assert_eq!(body["message"], reason);
        assert_eq!(body["error"]["kind"], "handler_error");
        let recovery = &body["recovery"];
        assert_eq!(recovery["cause"], "preflight_refusal", "{body}");
        assert_eq!(recovery["mutation_outcome"], "none");
        assert_eq!(recovery["retry_safe"], false);
        assert_eq!(recovery["evidence"]["reason"], reason);
        assert!(recovery.get("troubleshooting_url").is_none());
    }

    fn assert_edit_preflight(
        result: &CallToolResult,
        reason: &str,
        sent: &Mutex<Sent>,
        commands: &[&str],
    ) {
        assert!(result.is_error, "{}", text(result));
        assert_preflight(
            &serde_json::from_str(&text(result)).unwrap(),
            reason,
            sent,
            commands,
        );
    }

    /// Run `edit_component` with `arguments` (beside `board` and the
    /// capture's reference) against a mock KiCad holding `footprint`.
    async fn edit(
        footprint: kiapi::board::types::FootprintInstance,
        kicad: Kicad,
        arguments: serde_json::Value,
    ) -> (CallToolResult, Arc<Mutex<Sent>>, String) {
        let temp = tempfile::tempdir().unwrap();
        let board = temp.path().join("board.kicad_pcb");
        std::fs::write(&board, "(kicad_pcb (version 20240108))").unwrap();
        let reference = footprint_instance_reference(&footprint);
        let (server, sent) = spawn(&board, footprint, kicad);
        let mut args = json!({ "board": board.to_string_lossy(), "reference": reference });
        for (key, value) in arguments.as_object().unwrap() {
            args[key] = value.clone();
        }
        let result = handle_edit_component(&args, &context(server.address().to_string()))
            .await
            .unwrap();
        assert_named_the_board(&sent.lock().unwrap(), &board.to_string_lossy());
        (result, sent, reference)
    }

    #[tokio::test]
    async fn the_flags_are_set_in_one_commit_and_read_back() {
        let before = c_0603(false);
        let (result, sent, reference) = edit(
            before.clone(),
            Kicad::Keeps,
            json!({ "dnp": true, "exclude_from_bom": true, "exclude_from_pos_files": true }),
        )
        .await;
        assert!(!result.is_error, "{}", text(&result));
        let body: serde_json::Value = serde_json::from_str(&text(&result)).unwrap();
        assert_eq!(body["status"], "applied");
        assert_eq!(body["reference"], reference);
        assert_eq!(body["footprint"], "Capacitor_SMD:C_0603_1608Metric");
        assert_eq!(
            (
                body["dnp"].clone(),
                body["exclude_from_bom"].clone(),
                body["exclude_from_pos_files"].clone()
            ),
            (json!(true), json!(true), json!(true))
        );
        assert_eq!(
            body["changed"],
            json!(["dnp", "exclude_from_bom", "exclude_from_pos_files"])
        );
        assert_eq!(body["before"]["dnp"], false);
        assert_eq!(body["undo"], "one KiCad undo step");
        let warning = body["warning"].as_str().unwrap();
        for expected in [
            "drop symbol-unit mappings on KiCad 10.0–10.0.7.",
            "rewrite rule areas inside footprints",
            "Readback verifies neither",
            "TROUBLESHOOTING.md#footprint-unit-metadata-disappears-after-a-live-pcb-edit",
        ] {
            assert!(
                warning.contains(expected),
                "missing {expected:?}: {warning}"
            );
        }
        assert!(
            !warning.contains("update_pcb_from_schematic"),
            "the capture is not linked to a symbol: {body}"
        );

        let sent = sent.lock().unwrap();
        assert_eq!(sent.begins, 1, "one KiCad undo step");
        // `edit` asserted that each of these named the board the call passed.
        let commands: Vec<_> = sent.named.iter().map(|(command, _)| *command).collect();
        assert_eq!(
            commands,
            [
                "GetItems",
                "BeginCommit",
                "UpdateItems",
                "EndCommit",
                "GetItems"
            ]
        );
        assert_eq!(sent.updates.len(), 1);
        let attributes = sent.updates[0].attributes.clone().unwrap();
        assert!(attributes.do_not_populate);
        assert!(attributes.exclude_from_bill_of_materials);
        assert!(attributes.exclude_from_position_files);
        // Only the flags changed: the rest of KiCad's message went back as sent.
        let mut unchanged = sent.updates[0].clone();
        unchanged.attributes = before.attributes.clone();
        assert_eq!(unchanged, before);
    }

    #[tokio::test]
    async fn the_value_and_a_flag_change_together() {
        let (result, sent, _) = edit(
            c_0603(false),
            Kicad::Keeps,
            json!({ "value": "1u", "dnp": true }),
        )
        .await;
        assert!(!result.is_error, "{}", text(&result));
        let body: serde_json::Value = serde_json::from_str(&text(&result)).unwrap();
        assert_eq!(body["value"], "1u");
        assert_eq!(body["dnp"], true);
        assert_eq!(body["changed"], json!(["value", "dnp"]));
        assert_eq!(sent.lock().unwrap().begins, 1);
    }

    #[tokio::test]
    async fn a_request_the_footprint_already_holds_sends_nothing() {
        let (result, sent, _) = edit(
            c_0603(false),
            Kicad::Keeps,
            json!({ "dnp": false, "exclude_from_bom": false }),
        )
        .await;
        assert!(!result.is_error, "{}", text(&result));
        let body: serde_json::Value = serde_json::from_str(&text(&result)).unwrap();
        assert_eq!(body["status"], "unchanged");
        assert_eq!(body["changed"], json!([]));
        assert_eq!(body["warning"], serde_json::Value::Null);
        let sent = sent.lock().unwrap();
        assert_eq!((sent.begins, sent.updates.len()), (0, 0));
    }

    #[tokio::test]
    async fn no_property_reports_the_footprint_without_a_commit() {
        let (result, sent, reference) = edit(c_0603(false), Kicad::Keeps, json!({})).await;
        assert!(!result.is_error, "{}", text(&result));
        let body: serde_json::Value = serde_json::from_str(&text(&result)).unwrap();
        assert_eq!(body["status"], "unchanged");
        assert_eq!(body["reference"], reference);
        assert_eq!(body["dnp"], false);
        let sent = sent.lock().unwrap();
        assert_eq!(sent.begins, 0);
        let commands: Vec<_> = sent.named.iter().map(|(command, _)| *command).collect();
        assert_eq!(commands, ["GetItems"]);
    }

    #[tokio::test]
    async fn a_flag_kicad_did_not_keep_is_an_uncertain_outcome() {
        let (result, sent, _) =
            edit(c_0603(false), Kicad::IgnoresFlags, json!({ "dnp": true })).await;
        assert!(result.is_error);
        assert_eq!(
            crate::mcp::error::extract_error_kind(&result).as_deref(),
            Some("mutation_outcome_uncertain")
        );
        assert!(
            text(&result).contains("dnp: expected true, observed false"),
            "{}",
            text(&result)
        );
        assert_eq!(sent.lock().unwrap().begins, 1);
    }

    /// The capture as KiCad reports it once locked.
    fn locked_c_0603(linked: bool) -> kiapi::board::types::FootprintInstance {
        let mut footprint = c_0603(linked);
        footprint.locked = kiapi::common::types::LockedState::LsLocked as i32;
        footprint
    }

    #[tokio::test]
    async fn the_lock_is_set_in_one_commit_and_read_back() {
        let before = c_0603(false);
        assert_eq!(
            before.locked,
            kiapi::common::types::LockedState::LsUnlocked as i32,
            "premise: KiCad reported the capture unlocked"
        );
        let (result, sent, _) = edit(before.clone(), Kicad::Keeps, json!({ "locked": true })).await;
        assert!(!result.is_error, "{}", text(&result));
        let body: serde_json::Value = serde_json::from_str(&text(&result)).unwrap();
        assert_eq!(body["status"], "applied");
        assert_eq!(body["locked"], true);
        assert_eq!(body["before"]["locked"], false);
        assert_eq!(body["changed"], json!(["locked"]));
        assert_eq!(body["undo"], "one KiCad undo step");

        let sent = sent.lock().unwrap();
        assert_eq!(sent.begins, 1, "one KiCad undo step");
        assert_eq!(sent.updates.len(), 1);
        assert_eq!(
            sent.updates[0].locked,
            kiapi::common::types::LockedState::LsLocked as i32
        );
        // Only the lock changed: the rest of KiCad's message went back as sent.
        let mut unchanged = sent.updates[0].clone();
        unchanged.locked = before.locked;
        assert_eq!(unchanged, before);
    }

    #[tokio::test]
    async fn the_lock_is_cleared_without_a_warning_about_the_sync() {
        let (result, sent, _) = edit(
            locked_c_0603(true),
            Kicad::Keeps,
            json!({ "locked": false }),
        )
        .await;
        assert!(!result.is_error, "{}", text(&result));
        let body: serde_json::Value = serde_json::from_str(&text(&result)).unwrap();
        assert_eq!(body["locked"], false);
        assert_eq!(body["changed"], json!(["locked"]));
        assert_eq!(
            sent.lock().unwrap().updates[0].locked,
            kiapi::common::types::LockedState::LsUnlocked as i32
        );
        // update_pcb_from_schematic keeps the board's lock when it updates a
        // footprint (`update_footprint_item`), so a linked footprint's lock
        // needs no warning about the sync.
        let warning = body["warning"].as_str().unwrap();
        assert!(!warning.contains("update_pcb_from_schematic"), "{warning}");
    }

    #[tokio::test]
    async fn a_lock_the_footprint_already_holds_sends_nothing() {
        let (result, sent, _) = edit(
            locked_c_0603(false),
            Kicad::Keeps,
            json!({ "locked": true }),
        )
        .await;
        assert!(!result.is_error, "{}", text(&result));
        let body: serde_json::Value = serde_json::from_str(&text(&result)).unwrap();
        assert_eq!(body["status"], "unchanged");
        assert_eq!(body["locked"], true);
        let sent = sent.lock().unwrap();
        assert_eq!((sent.begins, sent.updates.len()), (0, 0));
    }

    #[tokio::test]
    async fn no_property_reports_the_lock_without_a_commit() {
        let (result, sent, _) = edit(locked_c_0603(false), Kicad::Keeps, json!({})).await;
        assert!(!result.is_error, "{}", text(&result));
        let body: serde_json::Value = serde_json::from_str(&text(&result)).unwrap();
        assert_eq!(body["locked"], true);
        assert_eq!(sent.lock().unwrap().begins, 0);
    }

    #[tokio::test]
    async fn a_lock_kicad_did_not_keep_is_an_uncertain_outcome() {
        let (result, sent, _) =
            edit(c_0603(false), Kicad::IgnoresLock, json!({ "locked": true })).await;
        assert!(result.is_error);
        assert_eq!(
            crate::mcp::error::extract_error_kind(&result).as_deref(),
            Some("mutation_outcome_uncertain")
        );
        assert!(
            text(&result).contains("locked: expected true, observed false"),
            "{}",
            text(&result)
        );
        assert_eq!(sent.lock().unwrap().begins, 1);
    }

    #[tokio::test]
    async fn an_update_kicad_refused_is_an_error_not_an_uncertain_outcome() {
        let (result, _, _) = edit(c_0603(false), Kicad::Refuses, json!({ "dnp": true })).await;
        assert!(result.is_error);
        assert_ne!(
            crate::mcp::error::extract_error_kind(&result).as_deref(),
            Some("mutation_outcome_uncertain"),
            "a fresh read proved nothing changed: {}",
            text(&result)
        );
        assert!(text(&result).contains("rejected"), "{}", text(&result));
        let body: serde_json::Value = serde_json::from_str(&text(&result)).unwrap();
        assert_eq!(body["recovery"]["cause"], "kicad_api_rejection", "{body}");
        assert_eq!(body["recovery"]["evidence"]["status"], "AS_BAD_REQUEST");
        assert_eq!(body["recovery"]["mutation_outcome"], "unknown");
        assert_eq!(body["recovery"]["retry_safe"], false);
    }

    #[tokio::test]
    async fn an_item_request_rejection_is_from_kicad_not_copperpour() {
        let (result, sent, _) = edit(
            c_0603(false),
            Kicad::RefusesItemsRequest,
            json!({ "dnp": true }),
        )
        .await;
        assert!(result.is_error);
        let body: serde_json::Value = serde_json::from_str(&text(&result)).unwrap();
        assert_eq!(body["recovery"]["cause"], "kicad_api_rejection", "{body}");
        assert_eq!(
            body["recovery"]["evidence"]["status"],
            "IRS_DOCUMENT_NOT_FOUND"
        );
        assert_eq!(sent.lock().unwrap().commands, EDIT_READS);
    }

    #[tokio::test]
    async fn a_per_item_update_rejection_is_from_kicad_not_copperpour() {
        let (result, sent, _) = edit(
            c_0603(false),
            Kicad::RefusesItemUpdate,
            json!({ "dnp": true }),
        )
        .await;
        assert!(result.is_error);
        let body: serde_json::Value = serde_json::from_str(&text(&result)).unwrap();
        assert_eq!(body["recovery"]["cause"], "kicad_api_rejection", "{body}");
        assert_eq!(body["recovery"]["evidence"]["status"], "ISC_INVALID_DATA");
        assert_eq!(sent.lock().unwrap().updates.len(), 1);
    }

    #[tokio::test]
    async fn a_linked_footprint_warns_that_the_sync_sets_dnp_again() {
        let (result, _, _) = edit(
            c_0603(true),
            Kicad::Keeps,
            json!({ "dnp": true, "exclude_from_pos_files": true }),
        )
        .await;
        assert!(!result.is_error, "{}", text(&result));
        let body: serde_json::Value = serde_json::from_str(&text(&result)).unwrap();
        assert_eq!(body["linked_to_schematic"], true);
        let warning = body["warning"].as_str().unwrap();
        assert!(
            warning.contains("update_pcb_from_schematic sets dnp from the symbol"),
            "{warning}"
        );
        assert!(
            !warning.contains("exclude_from_pos_files"),
            "the sync does not set the position-file exclusion: {warning}"
        );
    }

    #[tokio::test]
    async fn a_missing_reference_is_refused_before_a_commit() {
        let temp = tempfile::tempdir().unwrap();
        let board = temp.path().join("board.kicad_pcb");
        let saved = b"(kicad_pcb (version 20240108))";
        std::fs::write(&board, saved).unwrap();
        let (server, sent) = spawn(&board, c_0603(false), Kicad::Keeps);
        let mut config = context(server.address().to_string()).config;
        config.eager_toolsets = true;
        let handler = crate::mcp::handler::McpHandler::new(config).await.unwrap();
        let result = handler
            .handle_message(json!({
                "jsonrpc": "2.0",
                "id": 1,
                "method": "tools/call",
                "params": {
                    "name": "edit_component",
                    "arguments": { "board": board, "reference": "R99", "dnp": true },
                }
            }))
            .await
            .unwrap()
            .result
            .unwrap();
        assert_eq!(result["isError"], true);
        let body = serde_json::from_str(result["content"][0]["text"].as_str().unwrap()).unwrap();
        assert_preflight(&body, "Footprint 'R99' not found", &sent, &EDIT_READS);
        assert_eq!(std::fs::read(board).unwrap(), saved);
    }

    #[tokio::test]
    async fn a_missing_reference_is_reported_when_no_edit_is_requested() {
        let (result, sent, _) =
            edit(c_0603(false), Kicad::Keeps, json!({ "reference": "R99" })).await;
        assert_edit_preflight(&result, "Footprint 'R99' not found", &sent, &NO_EDIT_READS);
    }

    async fn assert_incomplete_footprint_inventory(kicad: Kicad) {
        let (result, sent, _) = edit(c_0603(false), kicad, json!({ "dnp": true })).await;
        let body: serde_json::Value = serde_json::from_str(&text(&result)).unwrap();
        let reason = body["message"].as_str().unwrap();
        assert!(
            reason.starts_with("Cannot read footprint inventory:"),
            "{body}"
        );
        assert_edit_preflight(&result, reason, &sent, &EDIT_READS);
    }

    #[tokio::test]
    async fn a_missing_inventory_payload_is_not_a_missing_reference() {
        assert_incomplete_footprint_inventory(Kicad::MissingItemsResponse).await;
    }

    #[tokio::test]
    async fn an_unexpected_inventory_reply_type_is_not_a_missing_reference() {
        assert_incomplete_footprint_inventory(Kicad::WrongItemsResponse).await;
    }

    #[tokio::test]
    async fn an_unreadable_footprint_is_not_a_missing_reference() {
        assert_incomplete_footprint_inventory(Kicad::UnreadableFootprint).await;
    }

    /// Current callers keep their answer: `reference`, `value` and
    /// `footprint` at the top level, the library id written as
    /// `list_footprints` writes it, whether a property was named or not, and
    /// for a footprint whose library id is empty as well.
    #[tokio::test]
    async fn the_answer_keeps_the_keys_it_had() {
        let captured_value = c_0603(false)
            .value_field
            .and_then(|field| field.text)
            .and_then(|text| text.text)
            .map(|text| text.text)
            .unwrap();
        for (arguments, value) in [
            (json!({}), captured_value.as_str()),
            (json!({ "value": "1u" }), "1u"),
        ] {
            let (result, _, reference) = edit(c_0603(false), Kicad::Keeps, arguments.clone()).await;
            assert!(!result.is_error, "{}", text(&result));
            let body: serde_json::Value = serde_json::from_str(&text(&result)).unwrap();
            assert_eq!(body["reference"], reference, "{arguments}");
            assert_eq!(body["value"], value, "{arguments}");
            assert_eq!(
                body["footprint"], "Capacitor_SMD:C_0603_1608Metric",
                "{arguments}"
            );
        }

        let mut unnamed = c_0603(false);
        unnamed.definition.as_mut().unwrap().id = Some(Default::default());
        let (result, _, _) = edit(unnamed, Kicad::Keeps, json!({})).await;
        assert!(!result.is_error, "{}", text(&result));
        let body: serde_json::Value = serde_json::from_str(&text(&result)).unwrap();
        assert_eq!(
            body["footprint"], ":",
            "as list_footprints writes an empty id"
        );
    }

    /// A value edit of a footprint without a value field is refused with the
    /// text it was refused with before, and nothing is sent.
    #[tokio::test]
    async fn a_footprint_without_a_value_field_refuses_a_value_edit() {
        let mut footprint = c_0603(false);
        footprint.value_field = None;
        let (result, sent, reference) =
            edit(footprint, Kicad::Keeps, json!({ "value": "1u" })).await;
        assert_edit_preflight(
            &result,
            &format!("Footprint '{reference}' has no editable value field"),
            &sent,
            &EDIT_READS,
        );
    }

    #[tokio::test]
    async fn a_footprint_without_a_kiid_refuses_an_edit() {
        let mut footprint = c_0603(false);
        footprint.id = None;
        let (result, sent, reference) = edit(footprint, Kicad::Keeps, json!({ "dnp": true })).await;
        assert_edit_preflight(
            &result,
            &format!("footprint '{reference}' has no KIID"),
            &sent,
            &EDIT_READS,
        );
    }

    async fn assert_repeated_reference(arguments: serde_json::Value) {
        let commands: &[&str] = if arguments == json!({}) {
            &NO_EDIT_READS
        } else {
            &EDIT_READS
        };
        let (result, sent, reference) = edit(c_0603(false), Kicad::ListsTwice, arguments).await;
        assert_edit_preflight(
            &result,
            &format!("Footprint reference '{reference}' is ambiguous: KiCad returned 2 matches"),
            &sent,
            commands,
        );
    }

    #[tokio::test]
    async fn a_repeated_reference_is_refused_before_a_commit() {
        assert_repeated_reference(json!({ "dnp": true })).await;
    }

    #[tokio::test]
    async fn a_repeated_reference_is_refused_when_no_edit_is_requested() {
        assert_repeated_reference(json!({})).await;
    }

    fn c_0603_with(
        kiid: &str,
        reference: &str,
        linked: bool,
    ) -> kiapi::board::types::FootprintInstance {
        let mut footprint = c_0603(linked);
        footprint.id = Some(kiapi::common::types::Kiid {
            value: kiid.to_string(),
        });
        footprint
            .reference_field
            .as_mut()
            .unwrap()
            .text
            .as_mut()
            .unwrap()
            .text
            .as_mut()
            .unwrap()
            .text = reference.to_string();
        if let Some(text) = footprint
            .definition
            .as_mut()
            .and_then(|definition| definition.reference_field.as_mut())
            .and_then(|field| field.text.as_mut())
            .and_then(|board_text| board_text.text.as_mut())
        {
            text.text = reference.to_string();
        }
        footprint
    }

    #[tokio::test]
    async fn a_board_owned_footprint_is_renamed_in_one_commit_and_read_back_by_kiid() {
        // Also covers KiCad's empty SheetPath (`[""]`) emitted for board-only
        // footprints placed directly on the board.
        let mut before = c_0603(false);
        before.symbol_path = Some(kiapi::common::types::SheetPath {
            path: vec![kiapi::common::types::Kiid {
                value: String::new(),
            }],
            path_human_readable: "/".to_string(),
        });
        let original_kiid = before.id.as_ref().unwrap().value.clone();
        let (result, sent, original_ref) = edit(
            before.clone(),
            Kicad::Keeps,
            json!({ "new_reference": "LOGO1" }),
        )
        .await;
        assert!(!result.is_error, "{}", text(&result));
        let body: serde_json::Value = serde_json::from_str(&text(&result)).unwrap();
        assert_eq!(body["status"], "applied");
        assert_eq!(body["kiid"], original_kiid);
        assert_eq!(body["reference"], "LOGO1");
        assert_eq!(body["before"]["reference"], original_ref);
        assert_eq!(body["changed"], json!(["reference"]));
        assert_eq!(body["linked_to_schematic"], false);
        assert_eq!(body["undo"], "one KiCad undo step");

        let sent = sent.lock().unwrap();
        assert_eq!(sent.begins, 1);
        let commands: Vec<_> = sent.named.iter().map(|(command, _)| *command).collect();
        assert_eq!(
            commands,
            [
                "GetItems",
                "BeginCommit",
                "UpdateItems",
                "EndCommit",
                "GetItems"
            ]
        );
        assert_eq!(sent.updates.len(), 1);
        assert_eq!(footprint_instance_reference(&sent.updates[0]), "LOGO1");
        // Only the reference field changed; the rest of KiCad's message went back verbatim.
        let mut unchanged = sent.updates[0].clone();
        unchanged.reference_field = before.reference_field.clone();
        if let (Some(def), Some(before_def)) =
            (unchanged.definition.as_mut(), before.definition.as_ref())
        {
            def.reference_field = before_def.reference_field.clone();
        }
        assert_eq!(unchanged, before);
    }

    #[tokio::test]
    async fn a_schematic_linked_footprint_refuses_board_side_rename_before_commit() {
        let (result, sent, reference) = edit(
            c_0603(true),
            Kicad::Keeps,
            json!({ "new_reference": "C99" }),
        )
        .await;
        assert!(result.is_error, "{}", text(&result));
        assert_eq!(
            crate::mcp::error::extract_error_kind(&result).as_deref(),
            Some("plan_blocked"),
            "{}",
            text(&result)
        );
        let msg = text(&result);
        assert!(msg.contains(&reference), "{msg}");
        assert!(msg.contains("edit_schematic_component"), "{msg}");
        assert!(msg.contains("update_pcb_from_schematic"), "{msg}");
        let sent = sent.lock().unwrap();
        assert_eq!((sent.begins, sent.updates.len()), (0, 0));
    }

    #[tokio::test]
    async fn a_rename_to_an_existing_reference_is_refused_before_commit() {
        let temp = tempfile::tempdir().unwrap();
        let board = temp.path().join("board.kicad_pcb");
        std::fs::write(&board, "(kicad_pcb (version 20240108))").unwrap();
        let fp1 = c_0603_with("fp-1", "LOGO1", false);
        let fp2 = c_0603_with("fp-2", "FID1", false);
        let (server, sent) = spawn_many(&board, vec![fp1, fp2], Kicad::Keeps);
        let result = handle_edit_component(
            &json!({
                "board": board.to_string_lossy(),
                "reference": "LOGO1",
                "new_reference": "FID1",
            }),
            &context(server.address().to_string()),
        )
        .await
        .unwrap();
        assert!(result.is_error, "{}", text(&result));
        assert_eq!(
            crate::mcp::error::extract_error_kind(&result).as_deref(),
            Some("plan_blocked"),
            "{}",
            text(&result)
        );
        assert!(text(&result).contains("fp-2"), "{}", text(&result));
        assert!(text(&result).contains("FID1"), "{}", text(&result));
        let sent = sent.lock().unwrap();
        assert_eq!((sent.begins, sent.updates.len()), (0, 0));
    }

    #[tokio::test]
    async fn a_shared_reference_without_kiid_is_ambiguous_and_names_each_kiid() {
        for arguments in [json!({}), json!({ "new_reference": "LOGO1" })] {
            let temp = tempfile::tempdir().unwrap();
            let board = temp.path().join("board.kicad_pcb");
            std::fs::write(&board, "(kicad_pcb (version 20240108))").unwrap();
            let fp1 = c_0603_with("fp-logo", "REF**", false);
            let fp2 = c_0603_with("fp-fid", "REF**", false);
            let (server, sent) = spawn_many(&board, vec![fp1, fp2], Kicad::Keeps);
            let mut args = json!({ "board": board.to_string_lossy(), "reference": "REF**" });
            for (key, value) in arguments.as_object().unwrap() {
                args[key] = value.clone();
            }
            let result = handle_edit_component(&args, &context(server.address().to_string()))
                .await
                .unwrap();
            assert!(result.is_error, "{}", text(&result));
            let body: serde_json::Value = serde_json::from_str(&text(&result)).unwrap();
            assert_eq!(body["error"]["kind"], "ambiguous_target", "{body}");
            assert_eq!(body["error"]["target"], "REF**", "{body}");
            assert_eq!(
                body["error"]["candidates"],
                json!(["fp-fid", "fp-logo"]),
                "{body}"
            );
            assert!(text(&result).contains("pass kiid"), "{}", text(&result));
            let sent = sent.lock().unwrap();
            assert_eq!((sent.begins, sent.updates.len()), (0, 0));
        }
    }

    #[tokio::test]
    async fn kiid_disambiguates_duplicate_board_only_references_through_served_dispatch() {
        let temp = tempfile::tempdir().unwrap();
        let board = temp.path().join("board.kicad_pcb");
        std::fs::write(&board, "(kicad_pcb (version 20240108))").unwrap();
        let fp1 = c_0603_with("fp-logo", "REF**", false);
        let fp2 = c_0603_with("fp-fid", "REF**", false);
        let (server, sent) = spawn_many(&board, vec![fp1, fp2], Kicad::Keeps);
        let mut config = context(server.address().to_string()).config;
        config.eager_toolsets = true;
        let handler = crate::mcp::handler::McpHandler::new(config).await.unwrap();

        // Served dispatch: rename only `fp-logo` out of two `REF**` footprints.
        let reply = handler
            .handle_message(json!({
                "jsonrpc": "2.0",
                "id": 40,
                "method": "tools/call",
                "params": {
                    "name": "edit_component",
                    "arguments": {
                        "board": board,
                        "reference": "REF**",
                        "kiid": "fp-logo",
                        "new_reference": "LOGO1",
                    }
                }
            }))
            .await
            .unwrap()
            .result
            .unwrap();
        assert_ne!(reply["isError"], true, "{reply}");
        let body: serde_json::Value =
            serde_json::from_str(reply["content"][0]["text"].as_str().unwrap()).unwrap();
        assert_eq!(body["status"], "applied");
        assert_eq!(body["kiid"], "fp-logo");
        assert_eq!(body["reference"], "LOGO1");
        assert_eq!(body["before"]["reference"], "REF**");
        assert_eq!(body["changed"], json!(["reference"]));
        assert_eq!(sent.lock().unwrap().begins, 1);

        // The second footprint is still `REF**` and can be inspected by `kiid`.
        let inspect = handle_edit_component(
            &json!({
                "board": board.to_string_lossy(),
                "reference": "REF**",
                "kiid": "fp-fid",
            }),
            &context(server.address().to_string()),
        )
        .await
        .unwrap();
        assert!(!inspect.is_error, "{}", text(&inspect));
        let inspect_body: serde_json::Value = serde_json::from_str(&text(&inspect)).unwrap();
        assert_eq!(inspect_body["status"], "unchanged");
        assert_eq!(inspect_body["kiid"], "fp-fid");
        assert_eq!(inspect_body["reference"], "REF**");
    }

    #[tokio::test]
    async fn a_stale_kiid_or_mismatched_reference_is_refused_as_stale_target() {
        let before = c_0603_with("fp-1", "LOGO1", false);
        let original_kiid = "fp-1";
        for (args, expected_snippet) in [
            (
                json!({ "kiid": "fp-missing", "new_reference": "LOGO2" }),
                "fp-missing",
            ),
            (
                json!({ "reference": "WRONG", "kiid": original_kiid, "new_reference": "LOGO2" }),
                "currently has reference 'LOGO1'",
            ),
            (
                json!({ "reference": "WRONG", "kiid": original_kiid }),
                "currently has reference 'LOGO1'",
            ),
        ] {
            let (result, sent, _) = edit(before.clone(), Kicad::Keeps, args.clone()).await;
            assert!(result.is_error, "{args}: {}", text(&result));
            assert_eq!(
                crate::mcp::error::extract_error_kind(&result).as_deref(),
                Some("stale_target"),
                "{args}: {}",
                text(&result)
            );
            assert!(
                text(&result).contains(expected_snippet),
                "{args}: {}",
                text(&result)
            );
            let sent = sent.lock().unwrap();
            assert_eq!((sent.begins, sent.updates.len()), (0, 0));
        }
    }

    #[tokio::test]
    async fn a_reference_kicad_did_not_keep_is_an_uncertain_outcome() {
        let (result, sent, original_ref) = edit(
            c_0603(false),
            Kicad::IgnoresReference,
            json!({ "new_reference": "LOGO1" }),
        )
        .await;
        assert!(result.is_error, "{}", text(&result));
        assert_eq!(
            crate::mcp::error::extract_error_kind(&result).as_deref(),
            Some("mutation_outcome_uncertain"),
            "{}",
            text(&result)
        );
        let body: serde_json::Value = serde_json::from_str(&text(&result)).unwrap();
        let reason = body["error"]["reason"].as_str().unwrap();
        assert!(
            reason.contains(&format!(
                "reference: expected \"LOGO1\", observed \"{original_ref}\""
            )),
            "{body}"
        );
        assert_eq!(sent.lock().unwrap().begins, 1);
    }

    #[tokio::test]
    async fn a_rename_to_the_same_reference_is_a_noop_when_board_owned_and_blocked_when_linked() {
        let before = c_0603_with("fp-1", "LOGO1", false);
        let (result, sent, _) =
            edit(before, Kicad::Keeps, json!({ "new_reference": "LOGO1" })).await;
        assert!(!result.is_error, "{}", text(&result));
        let body: serde_json::Value = serde_json::from_str(&text(&result)).unwrap();
        assert_eq!(body["status"], "unchanged");
        assert_eq!(body["changed"], json!([]));
        assert_eq!(sent.lock().unwrap().begins, 0);

        let linked = c_0603_with("fp-1", "C1", true);
        let (result, sent, _) = edit(linked, Kicad::Keeps, json!({ "new_reference": "C1" })).await;
        assert!(result.is_error, "{}", text(&result));
        assert_eq!(
            crate::mcp::error::extract_error_kind(&result).as_deref(),
            Some("plan_blocked"),
            "{}",
            text(&result)
        );
        assert_eq!(sent.lock().unwrap().begins, 0);
    }

    #[tokio::test]
    async fn invalid_new_reference_and_kiid_arguments_are_refused_before_ipc() {
        let ctx = context("ipc:///nonexistent-copperpour-test-40.sock".to_string());
        for (args, field) in [
            (
                json!({ "board": "b.kicad_pcb", "reference": "R1", "new_reference": "" }),
                "new_reference",
            ),
            (
                json!({ "board": "b.kicad_pcb", "reference": "R1", "new_reference": "   " }),
                "new_reference",
            ),
            (
                json!({ "board": "b.kicad_pcb", "reference": "R1", "new_reference": "R 1" }),
                "new_reference",
            ),
            (
                json!({ "board": "b.kicad_pcb", "reference": "R1", "new_reference": "R1\n" }),
                "new_reference",
            ),
            (
                json!({ "board": "b.kicad_pcb", "reference": "R1", "new_reference": 42 }),
                "new_reference",
            ),
            (
                json!({ "board": "b.kicad_pcb", "reference": "R1", "kiid": "" }),
                "kiid",
            ),
            (
                json!({ "board": "b.kicad_pcb", "reference": "R1", "kiid": 42 }),
                "kiid",
            ),
        ] {
            let result = handle_edit_component(&args, &ctx).await.unwrap();
            assert!(result.is_error, "{args}: {}", text(&result));
            assert_eq!(
                crate::mcp::error::extract_error_kind(&result).as_deref(),
                Some("invalid_argument"),
                "{args}: {}",
                text(&result)
            );
            assert!(text(&result).contains(field), "{args}: {}", text(&result));
        }
    }

    use super::tests::{SHARED_FIRST_UUID, SHARED_SECOND_UUID};

    /// One of `board_only_shared_reference.kicad_pcb`'s two `REF**` footprints, as KiCad's IPC API sent
    /// it, with only its KIID varied (see the capture's README).
    fn ref_star(kiid: &str) -> kiapi::board::types::FootprintInstance {
        let mut footprint = kiapi::board::types::FootprintInstance::decode(
            include_bytes!("../../tests/fixtures/board_only_shared_reference.ipc.bin").as_slice(),
        )
        .unwrap();
        footprint.id = Some(kiapi::common::types::Kiid {
            value: kiid.to_string(),
        });
        footprint
    }

    async fn find(board: &Path, address: String, arguments: serde_json::Value) -> CallToolResult {
        let mut args = json!({ "board": board.to_string_lossy(), "reference": "REF**" });
        for (key, value) in arguments.as_object().unwrap() {
            args[key] = value.clone();
        }
        handle_find_component(&args, &context(address))
            .await
            .unwrap()
    }

    /// A reference two footprints share is refused as `ambiguous_target`
    /// naming both KIIDs, sorted; `kiid` finds each, and a KIID no footprint
    /// carries is `stale_target` (#880).
    #[tokio::test]
    async fn find_component_refuses_a_shared_reference_and_finds_each_by_kiid() {
        let temp = tempfile::tempdir().unwrap();
        let board = temp.path().join("board.kicad_pcb");
        std::fs::write(&board, "(kicad_pcb (version 20240108))").unwrap();
        let footprints = vec![ref_star(SHARED_SECOND_UUID), ref_star(SHARED_FIRST_UUID)];
        let (server, _) = spawn_many(&board, footprints, Kicad::Keeps);
        let address = server.address().to_string();

        let ambiguous = find(&board, address.clone(), json!({})).await;
        assert!(ambiguous.is_error, "{}", text(&ambiguous));
        let body: serde_json::Value = serde_json::from_str(&text(&ambiguous)).unwrap();
        assert_eq!(body["error"]["kind"], "ambiguous_target", "{body}");
        assert_eq!(body["error"]["target"], "REF**", "{body}");
        assert_eq!(
            body["error"]["candidates"],
            json!([SHARED_FIRST_UUID, SHARED_SECOND_UUID]),
            "{body}"
        );
        assert!(text(&ambiguous).contains("pass kiid"), "{body}");

        for kiid in [SHARED_FIRST_UUID, SHARED_SECOND_UUID] {
            let found = find(&board, address.clone(), json!({ "kiid": kiid })).await;
            assert!(!found.is_error, "{}", text(&found));
            let body: serde_json::Value = serde_json::from_str(&text(&found)).unwrap();
            assert_eq!(body["kiid"], kiid, "{body}");
            assert_eq!(body["reference"], "REF**", "{body}");
            assert_eq!(
                body["footprint"], "MountingHole:MountingHole_2.7mm_M2.5_DIN965",
                "{body}"
            );
        }

        let stale = find(&board, address, json!({ "kiid": "not-on-the-board" })).await;
        assert_eq!(
            crate::mcp::error::extract_error_kind(&stale).as_deref(),
            Some("stale_target"),
            "{}",
            text(&stale)
        );
    }

    /// A reference one footprint carries is found without `kiid`, and the
    /// answer now carries its KIID.
    #[tokio::test]
    async fn find_component_reports_the_kiid_of_a_unique_reference() {
        let temp = tempfile::tempdir().unwrap();
        let board = temp.path().join("board.kicad_pcb");
        std::fs::write(&board, "(kicad_pcb (version 20240108))").unwrap();
        let (server, _) = spawn_many(
            &board,
            vec![
                c_0603_with("fp-1", "C1", false),
                ref_star(SHARED_FIRST_UUID),
            ],
            Kicad::Keeps,
        );
        let result = handle_find_component(
            &json!({ "board": board.to_string_lossy(), "reference": "C1" }),
            &context(server.address().to_string()),
        )
        .await
        .unwrap();
        assert!(!result.is_error, "{}", text(&result));
        let body: serde_json::Value = serde_json::from_str(&text(&result)).unwrap();
        assert_eq!(body["reference"], "C1", "{body}");
        assert_eq!(body["kiid"], "fp-1", "{body}");
    }
}

#[cfg(test)]
mod structured_error_kind_tests {
    use super::*;
    use serde_json::json;

    async fn served_call(name: &str, arguments: serde_json::Value) -> (bool, serde_json::Value) {
        let handler = crate::mcp::handler::McpHandler::new(crate::tools::ServerConfig {
            ipc_address: "ipc:///nonexistent-copperpour-test-741.sock".to_string(),
            eager_toolsets: true,
            ..Default::default()
        })
        .await
        .unwrap();
        let reply = handler
            .handle_message(json!({
                "jsonrpc": "2.0",
                "id": 741,
                "method": "tools/call",
                "params": {
                    "name": name,
                    "arguments": arguments
                }
            }))
            .await
            .unwrap();
        let result = reply.result.expect("tools/call returned a JSON-RPC result");
        let is_error = result["isError"].as_bool().unwrap_or(false);
        let text = result["content"][0]["text"].as_str().unwrap_or_default();
        let parsed = serde_json::from_str::<serde_json::Value>(text)
            .unwrap_or_else(|_| panic!("{name} returned non-JSON body: {text}"));
        (is_error, parsed)
    }

    fn write_project_lib(dir: &Path, lib_name: &str, fp_name: &str, content: &str) {
        let pretty = dir.join(format!("{lib_name}.pretty"));
        std::fs::create_dir_all(&pretty).unwrap();
        std::fs::write(pretty.join(format!("{fp_name}.kicad_mod")), content).unwrap();
        std::fs::write(
            dir.join("fp-lib-table"),
            format!(
                "(fp_lib_table\n  (version 7)\n  (lib (name \"{lib_name}\")(type \"KiCad\")(uri \"${{KIPRJMOD}}/{lib_name}.pretty\")(options \"\")(descr \"\"))\n)\n"
            ),
        )
        .unwrap();
    }

    const MINIMAL_BOARD: &str = "(kicad_pcb (version 20240108) (generator \"pcbnew\")\n  (net 0 \"\")\n  (footprint \"Resistor_SMD:R_0603_1608Metric\"\n    (layer \"F.Cu\")\n    (at 10 20)\n    (property \"Reference\" \"R1\" (at 0 0) (layer \"F.SilkS\"))\n    (pad \"1\" smd rect (at -0.8 0) (size 0.8 0.9) (layers \"F.Cu\" \"F.Paste\" \"F.Mask\"))\n  )\n)\n";

    #[tokio::test]
    async fn place_component_refusals_report_structured_error_kinds() {
        let tmp = tempfile::tempdir().unwrap();
        let board = tmp.path().join("board.kicad_pcb");
        std::fs::write(&board, MINIMAL_BOARD).unwrap();

        // Bad Library:Footprint syntax -> invalid_argument
        let (is_error, body) = served_call(
            "place_component",
            json!({
                "board": board,
                "footprint": "NoColonSyntax",
                "reference": "R2",
                "x": 10.0,
                "y": 10.0
            }),
        )
        .await;
        assert!(is_error);
        assert_eq!(body["error"]["kind"], "invalid_argument");
        assert_eq!(body["error"]["field"], "footprint");

        // Unresolved library footprint -> file_not_found
        let (is_error, body) = served_call(
            "place_component",
            json!({
                "board": board,
                "footprint": "NoSuchLib741:NoSuchPart741",
                "reference": "R2",
                "x": 10.0,
                "y": 10.0
            }),
        )
        .await;
        assert!(is_error);
        assert_eq!(body["error"]["kind"], "file_not_found");
        assert_eq!(body["error"]["path"], "NoSuchLib741:NoSuchPart741");
        assert_eq!(body["error"]["absolute_path"], "");

        // Malformed library .kicad_mod -> malformed_file
        write_project_lib(tmp.path(), "Broken741", "BadFp", "(footprint \"BadFp\"");
        let (is_error, body) = served_call(
            "place_component",
            json!({
                "board": board,
                "footprint": "Broken741:BadFp",
                "reference": "R2",
                "x": 10.0,
                "y": 10.0
            }),
        )
        .await;
        assert!(is_error);
        assert_eq!(body["error"]["kind"], "malformed_file");

        // Closed-board B.Cu placement of footprint with unsupported 3D model -> plan_blocked
        write_project_lib(
            tmp.path(),
            "BackModel741",
            "WithModel",
            "(footprint \"WithModel\"\n  (layer \"F.Cu\")\n  (property \"Reference\" \"REF**\" (at 0 0) (layer \"F.SilkS\"))\n  (property \"Value\" \"WithModel\" (at 0 1) (layer \"F.Fab\"))\n  (pad \"1\" smd rect (at 0 0) (size 1 1) (layers \"F.Cu\"))\n  (model \"foo.step\" (offset (xyz 0 0 0)) (scale (xyz 1 1 1)) (rotate (xyz 0 0 0)))\n)\n",
        );
        let (is_error, body) = served_call(
            "place_component",
            json!({
                "board": board,
                "footprint": "BackModel741:WithModel",
                "reference": "R2",
                "x": 10.0,
                "y": 10.0,
                "layer": "B.Cu"
            }),
        )
        .await;
        assert!(is_error);
        assert_eq!(body["error"]["kind"], "plan_blocked");
        assert_eq!(body["error"]["operation"], "place_component");
        assert_eq!(std::fs::read_to_string(&board).unwrap(), MINIMAL_BOARD);
    }

    #[test]
    fn a_board_footprint_without_a_library_nickname_is_not_blamed_on_an_argument() {
        // KiCad 10.0.8 loads and resaves `(footprint "R_0603_1608Metric" …)`
        // unchanged, so duplicate_component can read such an id off the board.
        let tmp = tempfile::tempdir().unwrap();
        let board = tmp.path().join("board.kicad_pcb");
        let refusal = read_footprint_source_or_error("R_0603_1608Metric", &board, None)
            .expect_err("an id without a nickname resolves to no library");
        assert_eq!(
            crate::mcp::error::extract_error_kind(&refusal).as_deref(),
            Some("file_not_found")
        );
        let refusal =
            read_footprint_source_or_error("R_0603_1608Metric", &board, Some("footprint"))
                .expect_err("a caller-supplied id without a nickname is refused");
        assert_eq!(
            crate::mcp::error::extract_error_kind(&refusal).as_deref(),
            Some("invalid_argument")
        );
    }

    #[tokio::test]
    async fn closed_board_unusable_refusal_reports_malformed_file_across_callers() {
        let tmp = tempfile::tempdir().unwrap();
        let board = tmp.path().join("broken.kicad_pcb");
        let broken = "(not_a_kicad_pcb (version 1))\n";
        std::fs::write(&board, broken).unwrap();

        let (is_error, body) = served_call(
            "move_component",
            json!({
                "board": board,
                "reference": "R1",
                "x": 20.0,
                "y": 30.0
            }),
        )
        .await;
        assert!(is_error);
        assert_eq!(body["error"]["kind"], "malformed_file");
        assert_eq!(body["error"]["path"], board.display().to_string());
        assert_eq!(std::fs::read_to_string(&board).unwrap(), broken);
    }

    #[tokio::test]
    async fn repair_corrupted_footprints_without_live_kicad_reports_editor_unavailable() {
        let tmp = tempfile::tempdir().unwrap();
        let board = tmp.path().join("board.kicad_pcb");
        std::fs::write(&board, MINIMAL_BOARD).unwrap();

        let (is_error, body) =
            served_call("repair_corrupted_footprints", json!({ "board": board })).await;
        assert!(is_error);
        assert_eq!(body["error"]["kind"], "editor_unavailable");
        assert_eq!(body["error"]["editor"], "pcb");
    }

    #[tokio::test]
    async fn saved_pad_queries_report_structured_error_kinds() {
        let tmp = tempfile::tempdir().unwrap();
        let board = tmp.path().join("board.kicad_pcb");
        std::fs::write(&board, MINIMAL_BOARD).unwrap();

        // Missing footprint -> invalid_argument
        let (is_error, body) = served_call(
            "get_component_pads",
            json!({ "board": board, "reference": "U99" }),
        )
        .await;
        assert!(is_error);
        assert_eq!(body["error"]["kind"], "invalid_argument");
        assert_eq!(body["error"]["field"], "reference");

        // Missing pad -> invalid_argument
        let (is_error, body) = served_call(
            "get_pad_position",
            json!({ "board": board, "reference": "R1", "pad_number": "99" }),
        )
        .await;
        assert!(is_error);
        assert_eq!(body["error"]["kind"], "invalid_argument");
        assert_eq!(body["error"]["field"], "pad_number");

        // Malformed saved pads: missing (at), invalid (at), unreadable pad number
        for (label, pad_sexp) in [
            (
                "missing_at",
                "(pad \"1\" smd rect (size 0.8 0.9) (layers \"F.Cu\"))",
            ),
            (
                "invalid_at",
                "(pad \"1\" smd rect (at not_a_num) (size 0.8 0.9) (layers \"F.Cu\"))",
            ),
            (
                "unreadable_number",
                "(pad (nested) smd rect (at 0 0) (size 0.8 0.9) (layers \"F.Cu\"))",
            ),
        ] {
            let bad_board = tmp.path().join(format!("{label}.kicad_pcb"));
            std::fs::write(
                &bad_board,
                format!(
                    "(kicad_pcb (version 20240108) (generator \"pcbnew\")\n  (footprint \"Resistor_SMD:R_0603_1608Metric\"\n    (layer \"F.Cu\")\n    (at 10 20)\n    (property \"Reference\" \"R1\" (at 0 0) (layer \"F.SilkS\"))\n    {pad_sexp}\n  )\n)\n"
                ),
            )
            .unwrap();
            let (is_error, body) = served_call(
                "get_component_pads",
                json!({ "board": bad_board, "reference": "R1" }),
            )
            .await;
            assert!(is_error, "{label}");
            assert_eq!(body["error"]["kind"], "malformed_file", "{label}: {body}");
            assert_eq!(body["error"]["path"], bad_board.display().to_string());
        }
    }

    #[tokio::test]
    async fn place_array_and_align_components_report_structured_error_kinds() {
        let tmp = tempfile::tempdir().unwrap();
        let board = tmp.path().join("board.kicad_pcb");
        std::fs::write(&board, MINIMAL_BOARD).unwrap();

        // place_array: count overflow -> invalid_argument
        let (is_error, body) = served_call(
            "place_component_array",
            json!({
                "board": board,
                "footprint": "Lib:Fp",
                "start_x": 0.0,
                "start_y": 0.0,
                "count_x": u64::MAX,
                "count_y": 2,
                "spacing_x": 5.0
            }),
        )
        .await;
        assert!(is_error);
        assert_eq!(body["error"]["kind"], "invalid_argument");
        assert_eq!(body["error"]["field"], "count_x");

        // place_array: zero count -> invalid_argument
        let (is_error, body) = served_call(
            "place_component_array",
            json!({
                "board": board,
                "footprint": "Lib:Fp",
                "start_x": 0.0,
                "start_y": 0.0,
                "count_x": 2,
                "count_y": 0,
                "spacing_x": 5.0
            }),
        )
        .await;
        assert!(is_error);
        assert_eq!(body["error"]["kind"], "invalid_argument");
        assert_eq!(body["error"]["field"], "count_y");

        // place_array: ref_start overflow -> invalid_argument
        let (is_error, body) = served_call(
            "place_component_array",
            json!({
                "board": board,
                "footprint": "Lib:Fp",
                "start_x": 0.0,
                "start_y": 0.0,
                "count_x": 2,
                "count_y": 1,
                "spacing_x": 5.0,
                "ref_start": u64::MAX
            }),
        )
        .await;
        assert!(is_error);
        assert_eq!(body["error"]["kind"], "invalid_argument");
        assert_eq!(body["error"]["field"], "ref_start");

        // place_array: valid footprint, unreachable IPC -> editor_unavailable via ipc_failure_result
        write_project_lib(
            tmp.path(),
            "ArrayLib741",
            "R",
            "(footprint \"R\"\n  (layer \"F.Cu\")\n  (property \"Reference\" \"REF**\" (at 0 0) (layer \"F.SilkS\"))\n  (property \"Value\" \"R\" (at 0 1) (layer \"F.Fab\"))\n  (pad \"1\" smd rect (at 0 0) (size 1 1) (layers \"F.Cu\"))\n)\n",
        );
        let (is_error, body) = served_call(
            "place_component_array",
            json!({
                "board": board,
                "footprint": "ArrayLib741:R",
                "start_x": 0.0,
                "start_y": 0.0,
                "count_x": 2,
                "count_y": 1,
                "spacing_x": 5.0
            }),
        )
        .await;
        assert!(is_error);
        assert_eq!(body["error"]["kind"], "editor_unavailable");

        // align_components: empty references -> invalid_argument
        let (is_error, body) = served_call(
            "align_components",
            json!({ "board": board, "references": [], "axis": "x", "value": 10.0 }),
        )
        .await;
        assert!(is_error);
        assert_eq!(body["error"]["kind"], "invalid_argument");
        assert_eq!(body["error"]["field"], "references");

        // align_components: non-string reference -> invalid_argument (schema validates element)
        let (is_error, body) = served_call(
            "align_components",
            json!({ "board": board, "references": ["R1", 42], "axis": "x", "value": 10.0 }),
        )
        .await;
        assert!(is_error);
        assert_eq!(body["error"]["kind"], "invalid_argument");
        assert_eq!(body["error"]["field"], "references[1]");

        // Direct handler check for the in-handler non-string reference guard
        let direct = handle_align_components(
            &json!({ "board": board, "references": ["R1", 42], "axis": "x", "value": 10.0 }),
            &crate::tools::ToolContext::new(
                Default::default(),
                std::sync::Arc::new(crate::tools::ToolRouter::new()),
            ),
        )
        .await
        .unwrap();
        assert!(direct.is_error);
        assert_eq!(
            crate::mcp::error::extract_error_kind(&direct).as_deref(),
            Some("invalid_argument")
        );

        // align_components: invalid axis -> invalid_argument
        let (is_error, body) = served_call(
            "align_components",
            json!({ "board": board, "references": ["R1"], "axis": "z", "value": 10.0 }),
        )
        .await;
        assert!(is_error);
        assert_eq!(body["error"]["kind"], "invalid_argument");
        assert_eq!(body["error"]["field"], "axis");

        // align_components: unreachable IPC -> editor_unavailable via ipc_failure_result
        let (is_error, body) = served_call(
            "align_components",
            json!({ "board": board, "references": ["R1"], "axis": "x", "value": 10.0 }),
        )
        .await;
        assert!(is_error);
        assert_eq!(body["error"]["kind"], "editor_unavailable");
    }
}
