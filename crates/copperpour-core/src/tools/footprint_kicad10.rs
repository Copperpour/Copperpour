//! What `create_footprint` needs to write a footprint as KiCad 10.0.8's own
//! footprint writer does (#711): its number formats, its order of graphics and
//! pads, and its item syntax. The text goes through `copperpour_sexp::prettify`,
//! KiCad's layout pass, so `kicad-cli fp upgrade --force` gives the file back
//! unchanged, UUIDs and the generator aside.

use std::cmp::Ordering;

/// The footprint format KiCad 10.0.8 writes (`SEXPR_BOARD_FILE_VERSION`).
pub(super) const FOOTPRINT_VERSION: u32 = 20260206;

/// A length in mm as KiCad writes one (`EDA_UNIT_UTILS::FormatInternalUnits`,
/// `common/eda_units.cpp`): KiCad holds whole nanometres, so the value is
/// rounded to one, then written without trailing zeros. Its `{:.10g}` and its
/// fixed format for values up to 0.0001 mm both give that exact decimal.
pub(super) fn mm(value: f64) -> String {
    let nm = nanometres(value);
    let sign = if nm < 0 { "-" } else { "" };
    let nm = nm.unsigned_abs();
    let (whole, fraction) = (nm / 1_000_000, nm % 1_000_000);
    if fraction == 0 {
        return format!("{sign}{whole}");
    }
    let fraction = format!("{fraction:06}");
    format!("{sign}{whole}.{}", fraction.trim_end_matches('0'))
}

/// A length in KiCad's nanometres, as its parser rounds one (`KiROUND`).
pub(super) fn nanometres(value: f64) -> i64 {
    (value * 1_000_000.0).round() as i64
}

/// A double as KiCad writes an angle or a model transform: `{:.10g}`
/// (`EDA_UNIT_UTILS::FormatAngle`, `FormatDouble2Str`).
pub(super) fn g10(value: f64) -> String {
    if value == 0.0 {
        return "0".to_string();
    }
    // The decimal exponent of the value once rounded to 10 significant digits.
    let exponent: i32 = format!("{value:.9e}")
        .rsplit('e')
        .next()
        .and_then(|exponent| exponent.parse().ok())
        .unwrap_or(0);
    if !(-5..10).contains(&exponent) {
        return format!("{value:.9e}");
    }
    let decimals = (9 - exponent).max(0) as usize;
    let text = format!("{value:.decimals$}");
    if text.contains('.') {
        text.trim_end_matches('0').trim_end_matches('.').to_string()
    } else {
        text
    }
}

/// KiCad's natural order of pad numbers (`StrNumCmp`, `common/string_utils.cpp`,
/// case-sensitive as `FOOTPRINT::cmp_pads` calls it): runs of digits compare
/// as numbers. Ported with its quirk: after two equal numbers, the characters
/// that follow are compared and then skipped.
pub(super) fn str_num_cmp(a: &str, b: &str) -> Ordering {
    let (a, b): (Vec<char>, Vec<char>) = (a.chars().collect(), b.chars().collect());
    let (mut i, mut j) = (0, 0);
    while i < a.len() && j < b.len() {
        let (mut c1, mut c2) = (a[i], b[j]);
        if c1.is_ascii_digit() && c2.is_ascii_digit() {
            let number = |text: &[char], at: &mut usize| {
                let mut value: u64 = 0;
                while *at < text.len() && text[*at].is_ascii_digit() {
                    value = value * 10 + u64::from(text[*at] as u8 - b'0');
                    *at += 1;
                }
                value
            };
            let (n1, n2) = (number(&a, &mut i), number(&b, &mut j));
            if n1 != n2 {
                return n1.cmp(&n2);
            }
            c1 = a.get(i).copied().unwrap_or('\0');
            c2 = b.get(j).copied().unwrap_or('\0');
        }
        if c1 != c2 {
            return c1.cmp(&c2);
        }
        i = (i + 1).min(a.len());
        j = (j + 1).min(b.len());
    }
    // Equal so far: the one with characters left is the greater.
    (i < a.len()).cmp(&(j < b.len()))
}

/// One footprint graphic `create_footprint` draws.
#[derive(Debug, Clone, PartialEq)]
pub(super) enum Shape {
    Line { start: (f64, f64), end: (f64, f64) },
    Rect { start: (f64, f64), end: (f64, f64) },
    Circle { center: (f64, f64), end: (f64, f64) },
    Poly { points: Vec<(f64, f64)> },
}

#[derive(Debug, Clone, PartialEq)]
pub(super) struct Graphic {
    pub shape: Shape,
    pub layer: &'static str,
    pub width: f64,
    /// Filled; KiCad writes `(fill …)` for every shape but a line.
    pub filled: bool,
}

impl Graphic {
    /// KiCad's `SHAPE_T` value (`include/eda_shape.h`), its sort's third key.
    fn shape_order(&self) -> u8 {
        match self.shape {
            Shape::Line { .. } => 0,
            Shape::Rect { .. } => 1,
            Shape::Circle { .. } => 3,
            Shape::Poly { .. } => 4,
        }
    }

    /// The layer's `PCB_LAYER_ID` (`include/layer_ids.h`).
    fn layer_order(&self) -> u8 {
        match self.layer {
            "F.SilkS" => 5,
            "F.CrtYd" => 31,
            "F.Fab" => 35,
            other => unreachable!("create_footprint draws no graphic on {other}"),
        }
    }

    /// The order `FOOTPRINT::cmp_drawings` (`pcbnew/footprint.cpp`) gives
    /// footprint graphics, in which KiCad writes them: layer, shape, then a
    /// shape's start and end points, or a polygon's size and points, then the
    /// stroke width; points compare by x, then y, in nanometres.
    pub fn kicad_order(&self, other: &Self) -> Ordering {
        let point = |p: (f64, f64)| (nanometres(p.0), nanometres(p.1));
        let ends = |graphic: &Self| match &graphic.shape {
            Shape::Line { start, end } | Shape::Rect { start, end } => {
                vec![point(*start), point(*end)]
            }
            Shape::Circle { center, end } => vec![point(*center), point(*end)],
            Shape::Poly { .. } => Vec::new(),
        };
        let vertices = |graphic: &Self| match &graphic.shape {
            Shape::Poly { points } => points.iter().map(|p| point(*p)).collect(),
            _ => Vec::new(),
        };
        self.layer_order()
            .cmp(&other.layer_order())
            .then(self.shape_order().cmp(&other.shape_order()))
            .then_with(|| ends(self).cmp(&ends(other)))
            .then_with(|| {
                let (a, b): (Vec<_>, Vec<_>) = (vertices(self), vertices(other));
                a.len().cmp(&b.len()).then_with(|| a.cmp(&b))
            })
            .then(nanometres(self.width).cmp(&nanometres(other.width)))
    }

    /// The graphic as KiCad's writer prints it, before the layout pass.
    pub fn sexp(&self, uuid: &str) -> String {
        let xy = |p: (f64, f64)| format!("{} {}", mm(p.0), mm(p.1));
        let geometry = match &self.shape {
            Shape::Line { start, end } => {
                format!("(fp_line (start {}) (end {})", xy(*start), xy(*end))
            }
            Shape::Rect { start, end } => {
                format!("(fp_rect (start {}) (end {})", xy(*start), xy(*end))
            }
            Shape::Circle { center, end } => {
                format!("(fp_circle (center {}) (end {})", xy(*center), xy(*end))
            }
            Shape::Poly { points } => {
                let points: Vec<String> =
                    points.iter().map(|p| format!("(xy {})", xy(*p))).collect();
                format!("(fp_poly (pts {})", points.join(" "))
            }
        };
        let fill = match self.shape {
            Shape::Line { .. } => String::new(),
            _ => format!(" (fill {})", if self.filled { "yes" } else { "no" }),
        };
        format!(
            "{geometry} (stroke (width {}) (type solid)){fill} (layer \"{}\") (uuid \"{uuid}\"))",
            mm(self.width),
            self.layer
        )
    }
}

/// A new item UUID, as KiCad gives a new item.
pub(super) fn new_uuid() -> String {
    uuid::Uuid::new_v4().to_string()
}

/// A footprint field as KiCad writes it (`PCB_IO_KICAD_SEXPR::format( const
/// PCB_FIELD* )`): position with its angle, layer, visibility, then the font.
pub(super) fn field_sexp(
    name: &str,
    text: &str,
    at: (f64, f64),
    layer: &str,
    hidden: bool,
    font: &str,
) -> String {
    format!(
        "(property {} {} (at {} {} 0) (layer \"{layer}\"){} (uuid \"{}\") (effects (font {font})))",
        super::footprint_metadata::quote_string(name),
        super::footprint_metadata::quote_string(text),
        mm(at.0),
        mm(at.1),
        if hidden { " (hide yes)" } else { "" },
        new_uuid()
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `text` with every run of whitespace outside a string turned into one
    /// space: KiCad's compact print, before its layout pass.
    fn compacted(text: &str) -> String {
        let (mut out, mut in_quote, mut escaped, mut space) = (String::new(), false, false, false);
        for c in text.chars() {
            if in_quote {
                out.push(c);
                if escaped {
                    escaped = false;
                } else if c == '\\' {
                    escaped = true;
                } else if c == '"' {
                    in_quote = false;
                }
            } else if c.is_whitespace() {
                space = true;
            } else {
                if space && !out.is_empty() {
                    out.push(' ');
                }
                space = false;
                in_quote = c == '"';
                out.push(c);
            }
        }
        out
    }

    /// `prettify` lays out KiCad-written footprints exactly as KiCad did:
    /// custom pads with long point lists, a THT connector, an SMD package.
    #[test]
    fn prettify_gives_back_kicad_written_footprints() {
        for fixture in [
            include_str!("../../tests/fixtures/pad_primitives_vqfn20_kicad10.kicad_mod"),
            include_str!("../../tests/fixtures/pad_primitives_wuerth_kicad10.kicad_mod"),
            include_str!("../../tests/fixtures/rename_footprint_source_kicad10.kicad_mod"),
            include_str!("../../tests/fixtures/pad_layers_source_kicad10.kicad_mod"),
        ] {
            let kicad = fixture.replace("\r\n", "\n");
            let name = kicad.lines().next().unwrap();
            assert_eq!(
                copperpour_sexp::prettify(&compacted(&kicad)),
                kicad,
                "{name}"
            );
        }
    }

    #[test]
    fn lengths_are_written_in_whole_nanometres_as_kicad_writes_them() {
        for (value, written) in [
            (0.0, "0"),
            (1.0, "1"),
            (-1.85, "-1.85"),
            (5.715 + 0.25, "5.965"),
            (0.1 + 0.2, "0.3"),
            (0.00005, "0.00005"),
            (-0.0000004, "0"),
            (12.62 / 2.0 + 0.11, "6.42"),
            (2.54 * 7.0, "17.78"),
        ] {
            assert_eq!(mm(value), written, "{value}");
        }
    }

    #[test]
    fn angles_and_transforms_use_ten_significant_digits() {
        for (value, written) in [
            (0.0, "0"),
            (90.0, "90"),
            (-90.0, "-90"),
            (45.5, "45.5"),
            (1.0 / 3.0, "0.3333333333"),
            (123.456789012345, "123.456789"),
            (0.1 + 0.2, "0.3"),
        ] {
            assert_eq!(g10(value), written, "{value}");
        }
    }

    #[test]
    fn pad_numbers_sort_as_kicad_sorts_them() {
        let mut numbers = vec!["10", "2", "", "A10", "A2", "1", "B1", "a1", "MP", "1a"];
        numbers.sort_by(|a, b| str_num_cmp(a, b));
        assert_eq!(
            numbers,
            vec!["", "1", "1a", "2", "10", "A2", "A10", "B1", "MP", "a1"]
        );
        assert_eq!(str_num_cmp("12", "12"), Ordering::Equal);
    }

    #[test]
    fn graphics_sort_by_layer_shape_points_and_width() {
        let line = |x: f64, layer| Graphic {
            shape: Shape::Line {
                start: (x, 0.0),
                end: (x, 1.0),
            },
            layer,
            width: 0.12,
            filled: false,
        };
        let rect = Graphic {
            shape: Shape::Rect {
                start: (-1.0, -1.0),
                end: (1.0, 1.0),
            },
            layer: "F.SilkS",
            width: 0.12,
            filled: false,
        };
        let mut graphics = [
            line(0.0, "F.Fab"),
            rect.clone(),
            line(1.0, "F.SilkS"),
            line(-1.0, "F.SilkS"),
            line(0.0, "F.CrtYd"),
        ];
        graphics.sort_by(|a, b| a.kicad_order(b));
        assert_eq!(
            graphics.iter().map(|g| g.layer).collect::<Vec<_>>(),
            ["F.SilkS", "F.SilkS", "F.SilkS", "F.CrtYd", "F.Fab"]
        );
        assert_eq!(graphics[0], line(-1.0, "F.SilkS"));
        assert_eq!(graphics[1], line(1.0, "F.SilkS"));
        assert_eq!(graphics[2], rect, "a line sorts before a rectangle");
    }
}
