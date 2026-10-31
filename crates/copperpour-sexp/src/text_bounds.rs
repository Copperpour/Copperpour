//! The extent of the text a schematic sheet draws for its labels and for its
//! placed symbols' fields, placed as eeschema 10 places it (#111).
//!
//! A string's extent is its run across the ink of its glyphs. The run is the
//! length kicad-cli's plot gives the string (`textLength`, from
//! `FONT::StringBoundaryLimits`, which pads it by 1.5 pen widths at each end).
//! Across the run it holds the strokes as `FONT::getLinePositions`
//! (`common/font/font.cpp`) sets them on their baseline, with half the stroke
//! width each side: capitals and digits from 20/21 of the height above the
//! baseline to 1/21 below it (`STROKE_FONT_SCALE`, `FONT_OFFSET`), and other
//! glyphs as far as they reach in `common/newstroke_font.cpp`. An overbar is
//! 1.23 heights up (`METRICS::m_OverbarHeight`); sub- and superscripts are 0.8
//! high, 0.12 down or 0.28 up (`STROKE_FONT::GetTextAsGlyphs`).
//!
//! Where each string sits follows eeschema:
//! - a field: centred on the centre of the box `SCH_FIELD::GetBoundingBox`
//!   lays out (`eeschema/sch_field.cpp`), at the field's written `(at …)`,
//!   justified and turned by its own angle and then turned and mirrored with
//!   its symbol, and drawn upright there (`SCH_FIELD::Plot`, `GetDrawRotation`);
//! - a net label: from its anchor, raised off its wire by
//!   `SCH_LABEL_BASE::GetSchematicTextOffset`, justified as written;
//! - a global label: inside the outline `SCH_GLOBALLABEL::CreateGraphicShape`
//!   draws, which is its extent;
//! - a hierarchical label: beside the flag `SCH_HIERLABEL::CreateGraphicShape`
//!   draws, the two together its extent.
//!
//! The run's length is KiCad's: each glyph advances as far as its entry in
//! `common/newstroke_font.cpp` says (`STROKE_FONT::loadNewStrokeFont`), less
//! the 0.2 gap after the last (`STROKE_FONT::GetTextAsGlyphs`). A glyph outside
//! ASCII is taken as a capital, 20/21 wide. A string is counted as drawn: escapes
//! such as `{slash}` read as `UnescapeString` reads them, and markup braces
//! (`~{}`, `_{}`, `^{}`) left out. The text offset and label size ratios and
//! the default line width are eeschema's defaults, not a project's own
//! schematic setup; italic, outline fonts, unit names other than letters and
//! text variables are taken as written. Free text, sheet fields, sheet pins,
//! labels' own fields and private fields are not measured.

use crate::geometry::{round6, transform_pin, PinTransform};
use crate::parser::SexpNode;
use crate::schematic::{find_lib_symbol, parse_at, symbol_instance, LabelKind, SymbolBounds};

/// The printable ASCII glyphs of `common/newstroke_font.cpp`, from space to
/// `~`, in 21sts of the text size: each one's advance, read off the first two
/// characters of its entry, and how far its strokes reach above and below the
/// baseline.
#[rustfmt::skip]
const ADVANCE: [i8; 95] = [
    16, 10, 16, 21, 20, 24, 26, 10, 14, 14, 16, 26, 10, 26, 10, 22,
    20, 20, 20, 20, 20, 20, 20, 20, 20, 20, 10, 10, 26, 26, 26, 18,
    27, 18, 21, 21, 21, 19, 18, 21, 22, 10, 16, 21, 17, 24, 22, 22,
    21, 22, 21, 20, 16, 22, 18, 24, 20, 18, 20, 14, 14, 14, 12, 16,
    8, 19, 19, 18, 19, 18, 12, 19, 19, 10, 10, 17, 11, 28, 19, 19,
    19, 19, 13, 17, 12, 19, 16, 22, 17, 16, 17, 14, 20, 14, 15,
];
#[rustfmt::skip]
const TOP: [i8; 95] = [
    0, 20, 20, 22, 23, 20, 20, 20, 23, 23, 20, 15, 0, 7, 1, 21,
    20, 20, 20, 20, 21, 20, 20, 20, 20, 20, 12, 12, 13, 10, 13, 20,
    16, 20, 20, 20, 20, 20, 20, 20, 20, 20, 20, 20, 20, 20, 20, 20,
    20, 20, 20, 20, 20, 20, 20, 20, 20, 20, 20, 22, 22, 22, 21, -3,
    21, 13, 20, 13, 20, 13, 20, 13, 20, 20, 20, 20, 20, 13, 13, 13,
    13, 13, 13, 13, 20, 13, 13, 13, 13, 13, 13, 23, 22, 23, 9,
];
#[rustfmt::skip]
const FOOT: [i8; 95] = [
    0, 1, -16, 5, 4, 1, 1, -16, 9, 9, -11, 1, 4, -7, 1, 6,
    1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 4, -1, -4, -1, 1,
    4, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1,
    1, 3, 1, 1, 1, 1, 1, 1, 1, 1, 1, 8, 5, 8, -18, 3,
    -18, 1, 1, 1, 1, 1, 1, 8, 1, 1, 8, 1, 1, 1, 1, 1,
    8, 8, 1, 1, 1, 1, 1, 1, 1, 8, 1, 9, 8, 9, -7,
];

/// A glyph's advance, top and foot in 21sts; outside ASCII, a capital's.
fn glyph(c: char) -> (i8, i8, i8) {
    match (c as usize).checked_sub(0x20) {
        Some(i) if i < ADVANCE.len() => (ADVANCE[i], TOP[i], FOOT[i]),
        _ => (20, 20, 1),
    }
}

/// `DEFAULT_TEXT_OFFSET_RATIO` and `DEFAULT_LABEL_SIZE_RATIO`
/// (`eeschema/default_values.h`).
const TEXT_OFFSET_RATIO: f64 = 0.15;
const LABEL_SIZE_RATIO: f64 = 0.375;

/// `DEFAULT_LINE_WIDTH_MILS`, 6 mils, the pen text without a thickness of its
/// own is drawn with (`SCH_FIELD::Plot`, `GetEffectiveTextPenWidth`).
const DEFAULT_LINE_WIDTH_MM: f64 = 0.1524;

/// Whose text an extent holds.
#[derive(Debug, Clone, PartialEq)]
pub enum TextOwner {
    Label {
        kind: LabelKind,
        net: String,
        uuid: Option<String>,
        /// The label's anchor, its `(at …)`.
        at: (f64, f64),
    },
    Field {
        reference: String,
        unit: u32,
        /// The placed symbol's `uuid`, which tells two unannotated `R?` apart.
        symbol_uuid: Option<String>,
        field: String,
    },
}

/// One drawn string and what it covers.
#[derive(Debug, Clone)]
pub struct TextExtent {
    pub owner: TextOwner,
    /// The string as drawn: unescaped, a multi-unit Reference with its unit
    /// letter, a field showing its name as `Name: value`, markup as written.
    pub text: String,
    /// The string alone.
    pub run: SymbolBounds,
    /// What the sheet shows of the item: the run, and a label's outline or flag.
    pub bounds: SymbolBounds,
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum Justify {
    Start,
    Center,
    End,
}

/// The text attributes an extent needs, read as `parseEDA_TEXT` reads them:
/// no `justify` means centred on both axes.
#[derive(Debug, Clone, Copy)]
struct TextStyle {
    height: f64,
    width: f64,
    /// `EDA_TEXT::GetEffectiveTextPenWidth()`: a thickness of its own, else a
    /// fifth of the width when bold or an eighth, which text boxes, labels'
    /// offsets and outlines use.
    pen: f64,
    /// The pen the text is drawn with: the thickness, else a fifth of the
    /// width when bold, else the default line width.
    stroke: f64,
    horizontal: Justify,
    vertical: Justify,
}

impl TextStyle {
    fn of(node: &SexpNode) -> Self {
        let effects = node.find("effects");
        let font = effects.and_then(|e| e.find("font"));
        let size = font.and_then(|f| f.find("size"));
        let height = size.and_then(|s| s.get_f64(1)).unwrap_or(1.27).abs();
        let width = size.and_then(|s| s.get_f64(2)).unwrap_or(height).abs();
        let bold = font.is_some_and(|f| flag(f, "bold"));
        let thickness = font
            .and_then(|f| f.find_f64("thickness"))
            .filter(|t| *t > 0.0);
        // `ClampTextPenSize`: no more than a quarter of the smaller size.
        let clamp = |pen: f64| pen.min(width.min(height) * 0.25);
        let (pen, stroke) = match thickness {
            Some(t) => (t, t),
            None if bold => (width / 5.0, width / 5.0),
            None => (width / 8.0, DEFAULT_LINE_WIDTH_MM),
        };
        let justify = effects.and_then(|e| e.find("justify"));
        let token = |t: &str| {
            justify.is_some_and(|j| {
                j.children()
                    .unwrap_or(&[])
                    .iter()
                    .any(|c| c.as_str() == Some(t))
            })
        };
        let horizontal = if token("left") {
            Justify::Start
        } else if token("right") {
            Justify::End
        } else {
            Justify::Center
        };
        let vertical = if token("top") {
            Justify::Start
        } else if token("bottom") {
            Justify::End
        } else {
            Justify::Center
        };
        Self {
            height,
            width,
            pen: clamp(pen),
            stroke: clamp(stroke),
            horizontal,
            vertical,
        }
    }

    /// The `textLength` kicad-cli would write for the drawn characters,
    /// padded by the pen they are drawn with.
    fn run_length(&self, drawn: &Drawn) -> f64 {
        drawn.advance() * self.width + 3.0 * self.stroke
    }

    /// The width `EDA_TEXT::GetTextBox` gives the text, padded by `pen`.
    fn box_width(&self, drawn: &Drawn) -> f64 {
        drawn.advance() * self.width + 3.0 * self.pen
    }

    /// The baseline below the text's position, in its own Y-down frame, as
    /// `FONT::getLinePositions` sets it for a vertical justification.
    fn baseline(&self, vertical: Justify) -> f64 {
        let (h, s) = (self.height, self.stroke);
        match vertical {
            Justify::Start => h - 0.052 * s,
            Justify::Center => 0.415 * h - 0.052 * s,
            Justify::End => -0.17 * h - 0.052 * s,
        }
    }

    /// The text drawn at the origin, unturned, in its own Y-down frame: the
    /// run along x from the justified start, and across it the strokes about
    /// the baseline for the vertical justification.
    fn drawn(&self, drawn: &Drawn, vertical: Justify) -> SymbolBounds {
        let length = self.run_length(drawn);
        let start = match self.horizontal {
            Justify::Start => 0.0,
            Justify::Center => -length / 2.0,
            Justify::End => -length,
        };
        let baseline = self.baseline(vertical);
        let (top, foot) = drawn.reach();
        SymbolBounds {
            min_x: start,
            min_y: baseline - top * self.height - self.stroke / 2.0,
            max_x: start + length,
            max_y: baseline + foot * self.height + self.stroke / 2.0,
        }
    }
}

/// How a drawn character sits on the line.
#[derive(Debug, Clone, Copy, PartialEq)]
enum Script {
    Line,
    Sub,
    Super,
}

/// The characters a string draws, and whether it draws an overbar.
struct Drawn {
    chars: Vec<(char, Script)>,
    overbar: bool,
}

impl Drawn {
    /// An opening `~{`, `_{` or `^{` and the `}` that closes it are markup.
    fn of(text: &str) -> Self {
        let source: Vec<char> = text.chars().collect();
        let (mut chars, mut overbar, mut open, mut i) = (Vec::new(), false, Vec::new(), 0);
        while i < source.len() {
            match source[i] {
                c @ ('~' | '_' | '^') if source.get(i + 1) == Some(&'{') => {
                    overbar |= c == '~';
                    open.push(c);
                    i += 1;
                }
                '}' if !open.is_empty() => {
                    open.pop();
                }
                c => {
                    let script = match open.iter().rev().find(|m| **m != '~') {
                        Some('_') => Script::Sub,
                        Some('^') => Script::Super,
                        _ => Script::Line,
                    };
                    chars.push((c, script));
                }
            }
            i += 1;
        }
        Self { chars, overbar }
    }

    /// The string's advance as a multiple of the text width: each glyph's,
    /// at 0.8 in a sub- or superscript, less the gap after the last.
    fn advance(&self) -> f64 {
        if self.chars.is_empty() {
            return 0.0;
        }
        let glyphs: f64 = self
            .chars
            .iter()
            .map(|(c, script)| {
                let scale = if *script == Script::Line { 1.0 } else { 0.8 };
                f64::from(glyph(*c).0) / 21.0 * scale
            })
            .sum();
        glyphs - 0.2
    }

    /// How far the strokes reach above and below the baseline, as fractions
    /// of the height.
    fn reach(&self) -> (f64, f64) {
        let (mut top, mut foot) = (f64::NEG_INFINITY, f64::NEG_INFINITY);
        for (c, script) in self.chars.iter().filter(|(c, _)| *c != ' ') {
            let (_, t, f) = glyph(*c);
            let (t, f) = (f64::from(t) / 21.0, f64::from(f) / 21.0);
            let (t, f) = match script {
                Script::Line => (t, f),
                Script::Super => (0.28 + 0.8 * t, 0.8 * f - 0.28),
                Script::Sub => (0.8 * t - 0.12, 0.12 + 0.8 * f),
            };
            top = top.max(t);
            foot = foot.max(f);
        }
        if !top.is_finite() {
            (top, foot) = (20.0 / 21.0, 1.0 / 21.0);
        }
        if self.overbar {
            top = top.max(1.23);
        }
        (top, foot)
    }
}

/// The string KiCad shows for the escaped one it writes (`UnescapeString`,
/// `common/string_utils.cpp`): `{slash}` is `/`, and markup and variables
/// keep their braces.
fn unescape(text: &str) -> String {
    let source: Vec<char> = text.chars().collect();
    let mut out = String::new();
    let mut i = 0;
    while i < source.len() {
        if source[i] != '{' {
            out.push(source[i]);
            i += 1;
            continue;
        }
        let prev = i.checked_sub(1).map(|p| source[p]);
        let (mut depth, mut j) = (1, i + 1);
        while j < source.len() {
            match source[j] {
                '{' => depth += 1,
                '}' => depth -= 1,
                _ => {}
            }
            if depth == 0 {
                break;
            }
            j += 1;
        }
        let token: String = source[i + 1..j.min(source.len())].iter().collect();
        if j >= source.len() {
            out.push('{');
            out.push_str(&unescape(&token));
        } else if matches!(prev, Some('$' | '~' | '^' | '_')) {
            out.push('{');
            out.push_str(&unescape(&token));
            out.push('}');
        } else {
            match token.as_str() {
                "dblquote" => out.push('"'),
                "quote" => out.push('\''),
                "lt" => out.push('<'),
                "gt" => out.push('>'),
                "backslash" => out.push('\\'),
                "slash" => out.push('/'),
                "bar" => out.push('|'),
                "comma" => out.push(','),
                "colon" => out.push(':'),
                "space" => out.push(' '),
                "dollar" => out.push('$'),
                "tab" => out.push('\t'),
                "return" => out.push('\n'),
                "brace" => out.push('{'),
                _ => {
                    out.push('{');
                    out.push_str(&unescape(&token));
                    out.push('}');
                }
            }
        }
        i = j + 1;
    }
    out
}

/// A `(bold yes)`-style flag, or the bare atom older files write.
fn flag(node: &SexpNode, name: &str) -> bool {
    node.children().unwrap_or(&[]).iter().any(|c| match c {
        SexpNode::Atom(a) => a == name,
        SexpNode::List(_) => {
            c.head() == Some(name) && c.get(1).and_then(|v| v.as_str()).unwrap_or("yes") == "yes"
        }
        SexpNode::Str(_) => false,
    })
}

/// Whether an angle is an odd number of quarter turns.
fn is_vertical(degrees: f64) -> bool {
    ((degrees / 90.0).round() as i64).rem_euclid(2) == 1
}

/// Turn a point about the origin by a multiple of 90°, as `RotatePoint` does
/// in Y-down space: 90° takes (x, y) to (y, −x).
fn turn((x, y): (f64, f64), degrees: f64) -> (f64, f64) {
    match (degrees / 90.0).round().rem_euclid(4.0) as u8 {
        1 => (y, -x),
        2 => (-x, -y),
        3 => (-y, x),
        _ => (x, y),
    }
}

/// Turn a box about the origin by a multiple of 90°.
fn turned(b: SymbolBounds, degrees: f64) -> SymbolBounds {
    SymbolBounds::spanning(
        turn((b.min_x, b.min_y), degrees),
        turn((b.max_x, b.max_y), degrees),
    )
}

/// Carry a point through a placed symbol's rotation and mirror about its
/// origin. `transform_pin` takes library Y-up coordinates, hence the negated y.
fn placed((x, y): (f64, f64), t: PinTransform) -> (f64, f64) {
    let at_origin = PinTransform {
        comp_x: 0.0,
        comp_y: 0.0,
        ..t
    };
    transform_pin(x, -y, at_origin)
}

/// Move a box by (dx, dy) and widen it by `grow` on every side.
fn moved(b: SymbolBounds, dx: f64, dy: f64, grow: f64) -> SymbolBounds {
    SymbolBounds {
        min_x: b.min_x + dx - grow,
        min_y: b.min_y + dy - grow,
        max_x: b.max_x + dx + grow,
        max_y: b.max_y + dy + grow,
    }
}

/// To the six decimals KiCad writes, as every reported coordinate is (#747).
fn rounded(b: SymbolBounds) -> SymbolBounds {
    SymbolBounds {
        min_x: round6(b.min_x),
        min_y: round6(b.min_y),
        max_x: round6(b.max_x),
        max_y: round6(b.max_y),
    }
}

/// eeschema's label spin styles, in the order its shape tables index them.
#[derive(Debug, Clone, Copy, PartialEq)]
enum Spin {
    Left,
    Up,
    Right,
    Bottom,
}

/// Every label's and every visible field's extent on the sheet: the labels
/// first, then each placed symbol's fields in file order.
pub fn text_extents(tree: &SexpNode) -> Vec<TextExtent> {
    let mut extents = Vec::new();
    for (tag, kind) in [
        ("label", LabelKind::NetLabel),
        ("global_label", LabelKind::GlobalLabel),
        ("hierarchical_label", LabelKind::HierarchicalLabel),
    ] {
        for node in tree.find_all(tag) {
            if let Some(e) = label_extent(node, kind) {
                extents.push(TextExtent {
                    run: rounded(e.run),
                    bounds: rounded(e.bounds),
                    ..e
                });
            }
        }
    }
    let lib_symbols = tree
        .find("lib_symbols")
        .map(|node| node.find_all("symbol"))
        .unwrap_or_default();
    for node in tree.find_all("symbol") {
        let Some(instance) = symbol_instance(node) else {
            continue;
        };
        let units = find_lib_symbol(&lib_symbols, &instance).map_or(1, unit_count);
        let t = instance.pin_transform();
        for property in node.find_all("property") {
            // eeschema draws a private field only in the symbol editor
            // (`SCH_PAINTER::draw`), and writes it `(property private …)`.
            if property
                .get(1)
                .is_some_and(|n| matches!(n, SexpNode::Atom(a) if a == "private"))
            {
                continue;
            }
            let (Some(name), Some(value)) = (
                property.get(1).and_then(|n| n.as_str()),
                property.get(2).and_then(|v| v.as_str()),
            ) else {
                continue;
            };
            let hidden =
                flag(property, "hide") || property.find("effects").is_some_and(|e| flag(e, "hide"));
            let shows_name = flag(property, "show_name");
            let Some((x, y, angle)) = parse_at(property) else {
                continue;
            };
            if hidden || (value.is_empty() && !shows_name) {
                continue;
            }
            let mut text = unescape(value);
            if name == "Reference" && units > 1 {
                text.push_str(&unit_letters(instance.unit));
            }
            if shows_name {
                text = format!("{name}: {text}");
            }
            let run = rounded(field_run(
                &TextStyle::of(property),
                &text,
                (x, y, angle),
                &t,
            ));
            extents.push(TextExtent {
                owner: TextOwner::Field {
                    reference: instance.reference.clone(),
                    unit: instance.unit,
                    symbol_uuid: instance.uuid.clone(),
                    field: name.to_string(),
                },
                text,
                run,
                bounds: run,
            });
        }
    }
    extents
}

/// A field's run as `SCH_FIELD::Plot` draws it: centred on the centre of the
/// box `EDA_TEXT::GetTextBox` lays out about the field's position, carried
/// through the field's own angle and its symbol's rotation and mirror as
/// `SCH_FIELD::GetBoundingBox` carries it, and drawn upright there.
fn field_run(
    style: &TextStyle,
    text: &str,
    (x, y, angle): (f64, f64, f64),
    t: &PinTransform,
) -> SymbolBounds {
    let drawn = Drawn::of(text);
    // The box: the width and its justification, and across it the height
    // KiCad gives the string, the cell and three pens, with its 0.17 stroke
    // fudge and a sixth more for an overbar.
    let extent_y = style.height + 3.0 * style.pen;
    let overbar = if drawn.overbar { extent_y / 12.0 } else { 0.0 };
    let half_width = style.box_width(&drawn) / 2.0;
    let centre = (
        match style.horizontal {
            Justify::Start => half_width,
            Justify::Center => 0.0,
            Justify::End => -half_width,
        },
        match style.vertical {
            Justify::Start => 0.415 * extent_y + overbar,
            Justify::Center => 0.0,
            Justify::End => -0.415 * extent_y - overbar,
        },
    );
    let (cx, cy) = placed(turn(centre, angle), *t);
    let centred = TextStyle {
        horizontal: Justify::Center,
        ..*style
    };
    let upright = centred.drawn(&drawn, Justify::Center);
    let upright = if is_vertical(angle) != is_vertical(t.rotation_deg) {
        turned(upright, 90.0)
    } else {
        upright
    };
    moved(upright, x + cx, y + cy, 0.0)
}

fn label_extent(node: &SexpNode, kind: LabelKind) -> Option<TextExtent> {
    let net = node.get(1)?.as_str()?.to_string();
    let (x, y, angle) = parse_at(node)?;
    let text = unescape(&net);
    let drawn = Drawn::of(&text);
    let style = TextStyle::of(node);
    // The parser keeps the text upright, 180° reading as 0° and 270° as 90°,
    // and the spin style follows from that angle and the justification.
    let vertical = is_vertical(angle);
    let spin = match (vertical, style.horizontal) {
        (false, Justify::End) => Spin::Left,
        (false, _) => Spin::Right,
        (true, Justify::End) => Spin::Bottom,
        (true, _) => Spin::Up,
    };
    let text_angle = if vertical { 90.0 } else { 0.0 };
    let h = style.height;
    let run_at = |dx: f64, dy: f64| {
        moved(
            turned(style.drawn(&drawn, style.vertical), text_angle),
            x + dx,
            y + dy,
            0.0,
        )
    };
    let (run, bounds) = match kind {
        LabelKind::GlobalLabel => {
            // `SCH_GLOBALLABEL::GetSchematicTextOffset`.
            let margin = LABEL_SIZE_RATIO * h;
            let shape = node.find_str("shape").unwrap_or("input");
            let pointed = matches!(shape, "input" | "bidirectional" | "tri_state");
            let horiz = margin + if pointed { h * 3.0 / 4.0 } else { 0.0 };
            let vert = h * 0.0715;
            let (dx, dy) = match spin {
                Spin::Left => (-horiz, vert),
                Spin::Up => (vert, -horiz),
                Spin::Right => (horiz, vert),
                Spin::Bottom => (vert, horiz),
            };
            // `SCH_GLOBALLABEL::CreateGraphicShape`: drawn leftward from the
            // anchor around the text box, one half-size longer for a point at
            // either end, then turned to the spin style, and stroked.
            let half = h / 2.0 + margin;
            let points = match shape {
                "input" | "output" => 1.0,
                "bidirectional" | "tri_state" => 2.0,
                _ => 0.0,
            };
            let reach = style.box_width(&drawn) + 2.0 * margin + style.pen + points * half;
            let across = half + style.pen;
            let outline = SymbolBounds::spanning((-reach, -across), (0.0, across));
            let quarter = match spin {
                Spin::Left => 0.0,
                Spin::Up => 270.0,
                Spin::Right => 180.0,
                Spin::Bottom => 90.0,
            };
            let outline = moved(turned(outline, quarter), x, y, style.stroke / 2.0);
            (run_at(dx, dy), outline)
        }
        LabelKind::HierarchicalLabel => {
            // `SCH_HIERLABEL::GetSchematicTextOffset`, and the flag's corners
            // from its shape tables, which span the same box for every shape.
            let dist = TEXT_OFFSET_RATIO * h + style.width;
            let half = h / 2.0;
            let (dx, dy, corner, opposite) = match spin {
                Spin::Left => (-dist, 0.0, (-2.0 * half, -half), (0.0, half)),
                Spin::Up => (0.0, -dist, (-half, -2.0 * half), (half, 0.0)),
                Spin::Right => (dist, 0.0, (0.0, -half), (2.0 * half, half)),
                Spin::Bottom => (0.0, dist, (-half, 0.0), (half, 2.0 * half)),
            };
            let run = run_at(dx, dy);
            let flag = moved(
                SymbolBounds::spanning(corner, opposite),
                x,
                y,
                style.stroke / 2.0,
            );
            let mut both = run;
            both.include(flag.min_x, flag.min_y);
            both.include(flag.max_x, flag.max_y);
            (run, both)
        }
        _ => {
            // `SCH_LABEL_BASE::GetSchematicTextOffset`: off the wire by the
            // text offset and a pen width, across the text's run.
            let dist = TEXT_OFFSET_RATIO * h + style.pen;
            let (dx, dy) = if vertical { (-dist, 0.0) } else { (0.0, -dist) };
            let run = run_at(dx, dy);
            (run, run)
        }
    };
    Some(TextExtent {
        owner: TextOwner::Label {
            kind,
            net,
            uuid: node.find_str("uuid").map(String::from),
            at: (x, y),
        },
        text,
        run,
        bounds,
    })
}

/// How many units a library symbol has: its highest `NAME_U_S` unit number.
fn unit_count(symbol: &SexpNode) -> u32 {
    let name = symbol.get(1).and_then(|n| n.as_str()).unwrap_or("");
    let base = name.rsplit(':').next().unwrap_or(name);
    symbol
        .find_all("symbol")
        .iter()
        .filter_map(|unit| {
            let rest = unit
                .get(1)?
                .as_str()?
                .strip_prefix(base)?
                .strip_prefix('_')?;
            rest.split('_').next()?.parse::<u32>().ok()
        })
        .max()
        .unwrap_or(1)
        .max(1)
}

/// `LIB_SYMBOL::LetterSubReference` with eeschema's default first id: 1 is
/// `A`, 26 is `Z`, 27 is `AA`.
fn unit_letters(unit: u32) -> String {
    let mut n = unit.max(1);
    let mut letters = Vec::new();
    while n > 0 {
        n -= 1;
        letters.push(char::from(b'A' + (n % 26) as u8));
        n /= 26;
    }
    letters.iter().rev().collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parser::parse_sexp;

    #[test]
    fn markup_braces_are_not_drawn() {
        let count = |t: &str| Drawn::of(t).chars.len();
        assert_eq!(count("~{RESET}"), 5);
        assert_eq!(count("V_{OUT}"), 4);
        assert_eq!(count("x^{2}"), 2);
        // A tilde or brace outside markup is drawn as written.
        assert_eq!(count("A~B"), 3);
        assert_eq!(count("a}b"), 3);
        assert!(Drawn::of("~{CS}").overbar);
        assert!(!Drawn::of("V_{OUT}").overbar);
        // V, then O, U and T at 0.8, less the gap after the last.
        let advance = Drawn::of("V_{OUT}").advance();
        assert!((advance - ((18.0 + 0.8 * (22.0 + 22.0 + 16.0)) / 21.0 - 0.2)).abs() < 1e-12);
    }

    #[test]
    fn escapes_read_as_kicad_reads_them() {
        assert_eq!(unescape("SDA{slash}SCL"), "SDA/SCL");
        assert_eq!(unescape("A{lt}B{gt}{dblquote}"), "A<B>\"");
        assert_eq!(unescape("~{RESET}"), "~{RESET}");
        assert_eq!(unescape("${REF}"), "${REF}");
        assert_eq!(unescape("{unknown}"), "{unknown}");
        assert_eq!(unescape("open{"), "open{");
    }

    #[test]
    fn the_band_reaches_as_far_as_the_strings_glyphs() {
        let reach = |t: &str| Drawn::of(t).reach();
        assert_eq!(reach("R10"), (20.0 / 21.0, 1.0 / 21.0));
        assert_eq!(reach("NRST_CH347"), (21.0 / 21.0, 3.0 / 21.0));
        assert_eq!(reach("SDA/SCL"), (21.0 / 21.0, 6.0 / 21.0));
        assert_eq!(reach("Conn_01x02 (J1)"), (23.0 / 21.0, 9.0 / 21.0));
        assert_eq!(reach("~{RESET}").0, 1.23);
        assert_eq!(reach("nc"), (13.0 / 21.0, 1.0 / 21.0));
        // `STROKE_FONT::GetTextAsGlyphs`: 0.8 high, 0.28 up or 0.12 down.
        let (top, _) = reach("x^{N}");
        assert!((top - (0.28 + 0.8 * 20.0 / 21.0)).abs() < 1e-12, "{top}");
        let (_, foot) = reach("V_{N}");
        assert!((foot - (0.12 + 0.8 / 21.0)).abs() < 1e-12, "{foot}");
    }

    #[test]
    fn units_are_lettered_as_eeschema_letters_them() {
        assert_eq!(unit_letters(1), "A");
        assert_eq!(unit_letters(2), "B");
        assert_eq!(unit_letters(26), "Z");
        assert_eq!(unit_letters(27), "AA");
    }

    fn field_extents(properties: &str) -> Vec<TextExtent> {
        let sheet = format!(
            "(kicad_sch (lib_symbols) (symbol (lib_id \"Device:R\") (at 100 50 0) (unit 1) \
             (uuid \"u\") {properties}))"
        );
        text_extents(&parse_sexp(&sheet).unwrap())
    }

    #[test]
    fn hidden_private_and_empty_fields_draw_nothing_and_a_shown_name_is_drawn() {
        let texts = field_extents(
            "(property \"Reference\" \"R1\" (at 102 50 0) (effects (font (size 1.27 1.27)))) \
             (property \"Value\" \"\" (at 100 52 0) (effects (font (size 1.27 1.27)))) \
             (property \"Footprint\" \"R_0603\" (at 100 54 0) (hide yes) (effects (font (size 1.27 1.27)))) \
             (property \"Datasheet\" \"x\" (at 100 56 0) (effects (font (size 1.27 1.27)) hide)) \
             (property \"MPN\" \"RC0603\" (at 100 58 0) (show_name yes) (effects (font (size 1.27 1.27)))) \
             (property \"Note\" \"\" (at 100 60 0) (show_name yes) (effects (font (size 1.27 1.27)))) \
             (property private \"Secret\" \"S\" (at 100 62 0) (effects (font (size 1.27 1.27))))",
        )
        .into_iter()
        .map(|e| e.text)
        .collect::<Vec<_>>();
        assert_eq!(texts, ["R1", "MPN: RC0603", "Note: "]);
    }

    #[test]
    fn a_field_without_a_position_is_not_measured() {
        assert!(field_extents("(property \"Reference\" \"R1\")").is_empty());
    }

    /// A top-justified field hangs below its position: drawn centred on the
    /// centre of its text box, 0.415 of the box's height (the cell and three
    /// pens) below it, and its baseline 0.415 of the height below that.
    #[test]
    fn a_top_justified_field_is_drawn_on_its_boxs_centre() {
        let extent = |justify: &str| {
            field_extents(&format!(
                "(property \"Reference\" \"R1\" (at 100 50 0) \
                 (effects (font (size 1.27 1.27)){justify}))"
            ))[0]
                .run
        };
        let (centred, top) = (extent(""), extent(" (justify top)"));
        let shift = 0.415 * (1.27 + 3.0 * 1.27 / 8.0);
        assert!(
            ((top.min_y - centred.min_y) - shift).abs() < 1e-5,
            "{top:?} {centred:?}"
        );
        assert!(((top.max_y - centred.max_y) - shift).abs() < 1e-5);
        assert_eq!((top.min_x, top.max_x), (centred.min_x, centred.max_x));
    }

    /// A left-justified field's box turns with its symbol: at 180° it runs
    /// left from its position as far as it ran right at 0°, still upright.
    #[test]
    fn a_justified_fields_box_turns_with_its_symbol() {
        let run = |rotation: u32| {
            let sheet = format!(
                "(kicad_sch (lib_symbols) (symbol (lib_id \"Device:R\") (at 100 50 {rotation}) \
                 (unit 1) (uuid \"u\") (property \"Reference\" \"R1\" (at 100 50 0) \
                 (effects (font (size 1.27 1.27)) (justify left)))))"
            );
            text_extents(&parse_sexp(&sheet).unwrap())[0].run
        };
        let (level, turned) = (run(0), run(180));
        assert!(
            level.min_x > 100.0 && turned.max_x < 100.0,
            "{level:?} {turned:?}"
        );
        assert!(((100.0 - turned.max_x) - (level.min_x - 100.0)).abs() < 1e-6);
        assert!(((turned.max_x - turned.min_x) - (level.max_x - level.min_x)).abs() < 1e-6);
        assert_eq!((turned.min_y, turned.max_y), (level.min_y, level.max_y));
    }

    #[test]
    fn bold_text_draws_a_wider_pen() {
        let length = |font: &str| {
            let sheet = format!(
                "(kicad_sch (label \"NET\" (at 0 0 0) (effects (font (size 1.27 1.27){font}) (justify left bottom))))"
            );
            let e = &text_extents(&parse_sexp(&sheet).unwrap())[0];
            e.run.max_x - e.run.min_x
        };
        // Three strokes of 1.27/5 against three of the 0.1524 default line.
        let widened = length(" (bold yes)") - length("");
        assert!(
            (widened - 3.0 * (1.27 / 5.0 - 0.1524)).abs() < 1e-6,
            "{widened}"
        );
    }
}
