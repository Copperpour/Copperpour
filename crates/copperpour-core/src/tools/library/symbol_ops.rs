//! Symbol creation and deletion tools (`create_symbol`, `delete_symbol`).

use super::footprint_ops::invalid_library_argument;
use super::search_and_info::top_level_symbol_names;
use crate::mcp::protocol::CallToolResult;
use crate::tools::footprint_metadata::quote_string;
use crate::tools::{get_path, require_str, ToolContext};
use copperpour_schematic_editor::types::fmt_f64;
use copperpour_sexp::geometry::round6;
#[cfg(test)]
use copperpour_sexp::parser::{parse_sexp, SexpNode};
use copperpour_sexp::writer::{
    find_direct_child_blocks, read_consistent, write_atomic_if_unchanged,
};
use serde_json::json;

// ─── Symbol library tools ─────────────────────────────────────────────────────

/// Minimal pin geometry for deriving the symbol body.
#[derive(Debug, Clone)]
pub(super) struct PinGeom {
    pub(super) x: f64,
    pub(super) y: f64,
    pub(super) angle: f64,
    pub(super) length: f64,
    pub(super) name: String,
}

/// The point where a pin meets the symbol body. In KiCAD symbols the pin's
/// connection endpoint (the "bulb", where wires attach) is at `(x, y)` and the
/// pin extends by `length` in its orientation to reach the body outline. Angles
/// are 0=E, 90=N, 180=W, 270=S with Y up, so the body-attach point (root) is
/// `(x + length*cos, y + length*sin)` — on the far side of the bulb.
pub(super) fn pin_root(x: f64, y: f64, angle_deg: f64, length: f64) -> (f64, f64) {
    let a = angle_deg.to_radians();
    (x + length * a.cos(), y + length * a.sin())
}

/// The body edge a pin attaches to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PinEdge {
    Left,
    Right,
    Bottom,
    Top,
}

/// Which edge a pin sits on, from its orientation (Y up): a pin pointing right
/// (0) sits on the left edge, left (180) on the right edge, up (90) on the
/// bottom edge, down (270) on the top edge. A degree of tolerance absorbs
/// callers whose 360 arithmetic lands just off. `None` for anything diagonal.
fn pin_edge(angle_deg: f64) -> Option<PinEdge> {
    let a = angle_deg.rem_euclid(360.0);
    let near = |t: f64| {
        let d = (a - t).abs();
        !(1.0..=359.0).contains(&d)
    };
    if near(0.0) {
        Some(PinEdge::Left)
    } else if near(180.0) {
        Some(PinEdge::Right)
    } else if near(90.0) {
        Some(PinEdge::Bottom)
    } else if near(270.0) {
        Some(PinEdge::Top)
    } else {
        None
    }
}

/// Body rectangle `(min_x, min_y, max_x, max_y)` for a symbol: edges that pins
/// attach to pass through those pins' roots (so each pin's far end touches the
/// border and its connection bulb sits outside), and edges with no pins are
/// pushed out by a margin so there is clear spacing beyond the outermost pins.
/// `None` when there are no pins.
pub(super) fn symbol_body_rect(pins: &[PinGeom], show_names: bool) -> Option<(f64, f64, f64, f64)> {
    if pins.is_empty() {
        return None;
    }
    let roots: Vec<(f64, f64)> = pins
        .iter()
        .map(|p| pin_root(p.x, p.y, p.angle, p.length))
        .collect();
    let mut min_x = f64::INFINITY;
    let mut min_y = f64::INFINITY;
    let mut max_x = f64::NEG_INFINITY;
    let mut max_y = f64::NEG_INFINITY;
    for &(x, y) in &roots {
        min_x = min_x.min(x);
        min_y = min_y.min(y);
        max_x = max_x.max(x);
        max_y = max_y.max(y);
    }

    // Which edges have pins attaching.
    let (mut has_left, mut has_right, mut has_bottom, mut has_top) = (false, false, false, false);
    for p in pins {
        match pin_edge(p.angle) {
            Some(PinEdge::Left) => has_left = true,
            Some(PinEdge::Right) => has_right = true,
            Some(PinEdge::Bottom) => has_bottom = true,
            Some(PinEdge::Top) => has_top = true,
            None => {}
        }
    }

    // Spacing beyond the last pin on any edge without attachments (~1 grid).
    let margin = 2.54;
    if !has_left {
        min_x -= margin;
    }
    if !has_right {
        max_x += margin;
    }
    if !has_bottom {
        min_y -= margin;
    }
    if !has_top {
        max_y += margin;
    }

    // Minimum visible body.
    let min_size = 2.54;
    if max_x - min_x < min_size {
        let c = (min_x + max_x) / 2.0;
        min_x = c - min_size / 2.0;
        max_x = c + min_size / 2.0;
    }
    if max_y - min_y < min_size {
        let c = (min_y + max_y) / 2.0;
        min_y = c - min_size / 2.0;
        max_y = c + min_size / 2.0;
    }

    // A box that only touches the pin roots is too small for the names KiCad
    // draws inside it: with long names the two columns run into each other.
    if show_names {
        grow_to(&mut min_x, &mut max_x, names_span(pins, Axis::Horizontal));
        grow_to(&mut min_y, &mut max_y, names_span(pins, Axis::Vertical));
    }
    Some((min_x, min_y, max_x, max_y))
}

/// The pair of facing edges a name span is measured across.
#[derive(Debug, Clone, Copy)]
pub(super) enum Axis {
    Horizontal,
    Vertical,
}

/// Outer size needed so the pin names on two facing edges never meet.
///
/// Pins level with each other share a row, which needs both names plus a gap.
/// Horizontally that pairs the left and right edges with rows keyed by Y;
/// vertically the bottom and top edges with rows keyed by X.
pub(super) fn names_span(pins: &[PinGeom], axis: Axis) -> f64 {
    let (a, b) = match axis {
        Axis::Horizontal => (PinEdge::Left, PinEdge::Right),
        Axis::Vertical => (PinEdge::Bottom, PinEdge::Top),
    };
    let mut rows: std::collections::HashMap<i64, (f64, f64)> = std::collections::HashMap::new();
    for p in pins {
        let Some(edge) = pin_edge(p.angle).filter(|e| *e == a || *e == b) else {
            continue;
        };
        let w = pin_name_width(&p.name, PIN_TEXT);
        if w == 0.0 {
            continue;
        }
        let across = match axis {
            Axis::Horizontal => p.y,
            Axis::Vertical => p.x,
        };
        let row = rows
            .entry((across * 1000.0).round() as i64)
            .or_insert((0.0, 0.0));
        let col = if edge == a { &mut row.0 } else { &mut row.1 };
        *col = col.max(w);
    }
    let widest = rows
        .values()
        .map(|&(a, b)| {
            if a > 0.0 && b > 0.0 {
                a + b + PIN_NAME_GAP
            } else {
                a + b
            }
        })
        .fold(0.0, f64::max);
    if widest == 0.0 {
        0.0
    } else {
        widest + 2.0 * PIN_NAME_OFFSET
    }
}

/// Widen `lo..hi` symmetrically to at least `needed`, keeping the new edges on
/// the schematic grid so pins pushed out to meet them stay on it too.
pub(super) fn grow_to(lo: &mut f64, hi: &mut f64, needed: f64) {
    if needed <= *hi - *lo {
        return;
    }
    let centre = (*lo + *hi) / 2.0;
    let half = needed / 2.0;
    // Snap each edge outwards on its own: rounding the half-extent instead only
    // lands on the grid when the centre already does. `+ 0.0` turns the `-0.0`
    // floor/ceil produce at the origin back into `0`.
    *lo = ((centre - half) / SYMBOL_GRID).floor() * SYMBOL_GRID + 0.0;
    *hi = ((centre + half) / SYMBOL_GRID).ceil() * SYMBOL_GRID + 0.0;
}

/// KiCAD's 12 valid pin electrical types — the first token of a
/// `(pin TYPE line …)` S-expression. Anything else makes eeschema refuse to
/// load the library ("Failed to load schematic"-class parse error), so the
/// value is validated instead of interpolated verbatim (#55).
pub(crate) const ALLOWED_PIN_ELECTRICAL_TYPES: [&str; 12] = [
    "input",
    "output",
    "bidirectional",
    "tri_state",
    "passive",
    "free",
    "unspecified",
    "power_in",
    "power_out",
    "open_collector",
    "open_emitter",
    "no_connect",
];

/// Error when any pin's electrical type is not one of KiCAD's 12 valid values
/// (#55) — eeschema refuses to load a library with a bad type, so nothing must
/// be written in that case. Shared by the rectangle and glyph paths.
fn validate_pin_types(pins_val: &[serde_json::Value]) -> anyhow::Result<()> {
    for pin in pins_val {
        let pin_type = pin["type"].as_str().unwrap_or("passive");
        if !ALLOWED_PIN_ELECTRICAL_TYPES.contains(&pin_type) {
            let number = pin["number"].as_str().unwrap_or("1");
            // The one mistake seen in the wild (#55) gets a targeted hint.
            let hint = if pin_type == "not_connected" {
                " (did you mean \"no_connect\"?)"
            } else {
                ""
            };
            anyhow::bail!(
                "invalid pin electrical type \"{}\" on pin \"{}\"{} — KiCAD accepts exactly one of: {}",
                pin_type,
                number,
                hint,
                ALLOWED_PIN_ELECTRICAL_TYPES.join(", ")
            );
        }
    }
    Ok(())
}

struct SymbolItems {
    pins: Vec<serde_json::Value>,
    units: Vec<serde_json::Value>,
    power_pins: Vec<serde_json::Value>,
}

/// Read and validate every array that contributes pins to a generated symbol.
/// This runs before library content is read or written, making a malformed
/// nested item an atomic, indexed `invalid_argument` rejection (#234).
fn parse_symbol_items(args: &serde_json::Value) -> Result<SymbolItems, CallToolResult> {
    fn optional_array(
        args: &serde_json::Value,
        field: &str,
    ) -> Result<Vec<serde_json::Value>, CallToolResult> {
        match args.get(field) {
            None => Ok(Vec::new()),
            Some(value) => value
                .as_array()
                .cloned()
                .ok_or_else(|| invalid_library_argument(field, "must be an array when provided")),
        }
    }

    fn validate_pins(
        pins: &[serde_json::Value],
        path: &str,
        require_xy: bool,
    ) -> Result<(), CallToolResult> {
        for (index, pin) in pins.iter().enumerate() {
            let pin_path = format!("{path}[{index}]");
            if !pin.is_object() {
                return Err(invalid_library_argument(&pin_path, "must be an object"));
            }
            for field in ["number", "name", "type"] {
                if pin[field].as_str().is_none() {
                    return Err(invalid_library_argument(
                        &format!("{pin_path}.{field}"),
                        "missing or not a string",
                    ));
                }
            }
            if require_xy {
                for field in ["x", "y"] {
                    if pin[field].as_f64().is_none() {
                        return Err(invalid_library_argument(
                            &format!("{pin_path}.{field}"),
                            "missing or not a number",
                        ));
                    }
                }
            }
        }
        Ok(())
    }

    let pins = optional_array(args, "pins")?;
    validate_pins(&pins, "pins", false)?;

    let units = optional_array(args, "units")?;
    for (index, unit) in units.iter().enumerate() {
        let unit_path = format!("units[{index}]");
        if !unit.is_object() {
            return Err(invalid_library_argument(&unit_path, "must be an object"));
        }
        let unit_pins = unit["pins"].as_array().ok_or_else(|| {
            invalid_library_argument(&format!("{unit_path}.pins"), "missing or not an array")
        })?;
        validate_pins(unit_pins, &format!("{unit_path}.pins"), false)?;
    }

    let power_pins = optional_array(args, "power_pins")?;
    validate_pins(&power_pins, "power_pins", true)?;

    Ok(SymbolItems {
        pins,
        units,
        power_pins,
    })
}

/// A conventional body shape for a symbol unit. `Rectangle` is the default (a
/// derived box around caller-positioned pins); the others draw a fixed op-amp or
/// logic-gate glyph copied from KiCAD's stock libraries and auto-place the pins.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Glyph {
    Rectangle,
    Opamp,
    Buffer,
    Inverter,
    Schmitt,
    SchmittInverter,
    And,
    Nand,
    Or,
    Nor,
    Xor,
    Xnor,
}

impl Glyph {
    fn parse(s: &str) -> Option<Glyph> {
        Some(match s {
            "rectangle" => Glyph::Rectangle,
            "opamp" => Glyph::Opamp,
            "buffer" => Glyph::Buffer,
            "inverter" => Glyph::Inverter,
            "schmitt" => Glyph::Schmitt,
            "schmitt_inverter" => Glyph::SchmittInverter,
            "and" => Glyph::And,
            "nand" => Glyph::Nand,
            "or" => Glyph::Or,
            "nor" => Glyph::Nor,
            "xor" => Glyph::Xor,
            "xnor" => Glyph::Xnor,
            _ => return None,
        })
    }

    fn name(self) -> &'static str {
        match self {
            Glyph::Rectangle => "rectangle",
            Glyph::Opamp => "opamp",
            Glyph::Buffer => "buffer",
            Glyph::Inverter => "inverter",
            Glyph::Schmitt => "schmitt",
            Glyph::SchmittInverter => "schmitt_inverter",
            Glyph::And => "and",
            Glyph::Nand => "nand",
            Glyph::Or => "or",
            Glyph::Nor => "nor",
            Glyph::Xor => "xor",
            Glyph::Xnor => "xnor",
        }
    }

    /// Inverting glyphs draw the same body as their non-inverting base and mark
    /// the inversion with an `inverted` output pin (matching KiCAD's own gates,
    /// which carry the bubble on the pin rather than as a body circle).
    fn is_inverting(self) -> bool {
        matches!(
            self,
            Glyph::Inverter | Glyph::SchmittInverter | Glyph::Nand | Glyph::Nor | Glyph::Xnor
        )
    }

    /// How many input pins the fixed geometry has room for.
    fn input_count(self) -> usize {
        match self {
            Glyph::Buffer | Glyph::Inverter | Glyph::Schmitt | Glyph::SchmittInverter => 1,
            _ => 2,
        }
    }

    /// The narrow triangle-bodied glyphs (op-amp and the buffer family). Their
    /// apex leaves no room for power-pin names, so a single-unit triangular
    /// symbol that carries power pins puts them on a separate rectangular power
    /// unit instead (the gate glyphs have a flat back with room, so they keep
    /// their power pins on the body).
    fn is_triangular(self) -> bool {
        matches!(
            self,
            Glyph::Opamp
                | Glyph::Buffer
                | Glyph::Inverter
                | Glyph::Schmitt
                | Glyph::SchmittInverter
        )
    }
}

/// Whether a pin is a supply pin (belongs on a power unit).
fn is_power_pin(p: &serde_json::Value) -> bool {
    matches!(p["type"].as_str(), Some("power_in") | Some("power_out"))
}

/// Lay out power pins for a standalone rectangular power unit: vertical, V+/V-
/// style — even-indexed pins enter from the top (pointing down), odd from the
/// bottom (pointing up), matching KiCAD's multi-unit op-amp power unit. Any
/// caller x/y is replaced. Same spread (bulbs at y = ±7.62) as the multi-unit
/// `power_pins` path so a single op-amp's power unit matches a dual's.
pub(super) fn layout_power_unit(power: &[serde_json::Value]) -> Vec<serde_json::Value> {
    // A repeated number is a legal stacked representation of one physical pin
    // and therefore owns one layout slot. Preserve the first-seen number order
    // while distributing distinct physical pins between the two sides.
    let mut numbers = Vec::<&str>::new();
    for pin in power {
        let number = pin["number"].as_str().unwrap_or("1");
        if !numbers.contains(&number) {
            numbers.push(number);
        }
    }
    let n_top = numbers.len().div_ceil(2);
    let n_bot = numbers.len() / 2;
    power
        .iter()
        .map(|p| {
            let mut q = p.clone();
            if let Some(obj) = q.as_object_mut() {
                let number = p["number"].as_str().unwrap_or("1");
                let slot = numbers
                    .iter()
                    .position(|candidate| *candidate == number)
                    .expect("the number was collected above");
                if slot % 2 == 0 {
                    let top_i = slot / 2;
                    let x = (top_i as f64 - (n_top as f64 - 1.0) / 2.0) * 2.54;
                    obj.insert("x".into(), json!(x));
                    obj.insert("y".into(), json!(7.62));
                    obj.insert("angle".into(), json!(270));
                    obj.insert("length".into(), json!(2.54));
                } else {
                    let bot_i = slot / 2;
                    let x = (bot_i as f64 - (n_bot as f64 - 1.0) / 2.0) * 2.54;
                    obj.insert("x".into(), json!(x));
                    obj.insert("y".into(), json!(-7.62));
                    obj.insert("angle".into(), json!(90));
                    obj.insert("length".into(), json!(2.54));
                }
            }
            q
        })
        .collect()
}

/// With no angles supplied, the dedicated power unit owns the conventional
/// top/bottom arrangement. Any explicit angle makes the caller's arrangement
/// authoritative; a collision in that arrangement is refused after layout.
fn power_pins_need_layout(power: &[serde_json::Value]) -> bool {
    !power.is_empty() && power.iter().all(|pin| pin["angle"].is_null())
}

/// Normalize a caller-supplied pin graphic style to a valid KiCAD token,
/// defaulting to `line`.
fn pin_style_token(s: Option<&str>) -> &'static str {
    match s {
        Some("inverted") => "inverted",
        Some("clock") => "clock",
        Some("inverted_clock") => "inverted_clock",
        Some("input_low") => "input_low",
        Some("clock_low") => "clock_low",
        Some("output_low") => "output_low",
        Some("edge_clock_high") => "edge_clock_high",
        Some("non_logic") => "non_logic",
        _ => "line",
    }
}

/// Default pin name/number text height (KiCAD's default).
pub(super) const PIN_TEXT: f64 = 1.27;
/// Smaller pin-name height for glyph units. The fixed op-amp/gate bodies are
/// compact (KiCAD's own gates keep pin names empty on them), so real pin names
/// at the default 1.27 mm collide; 0.762 mm (KiCAD's standard small text) fits
/// them without enlarging the body and breaking the library-matching shape.
const GLYPH_PIN_NAME_TEXT: f64 = 0.762;

/// Average advance per character of KiCad's stroke font, as a multiple of the
/// text height. Measured off a 400 dpi render of KiCad 10 output: an
/// 18-character name at 1.27 mm spans 24.03 mm. The font is proportional, so
/// this is an average — [`PIN_NAME_GAP`] absorbs the variance.
const STROKE_ADVANCE_RATIO: f64 = 1.05;
/// Clear space kept between the two columns of pin names inside a body.
pub(super) const PIN_NAME_GAP: f64 = 2.54;
/// Gap KiCad leaves between the body outline and the start of a pin name,
/// emitted as `(pin_names (offset …))`.
pub(super) const PIN_NAME_OFFSET: f64 = 1.016;
/// Schematic grid. Body edges land on it so pins pushed out to meet them do too.
pub(super) const SYMBOL_GRID: f64 = 1.27;

/// Whether `coord_mm` lies on KiCad's 1.27 mm (50 mil) schematic grid, within
/// [`POSITION_EPSILON`] (1e-6 mm, absorbing float noise while catching even a
/// single 0.0001 mm KiCad schematic internal unit of drift).
pub(super) fn is_on_symbol_grid(coord_mm: f64) -> bool {
    if !coord_mm.is_finite() {
        return false;
    }
    let snapped = (coord_mm / SYMBOL_GRID).round() * SYMBOL_GRID;
    (coord_mm - snapped).abs() <= POSITION_EPSILON
}

/// Warning when a single pin's written connection point `(x, y)` is off KiCad's
/// 1.27 mm schematic grid, or `None` when both axes align to the grid.
pub(crate) fn off_grid_pin_coordinate_warning(pin_number: &str, x: f64, y: f64) -> Option<String> {
    if is_on_symbol_grid(x) && is_on_symbol_grid(y) {
        return None;
    }
    Some(format!(
        "pin {pin_number} ({}, {}) is off the 1.27 mm schematic grid; grid-snapped wires and power symbols may not connect — use multiples of 1.27 mm",
        fmt_f64(x),
        fmt_f64(y),
    ))
}

/// Width of a pin name as KiCad draws it. `~{FOO}` is an overbar rather than
/// four extra glyphs, and a bare `~` means "unnamed" and draws nothing.
pub(super) fn pin_name_width(name: &str, size: f64) -> f64 {
    if name == "~" {
        return 0.0;
    }
    let glyphs = name.chars().filter(|c| !"~{}".contains(*c)).count();
    glyphs as f64 * size * STROKE_ADVANCE_RATIO
}

/// One `(pin …)` S-expression. `name_font` sets the pin-name text height;
/// numbers stay at the default (they sit outside the body and don't crowd).
///
/// Coordinates go through [`fmt_f64`] so binary floating-point artifacts stay
/// out of the file: `25.4 + 5.08` is `30.479999999999997` and `{}` prints it in
/// full.
#[allow(clippy::too_many_arguments)]
fn emit_pin(
    pin_type: &str,
    style: &str,
    x: f64,
    y: f64,
    angle: f64,
    length: f64,
    name: &str,
    number: &str,
    name_font: f64,
    hide: bool,
) -> String {
    format!(
        "\n    (pin {} {} (at {} {} {})\n      (length {}){}\n      (name \"{}\" (effects (font (size {} {}))))\n      (number \"{}\" (effects (font (size {} {}))))\n    )",
        pin_type,
        style,
        fmt_f64(x),
        fmt_f64(y),
        fmt_f64(angle),
        fmt_f64(length),
        if hide { "\n      (hide yes)" } else { "" },
        name,
        name_font,
        name_font,
        number,
        PIN_TEXT,
        PIN_TEXT
    )
}

/// A pin's `hide` argument; absent means drawn.
fn pin_is_hidden(pin: &serde_json::Value) -> bool {
    pin["hide"].as_bool().unwrap_or(false)
}

// ── Glyph geometry (coordinates copied verbatim from KiCAD 10's stock symbols)

fn fmt_pts(pts: &[(f64, f64)]) -> String {
    pts.iter()
        .map(|(x, y)| format!("(xy {} {})", x, y))
        .collect::<Vec<_>>()
        .join(" ")
}

fn g_polyline(pts: &[(f64, f64)], width: f64, fill: &str) -> String {
    format!(
        "\n      (polyline (pts {}) (stroke (width {}) (type default)) (fill (type {})))",
        fmt_pts(pts),
        width,
        fill
    )
}

fn g_arc(s: (f64, f64), m: (f64, f64), e: (f64, f64), fill: &str) -> String {
    format!(
        "\n      (arc (start {} {}) (mid {} {}) (end {} {}) (stroke (width 0.254) (type default)) (fill (type {})))",
        s.0, s.1, m.0, m.1, e.0, e.1, fill
    )
}

/// The OR/NOR body (also the base of XOR/XNOR): a concave back arc, two back
/// stubs, two front arcs meeting at the apex, and KiCAD's fill-outline polyline.
fn or_body() -> String {
    let mut b = g_arc((-3.81, 3.81), (-2.589, 0.0), (-3.81, -3.81), "none");
    b.push_str(&g_polyline(
        &[(-3.81, 3.81), (-0.635, 3.81)],
        0.254,
        "background",
    ));
    b.push_str(&g_polyline(
        &[(-3.81, -3.81), (-0.635, -3.81)],
        0.254,
        "background",
    ));
    b.push_str(&g_arc(
        (3.81, 0.0),
        (2.1855, -2.584),
        (-0.6096, -3.81),
        "background",
    ));
    b.push_str(&g_arc(
        (-0.6096, 3.81),
        (2.1928, 2.5924),
        (3.81, 0.0),
        "background",
    ));
    b.push_str(&g_polyline(
        &[
            (-0.635, 3.81),
            (-3.81, 3.81),
            (-3.81, 3.81),
            (-3.556, 3.4036),
            (-3.0226, 2.2606),
            (-2.6924, 1.0414),
            (-2.6162, -0.254),
            (-2.7686, -1.4986),
            (-3.175, -2.7178),
            (-3.81, -3.81),
            (-3.81, -3.81),
            (-0.635, -3.81),
        ],
        -25.4,
        "background",
    ));
    b
}

/// Fixed body + pin anchors for a glyph. Input anchors are ordered
/// top-to-bottom; the caller's pin order maps onto them in that order.
/// `power_top`/`power_bottom` are the points *on the body outline* where a
/// power pin's root should land (so the pin visually touches the shape, not the
/// bounding box).
struct GlyphGeom {
    body: String,
    inputs: Vec<(f64, f64, f64, f64)>,
    output: (f64, f64, f64, f64),
    power_top: (f64, f64),
    power_bottom: (f64, f64),
    rect: (f64, f64, f64, f64),
}

fn glyph_geom(g: Glyph) -> GlyphGeom {
    match g {
        Glyph::Rectangle => unreachable!("rectangle is handled by the rectangle path"),
        Glyph::Opamp => GlyphGeom {
            body: g_polyline(
                &[(-5.08, 5.08), (5.08, 0.0), (-5.08, -5.08), (-5.08, 5.08)],
                0.254,
                "background",
            ),
            inputs: vec![(-7.62, 2.54, 0.0, 2.54), (-7.62, -2.54, 0.0, 2.54)],
            output: (7.62, 0.0, 180.0, 2.54),
            // Centered on the triangle top/bottom edges (at x = 0, y = ±2.54),
            // so the power names clear the +/- input names on the left.
            power_top: (0.0, 2.54),
            power_bottom: (0.0, -2.54),
            rect: (-5.08, -5.08, 5.08, 5.08),
        },
        Glyph::Buffer | Glyph::Inverter => GlyphGeom {
            body: g_polyline(
                &[(-3.81, 3.81), (-3.81, -3.81), (3.81, 0.0), (-3.81, 3.81)],
                0.254,
                "background",
            ),
            inputs: vec![(-7.62, 0.0, 0.0, 3.81)],
            output: (7.62, 0.0, 180.0, 3.81),
            // Centered on the triangle top/bottom edges (x = 0, y = ±1.905).
            power_top: (0.0, 1.905),
            power_bottom: (0.0, -1.905),
            rect: (-3.81, -3.81, 3.81, 3.81),
        },
        Glyph::Schmitt | Glyph::SchmittInverter => {
            let mut body = g_polyline(
                &[(-3.81, 3.81), (-3.81, -3.81), (3.81, 0.0), (-3.81, 3.81)],
                0.254,
                "background",
            );
            // Hysteresis mark (from KiCAD's 74HC14).
            body.push_str(&g_polyline(
                &[(-2.54, -1.27), (-0.635, -1.27), (-0.635, 1.27), (0.0, 1.27)],
                0.254,
                "none",
            ));
            body.push_str(&g_polyline(
                &[(-1.905, -1.27), (-1.905, 1.27), (-0.635, 1.27)],
                0.254,
                "none",
            ));
            GlyphGeom {
                body,
                inputs: vec![(-7.62, 0.0, 0.0, 3.81)],
                output: (7.62, 0.0, 180.0, 3.81),
                // Centered (x = 0, y = ±1.905); the hysteresis mark sits at x <= 0.
                power_top: (0.0, 1.905),
                power_bottom: (0.0, -1.905),
                rect: (-3.81, -3.81, 3.81, 3.81),
            }
        }
        Glyph::And | Glyph::Nand => {
            let mut body = g_arc((0.0, 3.81), (3.7934, 0.0), (0.0, -3.81), "background");
            body.push_str(&g_polyline(
                &[(0.0, 3.81), (-3.81, 3.81), (-3.81, -3.81), (0.0, -3.81)],
                0.254,
                "background",
            ));
            GlyphGeom {
                body,
                inputs: vec![(-7.62, 2.54, 0.0, 3.81), (-7.62, -2.54, 0.0, 3.81)],
                output: (7.62, 0.0, 180.0, 3.81),
                // Right end of the flat back edges (x = 0, y = ±3.81), away from
                // the input names on the left.
                power_top: (0.0, 3.81),
                power_bottom: (0.0, -3.81),
                rect: (-3.81, -3.81, 3.81, 3.81),
            }
        }
        Glyph::Or | Glyph::Nor => GlyphGeom {
            body: or_body(),
            // Longer than the gates above: the back is a *concave* arc that sits
            // at x ≈ -3.10 at the input height, so length 4.52 puts the roots on
            // the curve (3.81 would leave a visible gap).
            inputs: vec![(-7.62, 2.54, 0.0, 4.52), (-7.62, -2.54, 0.0, 4.52)],
            output: (7.62, 0.0, 180.0, 3.81),
            // Rightmost point of the flat back stubs (y = ±3.81, x = -0.635),
            // away from the input names on the left.
            power_top: (-0.635, 3.81),
            power_bottom: (-0.635, -3.81),
            rect: (-3.81, -3.81, 3.81, 3.81),
        },
        Glyph::Xor | Glyph::Xnor => {
            // OR body plus a second offset back arc and two input stubs.
            let mut body = g_arc((-4.4196, 3.81), (-3.2033, 0.0), (-4.4196, -3.81), "none");
            body.push_str(&or_body());
            body.push_str(&g_polyline(
                &[(-3.81, 2.54), (-3.175, 2.54)],
                0.254,
                "background",
            ));
            body.push_str(&g_polyline(
                &[(-3.81, -2.54), (-3.175, -2.54)],
                0.254,
                "background",
            ));
            GlyphGeom {
                body,
                inputs: vec![(-7.62, 2.54, 0.0, 4.445), (-7.62, -2.54, 0.0, 4.445)],
                output: (7.62, 0.0, 180.0, 3.81),
                // Rightmost flat point of the back stubs (x = -0.635, y = ±3.81).
                power_top: (-0.635, 3.81),
                power_bottom: (-0.635, -3.81),
                rect: (-4.4196, -3.81, 3.81, 3.81),
            }
        }
    }
}

/// Build a glyph unit: the fixed body plus auto-placed pins. Returns `Err(msg)`
/// when the unit's pins don't fit the glyph (wrong input count, not exactly one
/// output, or unsupported pin types) so the caller can fall back to a rectangle.
fn build_glyph_unit(
    pins_val: &[serde_json::Value],
    g: Glyph,
) -> Result<(String, SymbolRect, Vec<ResolvedPin>), String> {
    let mut inputs: Vec<&serde_json::Value> = Vec::new();
    let mut outputs: Vec<&serde_json::Value> = Vec::new();
    let mut powers: Vec<&serde_json::Value> = Vec::new();
    let mut others = 0usize;
    for p in pins_val {
        match p["type"].as_str().unwrap_or("passive") {
            "input" => inputs.push(p),
            "output" | "tri_state" | "open_collector" | "open_emitter" => outputs.push(p),
            "power_in" | "power_out" => powers.push(p),
            _ => others += 1,
        }
    }

    let want = g.input_count();
    if inputs.len() != want {
        return Err(format!(
            "glyph '{}' expects {} input pin(s) but {} were given; drew a rectangle instead",
            g.name(),
            want,
            inputs.len()
        ));
    }
    if outputs.len() != 1 {
        return Err(format!(
            "glyph '{}' expects exactly 1 output pin but {} were given; drew a rectangle instead",
            g.name(),
            outputs.len()
        ));
    }
    if others > 0 {
        return Err(format!(
            "glyph '{}' only supports input/output/power pins; drew a rectangle instead",
            g.name()
        ));
    }

    let geom = glyph_geom(g);
    let mut sexp = geom.body.clone();
    let mut resolved = Vec::with_capacity(pins_val.len());

    // Inputs map onto the glyph anchors in the caller's order (top-to-bottom).
    for (p, &(x, y, angle, length)) in inputs.iter().zip(geom.inputs.iter()) {
        resolved.push(ResolvedPin::new(p, x, y));
        let number = p["number"].as_str().unwrap_or("1");
        let name = p["name"].as_str().unwrap_or("~");
        let style = pin_style_token(p["style"].as_str());
        sexp.push_str(&emit_pin(
            "input",
            style,
            x,
            y,
            angle,
            length,
            name,
            number,
            GLYPH_PIN_NAME_TEXT,
            pin_is_hidden(p),
        ));
    }

    // The single output sits at the apex; inverting glyphs default to an
    // inverted pin (the bubble), but the caller can override via `style`.
    let out = outputs[0];
    let out_number = out["number"].as_str().unwrap_or("1");
    let out_name = out["name"].as_str().unwrap_or("~");
    let out_type = out["type"].as_str().unwrap_or("output");
    let out_style = match out["style"].as_str() {
        Some(s) => pin_style_token(Some(s)),
        None if g.is_inverting() => "inverted",
        None => "line",
    };
    let (ox, oy, oa, ol) = geom.output;
    resolved.push(ResolvedPin::new(out, ox, oy));
    sexp.push_str(&emit_pin(
        out_type,
        out_style,
        ox,
        oy,
        oa,
        ol,
        out_name,
        out_number,
        GLYPH_PIN_NAME_TEXT,
        pin_is_hidden(out),
    ));

    // Power pins (e.g. a single op-amp's V+/V-) enter vertically, alternating
    // top/bottom, with their roots on the body outline so they touch the shape.
    for (i, p) in powers.iter().enumerate() {
        let number = p["number"].as_str().unwrap_or("1");
        let name = p["name"].as_str().unwrap_or("~");
        let ptype = p["type"].as_str().unwrap_or("power_in");
        let style = pin_style_token(p["style"].as_str());
        let length = 2.54;
        let (x, y, angle) = if i % 2 == 0 {
            let (ax, ay) = geom.power_top;
            (ax, ay + length, 270.0) // bulb above, root on the top edge
        } else {
            let (ax, ay) = geom.power_bottom;
            (ax, ay - length, 90.0) // bulb below, root on the bottom edge
        };
        resolved.push(ResolvedPin::new(p, x, y));
        sexp.push_str(&emit_pin(
            ptype,
            style,
            x,
            y,
            angle,
            length,
            name,
            number,
            GLYPH_PIN_NAME_TEXT,
            pin_is_hidden(p),
        ));
    }

    Ok((sexp, Some(geom.rect), resolved))
}

/// Build one unit's inner S-expression — an optional body (a rectangle, or a
/// conventional `glyph` shape) followed by its pins — and return it with the
/// body rect (used for reference/value placement) and an optional warning (e.g.
/// a glyph that didn't fit its pins and fell back to a rectangle). Shared by the
/// single- and multi-unit paths.
///
/// Errors (#55) when a pin's electrical type is not one of KiCAD's 12 valid
/// values — the caller must not write anything to disk in that case.
/// Glyph units keep their fixed library-matching shape, so `show_names` only
/// reaches the rectangle path.
pub(super) fn build_symbol_unit(
    pins_val: &[serde_json::Value],
    glyph: Option<Glyph>,
    show_names: bool,
    graphics: Option<&[serde_json::Value]>,
) -> anyhow::Result<BuiltUnit> {
    validate_pin_types(pins_val)?;
    // Caller-supplied geometry wins over every body. The automatic rectangle
    // exists to give pins something to sit on; a caller who has drawn the body
    // does not want a box around their drawing, and — because the auto path
    // slides pins out to the rectangle it computed — does not want their pin
    // coordinates moved either. Both follow from taking this branch (#501).
    if let Some(graphics) = graphics {
        validate_graphics(graphics).map_err(|e| anyhow::anyhow!(e))?;
        let (body_sexp, rect) = emit_symbol_graphics(graphics);
        let mut pins_sexp = String::new();
        let mut resolved = Vec::with_capacity(pins_val.len());
        for pin in pins_val {
            // The handler refuses a missing coordinate before this, where it
            // can name `units[i].pins[j].x`. Restating the contract where the
            // value is actually used keeps a future drawn call site from
            // reintroducing the origin the caller never gave.
            let (Some(x), Some(y)) = (pin["x"].as_f64(), pin["y"].as_f64()) else {
                anyhow::bail!(
                    "pin \"{}\" has no numeric x and y, and a drawn body writes pin coordinates exactly as supplied",
                    pin["number"].as_str().unwrap_or("1")
                );
            };
            resolved.push(ResolvedPin::new(pin, x, y));
            pins_sexp.push_str(&emit_pin(
                pin["type"].as_str().unwrap_or("passive"),
                pin_style_token(pin["style"].as_str()),
                x,
                y,
                pin["angle"].as_f64().unwrap_or(0.0),
                pin["length"].as_f64().unwrap_or(2.54),
                pin["name"].as_str().unwrap_or("~"),
                pin["number"].as_str().unwrap_or("1"),
                PIN_TEXT,
                pin_is_hidden(pin),
            ));
        }
        return Ok(BuiltUnit {
            sexp: format!("{body_sexp}{pins_sexp}"),
            rect,
            // Geometry replaces the body, so an explicitly requested glyph was
            // not drawn. Say so rather than discarding it silently.
            warning: glyph.map(|g| {
                format!(
                    "glyph '{}' was not drawn: this unit supplies its own graphics",
                    g.name()
                )
            }),
            body: "graphics",
            pins: resolved,
        });
    }
    if let Some(g) = glyph {
        if g != Glyph::Rectangle {
            match build_glyph_unit(pins_val, g) {
                Ok((sexp, rect, pins)) => {
                    return Ok(BuiltUnit {
                        sexp,
                        rect,
                        warning: None,
                        body: g.name(),
                        pins,
                    })
                }
                Err(reason) => {
                    let (sexp, rect, pins) = build_rect_unit(pins_val, show_names);
                    return Ok(BuiltUnit {
                        sexp,
                        rect,
                        warning: Some(reason),
                        body: "rectangle",
                        pins,
                    });
                }
            }
        }
    }
    let (sexp, rect, pins) = build_rect_unit(pins_val, show_names);
    Ok(BuiltUnit {
        sexp,
        rect,
        warning: None,
        body: "rectangle",
        pins,
    })
}

/// Read a `graphics` argument, keeping **present** and **absent** apart.
///
/// Two distinct mistakes live here. `as_array().unwrap_or_default()` turned a
/// present-but-malformed argument into an empty one, so the caller's drawing
/// went nowhere and the call still succeeded. Collapsing `[]` into "absent"
/// then lost the other half: #501 suppresses the automatic body "whenever
/// `graphics` is given for that unit", and `[]` **is** given — it asks for a
/// symbol with no body at all, which is a thing a caller can legitimately want
/// and cannot otherwise express. `None` here means the key was omitted; `Some`
/// means it was supplied, empty or not.
pub(super) fn graphics_arg(
    value: &serde_json::Value,
    whose: &str,
) -> Result<Option<Vec<serde_json::Value>>, String> {
    match value {
        serde_json::Value::Null => Ok(None),
        serde_json::Value::Array(items) => Ok(Some(items.clone())),
        _ => Err(format!(
            "{whose} 'graphics' must be an array of drawing primitives"
        )),
    }
}

/// Refuse a drawn-body pin whose coordinates the caller never supplied.
///
/// The whole justification for the drawn branch is that it writes pin
/// coordinates *exactly* as given, rather than sliding them out to a computed
/// edge the way the automatic rectangle does (#293). `x` and `y` therefore
/// become semantically required as soon as `graphics` is non-empty, though the
/// shared pin schema keeps them optional for the automatic path — and
/// `unwrap_or(0.0)` was answering a missing one with an origin the caller never
/// gave, which is the single invention this branch exists to prevent.
///
/// Every pin in a drawn scope is covered, power pins included. An earlier
/// version exempted the power pins of a triangular glyph, because those were
/// being moved to a generated unit whose coordinates `layout_power_unit`
/// overwrites. That exemption was precisely scoped and still wrong: the right
/// fix was to stop splitting when geometry supersedes the glyph at all, which
/// leaves nothing to exempt. **An exemption that makes a wrong behaviour
/// consistent is not a fix, and being able to prove it cannot drift is exactly
/// what makes it look finished.**
fn validate_drawn_pin_coordinates(
    pins: &[serde_json::Value],
    path: &str,
) -> Result<(), CallToolResult> {
    for (index, pin) in pins.iter().enumerate() {
        for field in ["x", "y"] {
            if pin[field].as_f64().is_none() {
                return Err(invalid_library_argument(
                    &format!("{path}[{index}].{field}"),
                    "missing or not a number; a drawn body writes pin coordinates exactly as supplied",
                ));
            }
        }
    }
    Ok(())
}

/// The keys each primitive accepts, which is also the list it requires: every
/// nested schema declares `additionalProperties: false` and names every one of
/// its properties in `required`. Held by hand beside the checks that use it,
/// like the other validators in this file rather than read back out of the
/// schema — `symbol_graphics_allow_exactly_the_schema_keys` fails if the two
/// ever disagree.
pub(super) fn graphics_primitive_keys(kind: &str) -> Option<&'static [&'static str]> {
    Some(match kind {
        "line" => &["type", "start", "end", "stroke_width_mm"],
        "arc" => &["type", "start", "mid", "end", "stroke_width_mm"],
        "rect" => &["type", "start", "end", "stroke_width_mm", "fill"],
        "circle" => &["type", "center", "radius_mm", "stroke_width_mm", "fill"],
        "poly" => &["type", "points", "stroke_width_mm", "fill"],
        _ => return None,
    })
}

/// Refuse geometry the emitter would otherwise draw wrongly or drop.
///
/// The MCP dispatch checks that required *arguments* are present; it does not
/// validate a `oneOf` inside an array item, so nothing else stops a misspelled
/// primitive type, a missing end point, or a fill outside the vocabulary. Each
/// of those has a silent wrong answer as its alternative — a typo'd `type`
/// emits nothing at all and still reports success — which is the failure this
/// validation exists to end (#501).
fn validate_graphics(graphics: &[serde_json::Value]) -> Result<(), String> {
    fn point(g: &serde_json::Value, key: &str, at: usize, kind: &str) -> Result<(), String> {
        let p = &g[key];
        if p["x"].as_f64().is_some() && p["y"].as_f64().is_some() {
            Ok(())
        } else {
            Err(format!(
                "graphics[{at}] ({kind}): '{key}' must be an object with numeric x and y"
            ))
        }
    }

    for (at, g) in graphics.iter().enumerate() {
        let Some(entry) = g.as_object() else {
            return Err(format!(
                "graphics[{at}]: each entry must be an object describing one primitive"
            ));
        };
        let kind = g["type"].as_str().unwrap_or("");
        let Some(allowed) = graphics_primitive_keys(kind) else {
            return Err(format!(
                "graphics[{at}]: unknown type {kind:?} — expected one of line, arc, rect, circle, poly"
            ));
        };
        // `fill` is the one unknown key with a better answer than the generic
        // one: it is real on the three closed shapes, and the emitter would
        // have honoured it on a `line` — the schema telling the caller one
        // thing while the writer does another. Say where a fill belongs
        // instead of only that it does not belong here.
        if matches!(kind, "line" | "arc") && !g["fill"].is_null() {
            return Err(format!(
                "graphics[{at}] ({kind}): '{kind}' takes no 'fill' — use 'poly' for a filled shape"
            ));
        }
        // The dispatch does not enforce a `oneOf` inside an array item, so any
        // other extra key reaches the emitter, which has no use for it and
        // drops it without a word — the caller asked for something the symbol
        // does not have. Refuse instead, the way the schema already says to.
        for key in entry.keys() {
            if !allowed.contains(&key.as_str()) {
                return Err(format!(
                    "graphics[{at}].{key} ({kind}): unknown field — {kind} takes {}",
                    allowed.join(", ")
                ));
            }
        }
        match kind {
            "line" => {
                point(g, "start", at, kind)?;
                point(g, "end", at, kind)?;
            }
            "arc" => {
                point(g, "start", at, kind)?;
                point(g, "mid", at, kind)?;
                point(g, "end", at, kind)?;
            }
            "rect" => {
                point(g, "start", at, kind)?;
                point(g, "end", at, kind)?;
            }
            "circle" => {
                point(g, "center", at, kind)?;
                match g["radius_mm"].as_f64() {
                    Some(r) if r > 0.0 => {}
                    Some(_) => {
                        return Err(format!(
                            "graphics[{at}] (circle): 'radius_mm' must be greater than 0"
                        ))
                    }
                    None => {
                        return Err(format!(
                            "graphics[{at}] (circle): 'radius_mm' is required and must be a number"
                        ))
                    }
                }
            }
            "poly" => {
                let points = g["points"].as_array().map(Vec::as_slice).unwrap_or(&[]);
                if points.len() < 3 {
                    return Err(format!(
                        "graphics[{at}] (poly): 'points' needs at least 3 entries, got {} — use type 'line' for two",
                        points.len()
                    ));
                }
                for (i, p) in points.iter().enumerate() {
                    if p["x"].as_f64().is_none() || p["y"].as_f64().is_none() {
                        return Err(format!(
                            "graphics[{at}] (poly): points[{i}] must have numeric x and y"
                        ));
                    }
                }
            }
            // Every other type was refused by `allowed_keys` above.
            _ => {}
        }
        match g["stroke_width_mm"].as_f64() {
            Some(w) if w >= 0.0 => {}
            Some(_) => {
                return Err(format!(
                    "graphics[{at}] ({kind}): 'stroke_width_mm' cannot be negative"
                ))
            }
            None => {
                return Err(format!(
                    "graphics[{at}] ({kind}): 'stroke_width_mm' is required and must be a number"
                ))
            }
        }
        // An unrecognised fill would silently become `none`, which is a
        // different drawing from the one that was asked for.
        match &g["fill"] {
            // The schema makes `fill` required on the three closed shapes. The
            // emitter answered a missing one with `(fill (type none))` — an
            // unfilled body where the stock libraries use KiCad's pale
            // `background`, chosen by Copperpour rather than by the caller.
            serde_json::Value::Null if matches!(kind, "rect" | "circle" | "poly") => {
                return Err(format!(
                    "graphics[{at}] ({kind}): 'fill' is required and must be \"none\", \"outline\" or \"background\""
                ))
            }
            serde_json::Value::Null => {}
            serde_json::Value::String(f)
                if matches!(f.as_str(), "none" | "outline" | "background") => {}
            other => {
                return Err(format!(
                    "graphics[{at}] ({kind}): 'fill' must be \"none\", \"outline\" or \"background\", got {other}"
                ))
            }
        }
    }
    Ok(())
}

/// Emit caller-supplied geometry into a symbol unit body, and report its
/// bounding box.
///
/// The primitive vocabulary is `set_footprint_graphics`'s, shared rather than
/// reinvented (#501): `line`, `arc`, `rect`, `circle`, `poly`, with points as
/// `{x, y}` and `stroke_width_mm`. Only the serialisation differs, because a
/// `.kicad_sym` writes `polyline`/`rectangle`/`circle`/`arc` where a footprint
/// writes `fp_*`, and only the fill vocabulary differs, because a symbol also
/// has KiCad's pale `background` body fill.
///
/// The bounding box is what the caller drew, so Reference and Value anchor to
/// the real body rather than to a rectangle that is no longer there.
fn emit_symbol_graphics(graphics: &[serde_json::Value]) -> (String, SymbolRect) {
    fn point(v: &serde_json::Value) -> (f64, f64) {
        (
            v["x"].as_f64().unwrap_or(0.0),
            v["y"].as_f64().unwrap_or(0.0),
        )
    }
    fn stroke(g: &serde_json::Value) -> String {
        format!(
            "(stroke (width {}) (type default))",
            fmt_f64(g["stroke_width_mm"].as_f64().unwrap_or(0.254))
        )
    }
    // KiCad symbol fills: `none`, `outline` (the shape's own colour) and
    // `background` (the pale body fill the automatic rectangle uses).
    fn fill(g: &serde_json::Value) -> String {
        let t = match g["fill"].as_str() {
            Some("outline") => "outline",
            Some("background") => "background",
            _ => "none",
        };
        format!("(fill (type {t}))")
    }

    let mut sexp = String::new();
    let mut bounds: Option<(f64, f64, f64, f64)> = None;
    let mut grow = |x: f64, y: f64| {
        bounds = Some(match bounds {
            None => (x, y, x, y),
            Some((min_x, min_y, max_x, max_y)) => {
                (min_x.min(x), min_y.min(y), max_x.max(x), max_y.max(y))
            }
        });
    };

    for g in graphics {
        match g["type"].as_str().unwrap_or("") {
            kind @ ("line" | "poly") => {
                let points: Vec<(f64, f64)> = if kind == "line" {
                    vec![point(&g["start"]), point(&g["end"])]
                } else {
                    g["points"]
                        .as_array()
                        .into_iter()
                        .flatten()
                        .map(point)
                        .collect()
                };
                if points.is_empty() {
                    continue;
                }
                let mut pts = String::new();
                for (x, y) in &points {
                    grow(*x, *y);
                    pts.push_str(&format!(" (xy {} {})", fmt_f64(*x), fmt_f64(*y)));
                }
                sexp.push_str(&format!(
                    "\n      (polyline\n        (pts{})\n        {}\n        {}\n      )",
                    pts,
                    stroke(g),
                    fill(g)
                ));
            }
            "rect" => {
                let (sx, sy) = point(&g["start"]);
                let (ex, ey) = point(&g["end"]);
                grow(sx, sy);
                grow(ex, ey);
                sexp.push_str(&format!(
                    "\n      (rectangle (start {} {}) (end {} {})\n        {}\n        {}\n      )",
                    fmt_f64(sx),
                    fmt_f64(sy),
                    fmt_f64(ex),
                    fmt_f64(ey),
                    stroke(g),
                    fill(g)
                ));
            }
            "circle" => {
                let (cx, cy) = point(&g["center"]);
                let r = g["radius_mm"].as_f64().unwrap_or(0.0);
                grow(cx - r, cy - r);
                grow(cx + r, cy + r);
                sexp.push_str(&format!(
                    "\n      (circle (center {} {}) (radius {})\n        {}\n        {}\n      )",
                    fmt_f64(cx),
                    fmt_f64(cy),
                    fmt_f64(r),
                    stroke(g),
                    fill(g)
                ));
            }
            "arc" => {
                let (sx, sy) = point(&g["start"]);
                let (mx, my) = point(&g["mid"]);
                let (ex, ey) = point(&g["end"]);
                for (x, y) in [(sx, sy), (mx, my), (ex, ey)] {
                    grow(x, y);
                }
                sexp.push_str(&format!(
                    "\n      (arc (start {} {}) (mid {} {}) (end {} {})\n        {}\n        {}\n      )",
                    fmt_f64(sx),
                    fmt_f64(sy),
                    fmt_f64(mx),
                    fmt_f64(my),
                    fmt_f64(ex),
                    fmt_f64(ey),
                    stroke(g),
                    fill(g)
                ));
            }
            _ => {}
        }
    }
    (sexp, bounds)
}

/// Space pins whose edge-axis coordinate was omitted, without moving supplied
/// edge-axis coordinates. Repeated physical numbers share one automatic slot.
fn distribute_rect_pins(pins: &[serde_json::Value], geoms: &mut [PinGeom]) {
    for edge in [PinEdge::Left, PinEdge::Right, PinEdge::Top, PinEdge::Bottom] {
        let vertical = matches!(edge, PinEdge::Left | PinEdge::Right);
        let axis = if vertical { "y" } else { "x" };
        let indices: Vec<_> = geoms
            .iter()
            .enumerate()
            .filter(|(_, g)| pin_edge(g.angle) == Some(edge))
            .map(|(i, _)| i)
            .collect();
        let mut occupied: Vec<_> = indices
            .iter()
            .filter_map(|&i| pins[i][axis].as_f64())
            .collect();
        let mut numbers = Vec::new();
        for &i in &indices {
            if pins[i][axis].as_f64().is_none() {
                let number = pins[i]["number"].as_str().unwrap_or("1");
                if !numbers.contains(&number) {
                    numbers.push(number);
                }
            }
        }
        let mut positions = Vec::new();
        let mut candidate = (numbers.len().saturating_sub(1) as f64) * 1.27;
        for _ in &numbers {
            while occupied
                .iter()
                .any(|value| (value - candidate).abs() < 1e-9)
            {
                candidate -= 2.54;
            }
            candidate = copperpour_sexp::geometry::round6(candidate);
            positions.push(candidate);
            occupied.push(candidate);
            candidate -= 2.54;
        }
        for i in indices {
            if pins[i][axis].as_f64().is_none() {
                let slot = numbers
                    .iter()
                    .position(|number| *number == pins[i]["number"].as_str().unwrap_or("1"))
                    .expect("omitted-coordinate pin owns a slot");
                if vertical {
                    geoms[i].y = positions[slot];
                } else {
                    geoms[i].x = positions[slot];
                }
            }
        }
    }
}

/// The default rectangle body + caller-positioned or distributed pins. Pin types are assumed
/// already validated by `build_symbol_unit`.
fn build_rect_unit(
    pins_val: &[serde_json::Value],
    show_names: bool,
) -> (String, SymbolRect, Vec<ResolvedPin>) {
    let mut pin_geoms: Vec<PinGeom> = Vec::new();
    for pin in pins_val {
        pin_geoms.push(PinGeom {
            x: pin["x"].as_f64().unwrap_or(0.0),
            y: pin["y"].as_f64().unwrap_or(0.0),
            angle: pin["angle"].as_f64().unwrap_or(0.0),
            length: pin["length"].as_f64().unwrap_or(2.54),
            name: pin["name"].as_str().unwrap_or("~").to_string(),
        });
    }
    distribute_rect_pins(pins_val, &mut pin_geoms);
    let body = symbol_body_rect(&pin_geoms, show_names);
    // Fitting the names can push the body past the pins that defined it.
    // Slide them back out, keeping the length the caller asked for.
    if let Some((min_x, min_y, max_x, max_y)) = body {
        for g in &mut pin_geoms {
            match pin_edge(g.angle) {
                Some(PinEdge::Left) => g.x = min_x - g.length,
                Some(PinEdge::Right) => g.x = max_x + g.length,
                Some(PinEdge::Bottom) => g.y = min_y - g.length,
                Some(PinEdge::Top) => g.y = max_y + g.length,
                None => {}
            }
        }
    }

    let mut pins_sexp = String::new();
    let mut resolved = Vec::with_capacity(pins_val.len());
    for (pin, g) in pins_val.iter().zip(&pin_geoms) {
        resolved.push(ResolvedPin::new(pin, g.x, g.y));
        pins_sexp.push_str(&emit_pin(
            pin["type"].as_str().unwrap_or("passive"),
            pin_style_token(pin["style"].as_str()),
            g.x,
            g.y,
            g.angle,
            g.length,
            &g.name,
            pin["number"].as_str().unwrap_or("1"),
            PIN_TEXT,
            pin_is_hidden(pin),
        ));
    }
    let body_sexp = match body {
        Some((min_x, min_y, max_x, max_y)) => format!(
            "\n      (rectangle (start {:.4} {:.4}) (end {:.4} {:.4})\n        (stroke (width 0.254) (type default))\n        (fill (type background))\n      )",
            min_x, min_y, max_x, max_y
        ),
        None => String::new(),
    };
    (format!("{}{}", body_sexp, pins_sexp), body, resolved)
}

type SymbolRect = Option<(f64, f64, f64, f64)>;

/// Where a pin ended up, next to where the caller asked for it.
///
/// `create_symbol` treats a pin's `x`/`y` as a starting point, not a fixed
/// position: the body is sized to fit the pin names and every pin on an edge is
/// then aligned to that edge. Callers cannot plan wiring from the coordinates
/// they sent, so the resolved ones are reported back.
struct ResolvedPin {
    number: String,
    name: String,
    requested: Option<(f64, f64)>,
    x: f64,
    y: f64,
    hide: bool,
}

impl ResolvedPin {
    fn new(pin: &serde_json::Value, x: f64, y: f64) -> Self {
        let requested = match (pin["x"].as_f64(), pin["y"].as_f64()) {
            (Some(rx), Some(ry)) => Some((rx, ry)),
            _ => None,
        };
        ResolvedPin {
            number: pin["number"].as_str().unwrap_or("1").to_string(),
            name: pin["name"].as_str().unwrap_or("~").to_string(),
            requested,
            // To the six decimals `fmt_f64` writes, so the report matches the
            // file (#747).
            x: round6(x),
            y: round6(y),
            hide: pin_is_hidden(pin),
        }
    }

    /// How far the pin was moved, or `None` when the caller named no position.
    fn displacement(&self) -> Option<f64> {
        let (rx, ry) = self.requested?;
        let moved = ((self.x - rx).powi(2) + (self.y - ry).powi(2)).sqrt();
        (moved > POSITION_EPSILON).then_some(moved)
    }

    fn to_json(&self) -> serde_json::Value {
        let mut out = json!({
            "number": self.number,
            "name": self.name,
            "x": self.x,
            "y": self.y
        });
        if self.hide {
            out["hide"] = json!(true);
        }
        // Only when it differs, so the common case stays compact and a moved
        // pin stands out.
        if let (Some((rx, ry)), Some(_)) = (self.requested, self.displacement()) {
            out["requested"] = json!({ "x": rx, "y": ry });
        }
        out
    }
}

/// Below this a difference is float noise from the body arithmetic, not a move.
const POSITION_EPSILON: f64 = 1e-6;

/// One unit's rendered body, and what became of the pins the caller placed.
pub(super) struct BuiltUnit {
    sexp: String,
    rect: SymbolRect,
    /// A glyph that did not fit its pins and fell back to a rectangle.
    warning: Option<String>,
    /// `"rectangle"` or the glyph's name — a glyph places pins by type, so a
    /// moved pin there is intended rather than a surprise.
    body: &'static str,
    pins: Vec<ResolvedPin>,
}

impl BuiltUnit {
    /// A per-unit summary of the auto-size override, or `None` when nothing
    /// moved. Per pin would be a dozen lines of noise on a normal symbol, and a
    /// glyph unit moves every pin by design.
    fn displacement_warning(&self, unit_label: &str) -> Option<String> {
        if self.body != "rectangle" {
            return None;
        }
        let moved: Vec<f64> = self
            .pins
            .iter()
            .filter_map(ResolvedPin::displacement)
            .collect();
        let furthest = moved.iter().copied().fold(f64::NEG_INFINITY, f64::max);
        (!moved.is_empty()).then(|| {
            format!(
                "{unit_label}: the body was sized to fit the pin names, moving {} of {} pins by up to {:.2} mm; the resolved positions are in `units`",
                moved.len(),
                self.pins.len(),
                furthest
            )
        })
    }

    /// A per-unit summary when any resolved pin sits off KiCad's 1.27 mm
    /// schematic grid, or `None` when every pin is grid-aligned.
    fn off_grid_pin_warning(&self, unit_label: &str) -> Option<String> {
        let off_grid: Vec<&ResolvedPin> = self
            .pins
            .iter()
            .filter(|pin| !is_on_symbol_grid(pin.x) || !is_on_symbol_grid(pin.y))
            .collect();
        if off_grid.is_empty() {
            return None;
        }
        const MAX_LISTED: usize = 5;
        let listed: Vec<String> = off_grid
            .iter()
            .take(MAX_LISTED)
            .map(|pin| {
                format!(
                    "pin {} ({}, {})",
                    pin.number,
                    fmt_f64(pin.x),
                    fmt_f64(pin.y)
                )
            })
            .collect();
        let more = if off_grid.len() > MAX_LISTED {
            format!(" and {} more", off_grid.len() - MAX_LISTED)
        } else {
            String::new()
        };
        Some(format!(
            "{unit_label}: {} of {} pins are off the 1.27 mm schematic grid ({}{more}); grid-snapped wires and power symbols may not connect — use multiples of 1.27 mm or omit x/y for automatic layout",
            off_grid.len(),
            self.pins.len(),
            listed.join(", "),
        ))
    }

    fn to_json(&self, unit: usize) -> serde_json::Value {
        json!({
            "unit": unit,
            "body": self.body,
            "pins": self.pins.iter().map(ResolvedPin::to_json).collect::<Vec<_>>()
        })
    }
}

pub(crate) async fn handle_create_symbol(
    args: &serde_json::Value,
    _ctx: &ToolContext,
) -> anyhow::Result<CallToolResult> {
    let lib_path = get_path(args, "library_path")?;
    // Schema-required. Defaulting them appended `(symbol "Symbol" …)` with
    // reference `U` to the library (#218).
    let name = match require_str(args, "name") {
        Ok(v) => v,
        Err(e) => return Ok(e),
    };
    let ref_prefix = match require_str(args, "reference_prefix") {
        Ok(v) => v,
        Err(e) => return Ok(e),
    };
    let value_str = args["value"].as_str().unwrap_or(name);
    // `~` is KiCAD's legacy "no datasheet" placeholder. Its library loader
    // normalises it to the empty string and its schematic lib_symbols loader
    // does not, so a symbol carrying `~` never matches its own library copy in
    // ERC. Write the normalised form on both paths.
    let datasheet = match args["datasheet"].as_str().unwrap_or("") {
        "~" => "",
        s => s,
    };
    let show_names = args["show_pin_names"].as_bool().unwrap_or(true);
    let show_numbers = args["show_pin_numbers"].as_bool().unwrap_or(true);
    let fields = match crate::tools::symbol_fields::parse_symbol_fields(args) {
        Ok(fields) => fields,
        Err(error) => return Ok(error),
    };

    let SymbolItems {
        pins,
        units: unit_objs,
        power_pins,
    } = match parse_symbol_items(args) {
        Ok(items) => items,
        Err(error) => return Ok(error),
    };

    // Optional conventional body shape. `glyph` may be set at the symbol level
    // (a default for every unit) and/or per unit (overriding the default).
    let mut warnings: Vec<String> = Vec::new();
    // Where each unit's pins actually ended up. The caller's x/y are a starting
    // point, so this is the only way to plan wiring without re-measuring the
    // finished symbol with batch_get_schematic_pin_locations.
    let mut units_report: Vec<serde_json::Value> = Vec::new();
    let sym_glyph = match args["glyph"].as_str() {
        None => None,
        Some(s) => match Glyph::parse(s) {
            Some(g) => Some(g),
            None => {
                warnings.push(format!("unknown glyph '{}'; used a rectangle", s));
                None
            }
        },
    };

    // Multi-unit when `units` is a non-empty array; otherwise the single-unit
    // `pins` path. Sub-symbols are named NAME_<unit>_1; units 1..N are the
    // individual units, and shared `power_pins` become a dedicated final unit.
    // Top-level geometry belongs to the single-unit `pins` path, the same way
    // top-level `pins` and `glyph` do.
    let sym_graphics = match graphics_arg(&args["graphics"], "top-level") {
        Ok(g) => g,
        Err(e) => return Ok(CallToolResult::error(e)),
    };
    // Top-level `pins` is superseded by `units` and the schema says so, but
    // losing a redundant pin list is not the same as losing a drawing: geometry
    // sent here while `units` is in use would never reach the file, and the
    // symbol would come out with an automatic rectangle and a success.
    if sym_graphics.is_some() && !unit_objs.is_empty() {
        return Ok(CallToolResult::error(
            "top-level 'graphics' has no effect when 'units' is given: move it to the \
             unit that should carry it, as units[].graphics",
        ));
    }

    let mut units_sexp = String::new();
    let unit_symbol = |unit: usize, inner: &str| {
        format!(
            "\n    (symbol {}{inner}\n    )",
            quote_string(&format!("{name}_{unit}_1"))
        )
    };
    let unit_count: usize;
    let power_pin_count: usize;
    let ref_body: SymbolRect;
    if unit_objs.is_empty() {
        let pins_val = pins;
        // A single-unit triangular glyph (op-amp/buffer/inverter/schmitt) has no
        // room for power-pin names on its narrow apex, so if it carries power
        // pins, split them onto a dedicated rectangular power unit (like KiCAD's
        // multi-unit op-amps) instead of drawing them on the triangle.
        // Only when the glyph is the thing actually drawn. Supplied geometry
        // supersedes the glyph, and the split exists solely because a
        // triangle's apex has no room for power-pin names — a body the caller
        // drew has whatever room they gave it. Splitting anyway moved their
        // power pins to a generated unit and let `layout_power_unit` overwrite
        // the coordinates, breaking both advertised contracts at once: that
        // geometry wins, and that pin coordinates are written as supplied.
        let split_power = sym_graphics.is_none()
            && matches!(sym_glyph, Some(g) if g.is_triangular())
            && pins_val.iter().any(is_power_pin);
        if sym_graphics.is_some() {
            if let Err(error) = validate_drawn_pin_coordinates(&pins_val, "pins") {
                return Ok(error);
            }
        }
        if split_power {
            let signal: Vec<serde_json::Value> = pins_val
                .iter()
                .filter(|p| !is_power_pin(p))
                .cloned()
                .collect();
            let power: Vec<serde_json::Value> = pins_val
                .iter()
                .filter(|p| is_power_pin(p))
                .cloned()
                .collect();
            // Unit 1: the triangle with its signal pins.
            let unit1 =
                match build_symbol_unit(&signal, sym_glyph, show_names, sym_graphics.as_deref()) {
                    Ok(v) => v,
                    Err(e) => return Ok(CallToolResult::error(e.to_string())),
                };
            warnings.extend(unit1.warning.clone());
            warnings.extend(unit1.displacement_warning("unit 1"));
            warnings.extend(unit1.off_grid_pin_warning("unit 1"));
            units_report.push(unit1.to_json(1));
            let (inner1, body1) = (unit1.sexp, unit1.rect);
            units_sexp.push_str(&unit_symbol(1, &inner1));
            // Unit 2: a rectangular power unit.
            let power_laid = layout_power_unit(&power);
            let unit2 = match build_symbol_unit(&power_laid, None, show_names, None) {
                Ok(v) => v,
                Err(e) => return Ok(CallToolResult::error(e.to_string())),
            };
            warnings.extend(unit2.warning.clone());
            warnings.extend(unit2.displacement_warning("unit 2"));
            warnings.extend(unit2.off_grid_pin_warning("unit 2"));
            units_report.push(unit2.to_json(2));
            let inner2 = unit2.sexp;
            units_sexp.push_str(&unit_symbol(2, &inner2));
            unit_count = 2;
            power_pin_count = power_laid.len();
            ref_body = body1;
        } else {
            // Single unit: body + all pins live in NAME_0_1 (unchanged behavior).
            let unit = match build_symbol_unit(
                &pins_val,
                sym_glyph,
                show_names,
                sym_graphics.as_deref(),
            ) {
                Ok(v) => v,
                Err(e) => return Ok(CallToolResult::error(e.to_string())),
            };
            warnings.extend(unit.warning.clone());
            warnings.extend(unit.displacement_warning("unit 1"));
            warnings.extend(unit.off_grid_pin_warning("unit 1"));
            units_report.push(unit.to_json(1));
            let (inner, body) = (unit.sexp, unit.rect);
            units_sexp.push_str(&unit_symbol(0, &inner));
            unit_count = 1;
            power_pin_count = pins_val.iter().filter(|pin| is_power_pin(pin)).count();
            ref_body = body;
        }
    } else {
        // Multi-unit: each signal unit is NAME_1_1..NAME_N_1, and the power
        // pins (if any) become a dedicated FINAL unit rather than being drawn
        // on every unit. KiCAD's own multi-unit parts do this (e.g. 74LS00 has
        // the four gates as units 1..4 and VCC/GND as unit 5). It means the
        // power pins appear on exactly one placed unit instead of on every
        // unit, where each duplicate would otherwise need wiring to pass ERC.
        let mut first_body: SymbolRect = None;
        for (i, u) in unit_objs.iter().enumerate() {
            let unit_pins = u["pins"].as_array().cloned().unwrap_or_default();
            // A per-unit `glyph` overrides the symbol-level default.
            let unit_glyph = match u["glyph"].as_str() {
                None => sym_glyph,
                Some(s) => match Glyph::parse(s) {
                    Some(g) => Some(g),
                    None => {
                        warnings.push(format!(
                            "unit {}: unknown glyph '{}'; used a rectangle",
                            i + 1,
                            s
                        ));
                        Some(Glyph::Rectangle)
                    }
                },
            };
            let unit_graphics = match graphics_arg(&u["graphics"], &format!("unit {}:", i + 1)) {
                Ok(g) => g,
                Err(e) => return Ok(CallToolResult::error(e)),
            };
            if unit_graphics.is_some() {
                if let Err(error) =
                    validate_drawn_pin_coordinates(&unit_pins, &format!("units[{i}].pins"))
                {
                    return Ok(error);
                }
            }
            let unit = match build_symbol_unit(
                &unit_pins,
                unit_glyph,
                show_names,
                unit_graphics.as_deref(),
            ) {
                Ok(v) => v,
                Err(e) => return Ok(CallToolResult::error(e.to_string())),
            };
            let unit_label = format!("unit {}", i + 1);
            if let Some(w) = unit.warning.clone() {
                warnings.push(format!("{unit_label}: {w}"));
            }
            warnings.extend(unit.displacement_warning(&unit_label));
            warnings.extend(unit.off_grid_pin_warning(&unit_label));
            units_report.push(unit.to_json(i + 1));
            let (inner, body) = (unit.sexp, unit.rect);
            if i == 0 {
                first_body = body;
            }
            units_sexp.push_str(&unit_symbol(i + 1, &inner));
        }
        let mut total = unit_objs.len();
        if !power_pins.is_empty() {
            // The power unit is always a rectangle.
            let power_laid = if power_pins_need_layout(&power_pins) {
                layout_power_unit(&power_pins)
            } else {
                power_pins.clone()
            };
            let unit = match build_symbol_unit(&power_laid, None, show_names, None) {
                Ok(v) => v,
                Err(e) => return Ok(CallToolResult::error(e.to_string())),
            };
            if let Some((first, second)) = unit.pins.iter().enumerate().find_map(|(i, first)| {
                unit.pins[i + 1..]
                    .iter()
                    .find(|second| {
                        first.number != second.number
                            && (first.x - second.x).abs() < POSITION_EPSILON
                            && (first.y - second.y).abs() < POSITION_EPSILON
                    })
                    .map(|second| (first, second))
            }) {
                return Ok(invalid_library_argument(
                    "power_pins",
                    format!(
                        "pins {} ({}) and {} ({}) resolve to the same connection point ({:.4}, {:.4}); provide distinct angles or positions",
                        first.number, first.name, second.number, second.name, first.x, first.y
                    ),
                ));
            }
            total += 1;
            let unit_label = format!("unit {total}");
            warnings.extend(unit.displacement_warning(&unit_label));
            warnings.extend(unit.off_grid_pin_warning(&unit_label));
            units_report.push(unit.to_json(total));
            let inner = unit.sexp;
            units_sexp.push_str(&unit_symbol(total, &inner));
        }
        unit_count = total;
        power_pin_count = power_pins.len();
        ref_body = first_body;
    }

    // Reference/value placement above/below the (first) unit body (Y-up).
    let (ref_y, value_y) = match ref_body {
        Some((_, min_y, _, max_y)) => (max_y + 2.54, min_y - 2.54),
        None => (2.54, -2.54),
    };

    let numbers = if show_numbers {
        String::new()
    } else {
        "\n    (pin_numbers hide)".to_string()
    };
    let names = if show_names {
        format!(
            "\n    (pin_names\n      (offset {}))",
            fmt_f64(PIN_NAME_OFFSET)
        )
    } else {
        format!(
            "\n    (pin_names\n      (offset {})\n      (hide yes))",
            fmt_f64(PIN_NAME_OFFSET)
        )
    };
    let visible_property = |name: &str, value: &str, y: f64| {
        format!(
            "\n    (property {} {} (at 0 {y:.4} 0) \
             (show_name no) (do_not_autoplace no) \
             (effects (font (size 1.27 1.27))))",
            quote_string(name),
            quote_string(value)
        )
    };
    let hidden_property = |name: &str, value: &str| {
        format!(
            "\n    (property {} {} (at 0 0 0) \
             (show_name no) (do_not_autoplace no) (hide yes) \
             (effects (font (size 1.27 1.27))))",
            quote_string(name),
            quote_string(value)
        )
    };
    let custom_properties: String = fields
        .created_after_description()
        .into_iter()
        .map(|(name, value)| hidden_property(name, value))
        .collect();

    let symbol_sexp = format!(
        "\n  (symbol {}{}{}\n    (exclude_from_sim no)\n    (in_bom yes)\n    (on_board yes)\n    (in_pos_files yes)\n    (duplicate_pin_numbers_are_jumpers no){}{}{}{}{}{}{}\n    (embedded_fonts no)\n  )",
        quote_string(name),
        numbers,
        names,
        visible_property("Reference", ref_prefix, ref_y),
        visible_property("Value", value_str, value_y),
        hidden_property("Footprint", fields.footprint.as_deref().unwrap_or("")),
        hidden_property("Datasheet", datasheet),
        hidden_property("Description", fields.description.as_deref().unwrap_or("")),
        custom_properties,
        units_sexp
    );

    let read = if lib_path.exists() {
        Some(read_consistent(&lib_path)?)
    } else {
        None
    };
    let content = match &read {
        Some(source) => {
            // KiCad keeps one of two same-named symbols on its next save,
            // without saying which (#123).
            if top_level_symbol_names(source)?.iter().any(|n| n == name) {
                return Ok(crate::tools::invalid_arg(
                    "name",
                    &format!(
                        "'{}' already defines symbol '{name}'; pick another name or \
                         delete_symbol it first",
                        lib_path.display()
                    ),
                ));
            }
            let mut content = source.replace("(version 20240108)", "(version 20251024)");
            if !content.contains("(generator_version ") {
                if let Some(position) = content.find("(generator \"") {
                    if let Some(end) = content[position..].find(')') {
                        let insert_at = position + end + 1;
                        content.insert_str(insert_at, "\n  (generator_version \"10.0\")");
                    }
                }
            }
            content
        }
        None => "(kicad_symbol_lib\n  (version 20251024)\n  (generator \"copperpour\")\n  \
                 (generator_version \"10.0\")\n)\n"
            .to_string(),
    };

    // Insert before closing paren of root expression
    let insert_pos = content.rfind(')').unwrap_or(content.len());
    let new_content = format!("{}{}\n)", &content[..insert_pos], symbol_sexp);

    if let Some(parent) = lib_path.parent() {
        tokio::fs::create_dir_all(parent).await?;
    }
    if let Some(refusal) = crate::tools::write_if_unchanged(
        &lib_path,
        "symbol library",
        "create_symbol",
        read.as_deref(),
        new_content,
    )? {
        return Ok(refusal);
    }

    let mut result = json!({
        "success": true,
        "symbol": name,
        "library": lib_path.to_str().unwrap_or(""),
        "unit_count": unit_count,
        "power_pin_count": power_pin_count
    });
    result["units"] = json!(units_report);
    if !warnings.is_empty() {
        result["warnings"] = json!(warnings);
    }

    Ok(CallToolResult::text(
        serde_json::to_string(&result).unwrap(),
    ))
}

pub(crate) async fn handle_delete_symbol(
    args: &serde_json::Value,
    _ctx: &ToolContext,
) -> anyhow::Result<CallToolResult> {
    let lib_path = get_path(args, "library_path")?;
    let symbol_name = match require_str(args, "symbol_name") {
        Ok(v) => v,
        Err(e) => return Ok(e),
    };

    let content = read_consistent(&lib_path)?;

    // Structural, not a fixed-indent search: KiCad saves libraries
    // tab-indented, and a nested unit `(symbol "NAME_1_1"` is never a match.
    let (start, end) = find_direct_child_blocks(&content, "kicad_symbol_lib")
        .into_iter()
        .find(|&(s, e)| {
            crate::tools::lib_symbol_name(&content[s..e]).as_deref() == Some(symbol_name)
        })
        .ok_or_else(|| anyhow::anyhow!("Symbol '{}' not found in library", symbol_name))?;

    let line_start = content[..start].rfind('\n').map_or(0, |i| i + 1);
    let block_start = if content[line_start..start].trim().is_empty() {
        line_start
    } else {
        start
    };
    let end = match content[end..].find('\n') {
        Some(n) if content[end..end + n].trim().is_empty() => end + n + 1,
        _ => end,
    };

    let new_content = format!("{}{}", &content[..block_start], &content[end..]);
    write_atomic_if_unchanged(&lib_path, &content, &new_content)?;

    Ok(CallToolResult::text(
        serde_json::to_string(&json!({
            "success": true,
            "deleted": symbol_name
        }))
        .unwrap(),
    ))
}

#[cfg(test)]
mod omitted_rect_pin_tests {
    use super::*;
    use crate::mcp::handler::McpHandler;
    use crate::tools::ServerConfig;

    #[tokio::test]
    async fn served_rect_units_distribute_omitted_coordinates_and_read_back() {
        let handler = McpHandler::new(ServerConfig {
            eager_toolsets: true,
            ..Default::default()
        })
        .await
        .unwrap();
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("omitted.kicad_sym");
        let pins = |count: usize, offset: usize| {
            (0..count).map(|i| {
            json!({"number": (offset+i).to_string(), "name": format!("P{i}"), "type": "passive"})
        }).collect::<Vec<_>>()
        };
        let response = handler
            .handle_message(json!({
                "jsonrpc": "2.0", "id": 464, "method": "tools/call",
                "params": {"name": "create_symbol", "arguments": {
                "library_path": path, "name": "Omitted", "reference_prefix": "U",
                "units": [{"pins": pins(6, 1)}, {"pins": pins(6, 11)}, {"pins": pins(63, 21), "glyph": "opamp"}]
                }}
            }))
            .await
            .unwrap()
            .result
            .unwrap();
        assert_ne!(response["isError"], true, "{response}");
        let report: serde_json::Value =
            serde_json::from_str(response["content"][0]["text"].as_str().unwrap()).unwrap();
        // Optional local evidence only: hosted regression does not require KiCad.
        if let Some(cli) = crate::app_dirs::env_var("TEST_SYMBOL_CLI") {
            let result = std::process::Command::new(cli)
                .args(["sym", "upgrade", "--force"])
                .arg(&path)
                .output()
                .unwrap();
            assert!(
                result.status.success(),
                "{}",
                String::from_utf8_lossy(&result.stderr)
            );
        }
        let root = parse_sexp(&std::fs::read_to_string(&path).unwrap()).unwrap();
        let symbol = root.find("symbol").unwrap();
        for (i, count) in [6, 6, 63].into_iter().enumerate() {
            assert_eq!(report["units"][i]["body"], "rectangle");
            let unit_name = format!("Omitted_{}_1", i + 1);
            let unit = symbol
                .find_all("symbol")
                .into_iter()
                .find(|u| u.get(1).and_then(SexpNode::as_str) == Some(unit_name.as_str()))
                .unwrap();
            let written = unit.find_all("pin");
            assert_eq!(written.len(), count);
            let reported = report["units"][i]["pins"].as_array().unwrap();
            let mut positions = Vec::new();
            for (pin, observed) in written.iter().zip(reported) {
                let at = pin.find("at").unwrap();
                let position = (at.get_f64(1).unwrap(), at.get_f64(2).unwrap());
                assert!(
                    !positions.contains(&position),
                    "pins collided within {unit_name}"
                );
                positions.push(position);
                assert_eq!(observed["x"].as_f64().unwrap(), position.0);
                assert_eq!(observed["y"].as_f64().unwrap(), position.1);
                assert_eq!(pin.find_str("number"), observed["number"].as_str());
            }
            for pair in positions.windows(2) {
                assert!(((pair[0].1 - pair[1].1) - 2.54).abs() < 1e-9);
            }
        }
    }

    #[test]
    fn omitted_rect_axes_preserve_explicit_coordinates_and_stacks() {
        for angle in [0, 180, 90, 270] {
            let vertical = angle == 0 || angle == 180;
            let input = vec![
                json!({"number":"1","name":"EXPLICIT","type":"passive","angle":angle,"x":0,"y":0}),
                json!({"number":"2","name":"A","type":"passive","angle":angle}),
                json!({"number":"2","name":"STACK","type":"passive","angle":angle}),
                json!({"number":"3","name":"B","type":"passive","angle":angle}),
            ];
            let (_, _, resolved) = build_rect_unit(&input, true);
            let axis = |pin: &ResolvedPin| if vertical { pin.y } else { pin.x };
            assert_eq!(axis(&resolved[0]), 0.0);
            assert_eq!(axis(&resolved[1]), axis(&resolved[2]));
            assert_ne!(axis(&resolved[1]), axis(&resolved[3]));
            assert_ne!(axis(&resolved[1]), 0.0);
            assert_ne!(axis(&resolved[3]), 0.0);
            let explicit_only = vec![input[0].clone()];
            let (_, _, explicit) = build_rect_unit(&explicit_only, true);
            assert_eq!(
                (resolved[0].x, resolved[0].y),
                (explicit[0].x, explicit[0].y)
            );
        }
    }
}
