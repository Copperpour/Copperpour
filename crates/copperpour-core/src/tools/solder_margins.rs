//! A pad's or a footprint's own solder mask and paste margins, as KiCad
//! writes and reads them (`PCB_IO_KICAD_SEXPR::format` for `PAD` and
//! `FOOTPRINT`, and the matching cases of `PCB_IO_KICAD_SEXPR_PARSER`).

use copperpour_sexp::writer::{apply_edits, find_direct_child_blocks, SexpEdit};
use copperpour_sexp::{parse_sexp, SexpNode};
use serde_json::json;

/// The last file version in which KiCad reads a margin of 0 as "no override":
/// `In pre-9.0 files "0" meant inherit` in the pad and footprint parsers.
pub(super) const ZERO_IS_INHERITED: u32 = 20240201;

/// The largest board unit KiCad keeps; it clamps a larger value
/// (`INT_LIMIT` in `pcb_io_kicad_sexpr_parser.cpp`).
const BOARD_UNIT_LIMIT: i64 = i32::MAX as i64 - 10;

/// The three margins, in the order KiCad writes them.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) enum Margin {
    Mask,
    Paste,
    PasteRatio,
}

impl Margin {
    pub(super) const ALL: [Self; 3] = [Self::Mask, Self::Paste, Self::PasteRatio];

    /// The argument name, which is also KiCad's token.
    pub(super) fn name(self) -> &'static str {
        match self {
            Self::Mask => "solder_mask_margin",
            Self::Paste => "solder_paste_margin",
            Self::PasteRatio => "solder_paste_margin_ratio",
        }
    }

    /// The tokens KiCad reads as this margin on a footprint: the ratio also
    /// had the legacy `solder_paste_ratio`. A pad knows only the current one.
    pub(super) fn footprint_tokens(self) -> &'static [&'static str] {
        match self {
            Self::PasteRatio => &["solder_paste_margin_ratio", "solder_paste_ratio"],
            Self::Mask => &["solder_mask_margin"],
            Self::Paste => &["solder_paste_margin"],
        }
    }

    fn is_ratio(self) -> bool {
        self == Self::PasteRatio
    }

    /// The argument's schema; `owner` is "pad" or "footprint".
    pub(super) fn schema(self, owner: &str) -> serde_json::Value {
        let fallback = if owner == "pad" {
            "the footprint's value, or the board's"
        } else {
            "the board's value"
        };
        let what = match self {
            Self::Mask => format!(
                "The {owner}'s own solder mask margin in mm: the mask opening grows by it on \
                 each side, or shrinks for a negative value"
            ),
            Self::Paste => format!(
                "The {owner}'s own solder paste margin in mm: the stencil opening grows by it on \
                 each side, or shrinks for a negative value"
            ),
            Self::PasteRatio => format!(
                "The {owner}'s own solder paste margin as a fraction of the pad size, as KiCad \
                 stores it (-0.1 for -10 %), added to the paste margin"
            ),
        };
        json!({
            "description": format!(
                "{what} (optional; omit it to leave it as it is). 0 is an override like any \
                 other value; 'inherited' removes the override, so {fallback} applies. A file \
                 older than format {ZERO_IS_INHERITED} cannot hold an override of 0, since \
                 KiCad reads 0 there as inherited."
            ),
            "oneOf": [
                {"type": "number"},
                {"type": "string", "enum": ["inherited"]}
            ]
        })
    }
}

/// What a call sets one margin to.
#[derive(Clone, PartialEq, Debug)]
pub(super) enum Setting {
    Inherited,
    /// The value as written, and as reported (mm, or a fraction).
    Set {
        text: String,
        value: f64,
    },
}

impl Setting {
    /// The child KiCad would write, or `None` for no override.
    pub(super) fn child(&self, margin: Margin) -> Option<String> {
        match self {
            Self::Inherited => None,
            Self::Set { text, .. } => Some(format!("({} {text})", margin.name())),
        }
    }

    fn is_zero(&self) -> bool {
        matches!(self, Self::Set { value, .. } if *value == 0.0)
    }
}

/// An argument refused before anything is read or written.
pub(super) struct Refusal {
    pub(super) field: &'static str,
    pub(super) reason: String,
}

/// The margins a call sets, in KiCad's order. Omitted ones are left alone.
pub(super) fn requested(args: &serde_json::Value) -> Result<Vec<(Margin, Setting)>, Refusal> {
    let mut requested = Vec::new();
    for margin in Margin::ALL {
        let Some(value) = args.get(margin.name()) else {
            continue;
        };
        let refusal = |reason: String| Refusal {
            field: margin.name(),
            reason,
        };
        let setting = match value {
            serde_json::Value::String(text) if text == "inherited" => Setting::Inherited,
            serde_json::Value::Number(number) => {
                let Some(value) = number.as_f64().filter(|value| value.is_finite()) else {
                    return Err(refusal("must be a finite number".to_string()));
                };
                if margin.is_ratio() {
                    let value = if value == 0.0 { 0.0 } else { value };
                    Setting::Set {
                        text: value.to_string(),
                        value,
                    }
                } else {
                    let nm = (value * 1e6).round();
                    if nm.abs() > BOARD_UNIT_LIMIT as f64 {
                        return Err(refusal(format!(
                            "{value} mm is beyond the largest length KiCad keeps, about 2147 mm"
                        )));
                    }
                    let nm = nm as i64;
                    Setting::Set {
                        text: super::pcb_board_outline::mm_text(nm),
                        value: nm as f64 / 1e6,
                    }
                }
            }
            _ => {
                return Err(refusal(
                    "must be a number, or 'inherited' to remove the override".to_string(),
                ))
            }
        };
        requested.push((margin, setting));
    }
    Ok(requested)
}

/// Refuse an override of 0 in a file KiCad reads 0 in as "inherited".
pub(super) fn refuse_zero_in_older_file(
    file_version: u32,
    requested: &[(Margin, Setting)],
) -> Result<(), Refusal> {
    if file_version > ZERO_IS_INHERITED {
        return Ok(());
    }
    match requested.iter().find(|(_, setting)| setting.is_zero()) {
        Some((margin, _)) => Err(Refusal {
            field: margin.name(),
            reason: format!(
                "the file is in KiCad's format {file_version}, older than \
                 {next}, and KiCad reads a margin of 0 there as inherited, so 0 cannot be an \
                 override in it; give a value other than 0, or bring the library to the current \
                 format first with upgrade_library",
                next = ZERO_IS_INHERITED + 1
            ),
        }),
        None => Ok(()),
    }
}

/// A file's `(version N)`, or 0 without one.
pub(super) fn file_version(root: &SexpNode) -> u32 {
    root.find("version")
        .and_then(|version| version.get(1))
        .and_then(SexpNode::as_str)
        .and_then(|version| version.parse().ok())
        .unwrap_or(0)
}

/// A margin as KiCad reads it from `node` (a pad or a footprint): the last of
/// its tokens, "inherited" without one, and 0 as "inherited" in a file up to
/// [`ZERO_IS_INHERITED`].
pub(super) fn read(
    node: &SexpNode,
    margin: Margin,
    tokens: &[&str],
    file_version: u32,
) -> serde_json::Value {
    let value = node
        .children()
        .unwrap_or(&[])
        .iter()
        .rfind(|child| child.head().is_some_and(|head| tokens.contains(&head)))
        .and_then(|child| child.get(1))
        .and_then(SexpNode::as_str)
        .and_then(|text| text.parse::<f64>().ok());
    let Some(value) = value else {
        return json!("inherited");
    };
    let value = if margin.is_ratio() {
        value
    } else {
        (value * 1e6).round() / 1e6
    };
    if value == 0.0 && file_version <= ZERO_IS_INHERITED {
        json!("inherited")
    } else {
        json!(if value == 0.0 { 0.0 } else { value })
    }
}

/// The footprint children KiCad writes before its own margins
/// (`PCB_IO_KICAD_SEXPR::format( const FOOTPRINT* )`, 10.0.8).
const BEFORE_FOOTPRINT_MARGINS: &[&str] = &[
    "version",
    "generator",
    "generator_version",
    "locked",
    "placed",
    "layer",
    "tedit",
    "uuid",
    "at",
    "descr",
    "tags",
    "property",
    "component_classes",
    "path",
    "sheetname",
    "sheetfile",
    "units",
];

/// Set the footprint's own `margin`, or remove it for `Inherited`, where
/// KiCad writes it: after the properties, path and sheet, before
/// `clearance`, `zone_connect`, `attr` and the items.
pub(super) fn set_footprint_margin(source: String, margin: Margin, setting: &Setting) -> String {
    let children: Vec<(usize, usize, String)> = find_direct_child_blocks(&source, "footprint")
        .into_iter()
        .filter_map(|(start, end)| {
            let head = parse_sexp(&source[start..end]).ok()?.head()?.to_string();
            Some((start, end, head))
        })
        .collect();
    let tokens = margin.footprint_tokens();
    let existing: Vec<(usize, usize)> = children
        .iter()
        .filter(|(_, _, head)| tokens.contains(&head.as_str()))
        .map(|&(start, end, _)| (start, end))
        .collect();
    let whitespace_before = |index: usize| {
        source[..index]
            .trim_end_matches(|c: char| c.is_ascii_whitespace())
            .len()
    };
    let mut edits = Vec::new();
    let delete = |edits: &mut Vec<SexpEdit>, &(start, end): &(usize, usize)| {
        edits.push(SexpEdit::delete(whitespace_before(start), end));
    };
    match (setting.child(margin), existing.split_first()) {
        (Some(child), Some((&(start, end), rest))) => {
            edits.push(SexpEdit::replace(start, end, child));
            for range in rest {
                delete(&mut edits, range);
            }
        }
        (Some(child), None) => {
            let earlier = &Margin::ALL[..Margin::ALL.iter().position(|m| *m == margin).unwrap()];
            let comes_before = |head: &str| {
                BEFORE_FOOTPRINT_MARGINS.contains(&head)
                    || earlier
                        .iter()
                        .any(|earlier| earlier.footprint_tokens().contains(&head))
            };
            let anchor = children.iter().rfind(|(_, _, head)| comes_before(head));
            if let Some(&(start, end, _)) = anchor {
                let separator = &source[whitespace_before(start)..start];
                edits.push(SexpEdit::insert(end, format!("{separator}{child}")));
            } else if let Some(&(start, _, _)) = children.first() {
                let separator = &source[whitespace_before(start)..start];
                edits.push(SexpEdit::insert(start, format!("{child}{separator}")));
            } else if let Some(close) = source.rfind(')') {
                edits.push(SexpEdit::insert(close, format!(" {child}")));
            }
        }
        (None, _) => {
            for range in &existing {
                delete(&mut edits, range);
            }
        }
    }
    apply_edits(source, edits)
}

/// One value per margin when every edited object agrees, or each distinct one.
pub(super) fn report(
    response: &mut serde_json::Value,
    margin: Margin,
    observed: Vec<serde_json::Value>,
) {
    let mut distinct: Vec<serde_json::Value> = Vec::new();
    for value in observed {
        if !distinct.contains(&value) {
            distinct.push(value);
        }
    }
    match distinct.as_slice() {
        [] => {}
        [one] => response[margin.name()] = one.clone(),
        many => response[margin.name()] = json!(many),
    }
}

/// A KiCad file with its UUIDs masked: KiCad gives a footprint loaded from
/// a file without them new ones on every save.
#[cfg(test)]
pub(super) fn without_uuids(text: &str) -> String {
    regex::Regex::new(r#"\(uuid "[^"]*"\)"#)
        .unwrap()
        .replace_all(text, "(uuid)")
        .into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn values_are_written_as_kicad_writes_them() {
        let setting = |margin: Margin, value: serde_json::Value| {
            let args = json!({ margin.name(): value });
            requested(&args).ok().unwrap().remove(0).1
        };
        let text = |setting: Setting| match setting {
            Setting::Set { text, .. } => text,
            Setting::Inherited => "inherited".to_string(),
        };
        assert_eq!(text(setting(Margin::Mask, json!(-0.05))), "-0.05");
        assert_eq!(text(setting(Margin::Paste, json!(0.0000004))), "0");
        assert_eq!(text(setting(Margin::Paste, json!(-0.0000006))), "-0.000001");
        assert_eq!(text(setting(Margin::Paste, json!(-0.0))), "0");
        assert_eq!(text(setting(Margin::Mask, json!(1.25))), "1.25");
        assert_eq!(text(setting(Margin::PasteRatio, json!(-0.2))), "-0.2");
        assert_eq!(text(setting(Margin::PasteRatio, json!(-0.0))), "0");
        assert_eq!(text(setting(Margin::Mask, json!("inherited"))), "inherited");
    }

    #[test]
    fn other_values_are_refused_on_their_argument() {
        for (margin, value) in [
            (Margin::Mask, json!(null)),
            (Margin::Mask, json!("0.1")),
            (Margin::Paste, json!("none")),
            (Margin::PasteRatio, json!(true)),
            (Margin::Mask, json!(2200.0)),
        ] {
            let refusal = requested(&json!({ margin.name(): value })).err().unwrap();
            assert_eq!(refusal.field, margin.name(), "{value}");
        }
        assert!(requested(&json!({"solder_paste_margin_ratio": 50.0})).is_ok());
    }

    #[test]
    fn a_zero_is_inherited_in_a_file_up_to_20240201() {
        let pad = parse_sexp(
            "(pad \"1\" smd rect (solder_mask_margin 0) (solder_paste_margin -0.1) \
             (solder_paste_margin_ratio 0))",
        )
        .unwrap();
        let read_all =
            |version| Margin::ALL.map(|margin| read(&pad, margin, &[margin.name()], version));
        assert_eq!(
            read_all(20240201),
            [json!("inherited"), json!(-0.1), json!("inherited")]
        );
        assert_eq!(read_all(20240202), [json!(0.0), json!(-0.1), json!(0.0)]);

        let zero = requested(&json!({"solder_mask_margin": 0})).ok().unwrap();
        let refusal = refuse_zero_in_older_file(20240201, &zero).err().unwrap();
        assert_eq!(refusal.field, "solder_mask_margin");
        assert!(
            refusal.reason.contains("upgrade_library"),
            "{}",
            refusal.reason
        );
        assert!(refuse_zero_in_older_file(20240202, &zero).is_ok());
        let other = requested(&json!({"solder_mask_margin": 0.1})).ok().unwrap();
        assert!(refuse_zero_in_older_file(20240108, &other).is_ok());
    }

    #[test]
    fn a_footprint_reads_the_last_of_the_legacy_and_current_ratio() {
        let footprint = parse_sexp(
            "(footprint \"F\" (version 20260206) (solder_paste_ratio -0.1) \
             (solder_paste_margin_ratio -0.2))",
        )
        .unwrap();
        let margin = Margin::PasteRatio;
        assert_eq!(
            read(&footprint, margin, margin.footprint_tokens(), 20260206),
            json!(-0.2)
        );
        let set = set_footprint_margin(
            "(footprint \"F\" (version 20260206) (solder_paste_ratio -0.1) (pad \"1\"))"
                .to_string(),
            margin,
            &Setting::Set {
                text: "-0.3".to_string(),
                value: -0.3,
            },
        );
        assert_eq!(
            set,
            "(footprint \"F\" (version 20260206) (solder_paste_margin_ratio -0.3) (pad \"1\"))"
        );
        assert_eq!(
            set_footprint_margin(set, margin, &Setting::Inherited),
            "(footprint \"F\" (version 20260206) (pad \"1\"))"
        );
    }
}
