//! `set_custom_rule`: add or replace one named rule in a board's sibling
//! `.kicad_dru`, after checking the whole file as KiCad 10 reads it and
//! having kicad-cli load it.
//!
//! KiCad loads a custom rules file whole or not at all, and `kicad-cli`
//! swallows the parse error and checks against its implicit rules alone
//! (#108), so one malformed rule silently disables every other. The check
//! follows KiCad 10.0.6: `DRC_RULES_PARSER` (`pcbnew/drc/drc_rule_parser.cpp`)
//! over `DSNLEXER` (`common/dsnlexer.cpp`), and for conditions and values the
//! expression compiler (`common/libeval_compiler`) with the items and
//! functions of `PCBEXPR_UCODE` (`pcbnew/pcbexpr_evaluator.cpp`,
//! `pcbnew/pcbexpr_functions.cpp`). It names the rule and the fault. It does
//! not check property names, which KiCad looks up in its property manager at
//! run time, so a probe then has kicad-cli load the file on a copy of the
//! board; that verdict is KiCad's own, but names nothing. A board with no item
//! gives the probe nothing to test, so there the check alone decides.

use crate::mcp::error::ToolErrorKind;
use crate::mcp::protocol::CallToolResult;
use crate::tool;
use crate::tools::pcb_components::escape_sexp_string;
use crate::tools::verification::sibling_custom_rules_path;
use crate::tools::{
    file_outcome_uncertain, get_path, invalid_arg, mutation_outcome_uncertain, opt_str,
    require_str, write_if_unchanged, ToolContext, ToolDef,
};
use anyhow::Context;
use copperpour_sexp::writer::read_consistent;
use copperpour_sexp::SexpError;
use serde_json::json;
use std::collections::BTreeSet;
use std::path::Path;

/// The constraint types a caller may write: those whose values are min, opt
/// and max. Lengths are written in mm; `track_angle` and `via_count` are
/// among KiCad's unitless types.
const CALLER_TYPES: &[&str] = &[
    "annular_width",
    "clearance",
    "connection_width",
    "courtyard_clearance",
    "creepage",
    "diff_pair_gap",
    "diff_pair_uncoupled",
    "edge_clearance",
    "hole_clearance",
    "hole_size",
    "hole_to_hole",
    "length",
    "physical_clearance",
    "physical_hole_clearance",
    "silk_clearance",
    "skew",
    "solder_mask_expansion",
    "solder_mask_sliver",
    "solder_paste_abs_margin",
    "text_height",
    "text_thickness",
    "thermal_relief_gap",
    "thermal_spoke_width",
    "track_angle",
    "track_segment_length",
    "track_width",
    "via_count",
    "via_diameter",
];

/// Every constraint type KiCad 10's rule parser reads.
const FILE_TYPES: &[&str] = &[
    "annular_width",
    "assertion",
    "bridged_mask",
    "clearance",
    "connection_width",
    "courtyard_clearance",
    "creepage",
    "diff_pair_gap",
    "diff_pair_uncoupled",
    "disallow",
    "edge_clearance",
    "hole_clearance",
    "hole_size",
    "hole_to_hole",
    "length",
    "min_resolved_spokes",
    "physical_clearance",
    "physical_hole_clearance",
    "silk_clearance",
    "skew",
    "solder_mask_expansion",
    "solder_mask_sliver",
    "solder_paste_abs_margin",
    "solder_paste_rel_margin",
    "text_height",
    "text_thickness",
    "thermal_relief_gap",
    "thermal_spoke_width",
    "track_angle",
    "track_segment_length",
    "track_width",
    "via_count",
    "via_dangling",
    "via_diameter",
    "zone_connection",
];

/// Old names the parser still reads as their replacements.
const RENAMED_TYPES: &[(&str, &str)] = &[
    ("mechanical_clearance", "physical_clearance"),
    ("mechanical_hole_clearance", "physical_hole_clearance"),
    ("hole", "hole_size"),
];

/// Types whose values KiCad reads with no units at all.
const UNITLESS_TYPES: &[&str] = &[
    "via_count",
    "min_resolved_spokes",
    "track_angle",
    "via_dangling",
    "bridged_mask",
];

const DISALLOW_ITEMS: &[&str] = &[
    "track",
    "via",
    "through_via",
    "blind_via",
    "buried_via",
    "micro_via",
    "pad",
    "zone",
    "text",
    "graphic",
    "hole",
    "footprint",
];

const SEVERITIES: &[&str] = &["ignore", "warning", "error", "exclusion"];

/// The units `PCBEXPR_UNIT_RESOLVER` reads, in the order its lexer tries them.
const PCB_UNITS: &[&str] = &["mil", "mm", "in", "deg", "fs", "ps"];

/// The functions `PCBEXPR_BUILTIN_FUNCTIONS::RegisterAllFunctions` registers,
/// with how many arguments each signature takes. KiCad matches the name
/// without regard to case.
const FUNCTIONS: &[(&str, usize)] = &[
    ("existsOnLayer", 1),
    ("isPlated", 0),
    ("insideCourtyard", 1),
    ("insideFrontCourtyard", 1),
    ("insideBackCourtyard", 1),
    ("intersectsCourtyard", 1),
    ("intersectsFrontCourtyard", 1),
    ("intersectsBackCourtyard", 1),
    ("insideArea", 1),
    ("intersectsArea", 1),
    ("enclosedByArea", 1),
    ("isMicroVia", 0),
    ("isBlindVia", 0),
    ("isBuriedVia", 0),
    ("isBlindBuriedVia", 0),
    ("memberOf", 1),
    ("memberOfGroup", 1),
    ("memberOfFootprint", 1),
    ("memberOfSheet", 1),
    ("memberOfSheetOrChildren", 1),
    ("fromTo", 2),
    ("isCoupledDiffPair", 0),
    ("inDiffPair", 1),
    ("getField", 1),
    ("hasNetclass", 1),
    ("hasExactNetclass", 1),
    ("hasComponentClass", 1),
];

/// Fields `PCBEXPR_UCODE::CreateVarRef` resolves itself, for `A` and `B` only.
const ITEM_FIELDS: &[&str] = &["NetClass", "ComponentClass", "NetName", "Type"];

/// The rule the probe adds to its copy of the file. Its assertion fails on
/// every item, so kicad-cli reports it when, and only when, it loaded the file.
const PROBE_RULE: &str = "copperpour_probe";

/// A board's top-level nodes that KiCad 10's DRC tests rules against
/// (`DRC_TEST_PROVIDER::forEachGeometryItem`). A board with none of them gives
/// the probe nothing to report, and no rule anything to act on yet.
const DRC_ITEMS: &[&str] = &[
    "arc",
    "barcode",
    "dimension",
    "footprint",
    "gr_arc",
    "gr_circle",
    "gr_curve",
    "gr_line",
    "gr_poly",
    "gr_rect",
    "gr_text",
    "gr_text_box",
    "module",
    "segment",
    "table",
    "target",
    "via",
    "zone",
];

pub(super) fn tool() -> ToolDef {
    tool!(
        "set_custom_rule",
        "Add or replace one named rule in the board's sibling .kicad_dru custom rules file: \
         its constraints' min, opt and max values, and an optional condition, layer and \
         severity. A rule of the same name is replaced where it stands; a new one goes last, \
         where KiCad gives it precedence over every earlier rule that matches the same items. \
         The rest of the file is left as written. kicad-cli drops a rules file it cannot \
         parse, so the whole file is first checked as KiCad 10 reads it, then loaded by \
         kicad-cli on a copy of the board; a fault, or kicad-cli not loading it, refuses the \
         write. On a board with no item yet kicad-cli has nothing to test, so the first check \
         alone decides, and the reply names the property names it left unchecked.",
        json!({
            "type": "object",
            "properties": {
                "board": { "type": "string", "description": "Path to .kicad_pcb file" },
                "name": { "type": "string", "description": "Rule name. A rule of this name already in the file is replaced" },
                "constraints": {
                    "type": "array",
                    "minItems": 1,
                    "description": "The rule's constraints. Values are in mm, except track_angle in degrees and via_count as a count. disallow, zone_connection, min_resolved_spokes, assertion, via_dangling and bridged_mask take other arguments or none, and solder_paste_rel_margin is not a length, so this tool does not write them",
                    "items": {
                        "type": "object",
                        "properties": {
                            "type": { "type": "string", "enum": CALLER_TYPES },
                            "min": { "type": "number" },
                            "opt": { "type": "number" },
                            "max": { "type": "number" }
                        },
                        "required": ["type"],
                        "additionalProperties": false
                    }
                },
                "condition": { "type": "string", "description": "KiCad rule condition, for example A.hasNetclass('DP') && A.Layer == 'F.Cu'" },
                "layer": { "type": "string", "description": "A layer name as the board names it, 'outer' or 'inner'. KiCad matches * and ? as wildcards" },
                "severity": { "type": "string", "enum": SEVERITIES }
            },
            "required": ["board", "name", "constraints"],
            "additionalProperties": false
        }),
        |args, ctx| async move { handle_set_custom_rule(args, ctx).await }
    )
}

/// One constraint of the requested rule.
struct Constraint {
    kind: String,
    values: Vec<(&'static str, f64)>,
}

/// The rule a call asks for.
struct Request {
    name: String,
    constraints: Vec<Constraint>,
    condition: Option<String>,
    layer: Option<String>,
    severity: Option<String>,
}

impl Request {
    fn read(args: &serde_json::Value) -> Result<Self, CallToolResult> {
        let name = require_str(args, "name")?.to_string();
        if name.contains(['\n', '\r']) {
            return Err(invalid_arg("name", "KiCad reads a rule name on one line"));
        }
        let mut constraints = Vec::new();
        for item in args["constraints"].as_array().into_iter().flatten() {
            let kind = require_str(item, "type")?;
            let values = ["min", "opt", "max"]
                .into_iter()
                .filter_map(|key| item[key].as_f64().map(|value| (key, value)))
                .collect();
            constraints.push(Constraint {
                kind: kind.to_string(),
                values,
            });
        }
        Ok(Self {
            name,
            constraints,
            condition: opt_str(args, "condition").map(str::to_string),
            layer: opt_str(args, "layer").map(str::to_string),
            severity: opt_str(args, "severity").map(str::to_string),
        })
    }

    /// The rule as written to the file, in `set_layer_constraints`' layout.
    fn text(&self) -> String {
        let mut text = format!("(rule \"{}\"", escape_sexp_string(&self.name));
        if let Some(condition) = &self.condition {
            text += &format!("\n  (condition \"{}\")", escape_sexp_string(condition));
        }
        match self.layer.as_deref() {
            Some(keyword @ ("outer" | "inner")) => text += &format!("\n  (layer {keyword})"),
            Some(layer) => text += &format!("\n  (layer \"{}\")", escape_sexp_string(layer)),
            None => {}
        }
        for constraint in &self.constraints {
            let unit = if UNITLESS_TYPES.contains(&constraint.kind.as_str()) {
                ""
            } else {
                "mm"
            };
            text += &format!("\n  (constraint {}", constraint.kind);
            for (key, value) in &constraint.values {
                text += &format!(" ({key} {value}{unit})");
            }
            text.push(')');
        }
        if let Some(severity) = &self.severity {
            text += &format!("\n  (severity {severity})");
        }
        text.push(')');
        text
    }
}

async fn handle_set_custom_rule(
    args: &serde_json::Value,
    ctx: &ToolContext,
) -> anyhow::Result<CallToolResult> {
    const OPERATION: &str = "set_custom_rule";
    let board = get_path(args, "board")?;
    let request = match Request::read(args) {
        Ok(request) => request,
        Err(refusal) => return Ok(refusal),
    };
    let BoardFacts {
        user_layers,
        has_items,
    } = read_board(&board)?;
    let rules_path = sibling_custom_rules_path(&board);
    let read = match read_consistent(&rules_path) {
        Ok(text) => Some(text),
        Err(SexpError::Io(error)) if error.kind() == std::io::ErrorKind::NotFound => None,
        Err(error) => return Err(error.into()),
    };
    let base = match &read {
        Some(text) if !text.trim().is_empty() => text.clone(),
        _ => "(version 1)\n".to_string(),
    };

    let named: Vec<RuleSpan> = check_rules(&base, &user_layers)
        .rules
        .into_iter()
        .filter(|rule| rule.name == request.name)
        .collect();
    if named.len() > 1 {
        return Ok(CallToolResult::error_kind(
            ToolErrorKind::AmbiguousTarget {
                target: request.name.clone(),
                candidates: named
                    .iter()
                    .map(|rule| format!("line {}", line_of(&base, rule.start)))
                    .collect(),
            },
            format!(
                "'{}' has {} rules named '{}', so Copperpour cannot tell which one to replace. \
                 Rename all but one, then retry.",
                rules_path.display(),
                named.len(),
                request.name
            ),
        ));
    }
    let rule = request.text();
    let previous = named
        .first()
        .map(|span| base[span.start..span.end].to_string());
    let next = match named.first() {
        Some(span) => format!("{}{}{}", &base[..span.start], rule, &base[span.end..]),
        None => {
            let mut next = base.trim_end().to_string();
            next.push_str("\n\n");
            next.push_str(&rule);
            next.push('\n');
            next
        }
    };

    let checked = check_rules(&next, &user_layers);
    if !checked.faults.is_empty() {
        return Ok(CallToolResult::error_kind(
            ToolErrorKind::PlanBlocked {
                operation: OPERATION.to_string(),
                reasons: checked.faults.clone(),
            },
            format!(
                "{OPERATION} wrote nothing: with this rule, KiCad would report errors in '{}': \
                 {}. kicad-cli drops a rules file it cannot parse and checks against its \
                 implicit rules alone.",
                rules_path.display(),
                checked.faults.join("; ")
            ),
        ));
    }
    // With no item on the board the probe could not tell a loaded file from a
    // dropped one, and no rule acts on anything yet.
    if has_items {
        let verdict =
            kicad_verdict(&ctx.config.kicad_cli, &board, &next, &checked.properties).await?;
        if let Some(reason) = verdict {
            return Ok(CallToolResult::error_kind(
                ToolErrorKind::PlanBlocked {
                    operation: OPERATION.to_string(),
                    reasons: vec![reason.clone()],
                },
                format!(
                    "{OPERATION} wrote nothing to '{}': {reason}",
                    rules_path.display()
                ),
            ));
        }
    }

    let changed = read.as_deref() != Some(next.as_str());
    if changed {
        if let Some(refusal) = write_if_unchanged(
            &rules_path,
            "custom rules file",
            OPERATION,
            read.as_deref(),
            next.clone(),
        )? {
            return Ok(refusal);
        }
    }

    let written = match read_consistent(&rules_path) {
        Ok(text) => text,
        Err(error) => {
            return Ok(file_outcome_uncertain(
                &rules_path,
                OPERATION,
                format!("the file could not be read back: {error}"),
            ))
        }
    };
    let current = check_rules(&written, &user_layers)
        .rules
        .into_iter()
        .find(|span| span.name == request.name)
        .map(|span| written[span.start..span.end].to_string());
    if current.as_deref() != Some(rule.as_str()) {
        return Ok(mutation_outcome_uncertain(
            &rules_path,
            OPERATION,
            "the rule read back is not the rule written",
        ));
    }

    Ok(CallToolResult::json(&json!({
        "success": true,
        "rules_file": rules_path,
        "name": request.name,
        "created": read.is_none(),
        "changed": changed,
        "previous": previous,
        "current": current,
        "kicad_checked": has_items,
        "unchecked_properties": if has_items { BTreeSet::new() } else { checked.properties },
    })))
}

/// What the check and the probe need from the board.
struct BoardFacts {
    /// The names the board gives its layers, besides the canonical ones;
    /// KiCad matches a rule's layer against both.
    user_layers: Vec<String>,
    /// Whether the board holds an item KiCad's DRC tests rules against.
    has_items: bool,
}

fn read_board(board: &Path) -> anyhow::Result<BoardFacts> {
    let text = std::fs::read_to_string(board)
        .with_context(|| format!("reading the layers of '{}'", board.display()))?;
    let tree = copperpour_sexp::parse_sexp(&text)?;
    let has_items = tree
        .children()
        .unwrap_or(&[])
        .iter()
        .filter_map(|node| node.children().and(node.head()))
        .any(|head| DRC_ITEMS.contains(&head));
    Ok(BoardFacts {
        user_layers: copperpour_sexp::layers::layers(&tree)
            .into_iter()
            .filter_map(|layer| layer.user_name)
            .collect(),
        has_items,
    })
}

/// KiCad's own verdict on `rules`: `None` when kicad-cli loads them, else why
/// the write is refused. KiCad 10 drops a rules file it cannot parse without a
/// word, so the probe runs kicad-cli's DRC on a copy of the board with the
/// file and [`PROBE_RULE`] beside it, and looks for that rule in the report.
/// The `.kicad_pro` is left behind on purpose: it holds the DRC severities,
/// and one that ignores assertion failures would hide the probe.
async fn kicad_verdict(
    cli: &str,
    board: &Path,
    rules: &str,
    properties: &BTreeSet<String>,
) -> anyhow::Result<Option<String>> {
    let dir = tempfile::tempdir()?;
    let copy = dir
        .path()
        .join(board.file_name().context("the board path names no file")?);
    tokio::fs::copy(board, &copy).await?;
    tokio::fs::write(
        copy.with_extension("kicad_dru"),
        format!(
            "{}\n\n(rule \"{PROBE_RULE}\"\n  (constraint assertion \"1mm < 0mm\"))\n",
            rules.trim_end()
        ),
    )
    .await?;
    let report = match crate::tools::cli::run_drc(cli, &copy, false).await {
        Ok(report) => report,
        Err(error) => {
            return Ok(Some(format!(
                "kicad-cli could not check the file: {error:#}"
            )))
        }
    };
    if report.violations.iter().any(|violation| {
        violation.rule == "assertion_failure" && violation.description.contains(PROBE_RULE)
    }) {
        return Ok(None);
    }
    let mut reason = "kicad-cli did not load the file: a probe rule that fails on every item \
                      reported nothing, which is how KiCad 10 drops a rules file it cannot parse"
        .to_string();
    if !properties.is_empty() {
        let names: Vec<String> = properties.iter().map(|name| format!("'{name}'")).collect();
        reason += &format!(
            ". Copperpour's own check does not know KiCad's property names, and the file \
             uses {}",
            names.join(", ")
        );
    }
    Ok(Some(reason))
}

fn line_of(text: &str, offset: usize) -> usize {
    text[..offset].matches('\n').count() + 1
}

// ─── The rules file, as DSNLEXER and DRC_RULES_PARSER read it ────────────────

#[derive(Debug, Clone, PartialEq)]
enum Tok {
    Open,
    Close,
    /// A quoted string, its escapes decoded.
    Quoted(String),
    /// Anything else up to a separator: a keyword, a symbol or a number.
    Bare(String),
}

impl Tok {
    fn text(&self) -> &str {
        match self {
            Tok::Open => "(",
            Tok::Close => ")",
            Tok::Quoted(text) | Tok::Bare(text) => text,
        }
    }

    /// `DSNLEXER::IsSymbol`: a string, a symbol or a keyword, not a number.
    fn is_symbol(&self) -> bool {
        match self {
            Tok::Quoted(_) => true,
            Tok::Bare(text) => !is_number(text),
            _ => false,
        }
    }

    fn is_keyword(&self, keyword: &str) -> bool {
        matches!(self, Tok::Bare(text) if text == keyword)
    }
}

struct Token {
    tok: Tok,
    start: usize,
    end: usize,
    /// What the lexer skipped before the token, which a value expression
    /// keeps between its tokens.
    separator: String,
}

fn is_space(byte: u8) -> bool {
    matches!(byte, b' ' | b'\n' | b'\r' | b'\t' | b'\0')
}

/// `DSNLEXER`'s number: `[-+]?[0-9]*\.?[0-9]+([eE][-+]?[0-9]+)?`, whole.
fn is_number(text: &str) -> bool {
    let bytes = text.as_bytes();
    let mut at = usize::from(matches!(bytes.first(), Some(b'-' | b'+')));
    let mut digits = false;
    while bytes.get(at).is_some_and(u8::is_ascii_digit) {
        at += 1;
        digits = true;
    }
    if bytes.get(at) == Some(&b'.') {
        at += 1;
        while bytes.get(at).is_some_and(u8::is_ascii_digit) {
            at += 1;
            digits = true;
        }
    }
    if digits && matches!(bytes.get(at), Some(b'e' | b'E')) {
        at += 1;
        digits = false;
        if matches!(bytes.get(at), Some(b'-' | b'+')) {
            at += 1;
        }
        while bytes.get(at).is_some_and(u8::is_ascii_digit) {
            at += 1;
            digits = true;
        }
    }
    digits && at == bytes.len()
}

/// The file's tokens, or the offset and text of a string left open. A line
/// whose first non-blank character is `#` is a comment; anywhere else `#` is
/// text.
fn lex(text: &str) -> Result<Vec<Token>, (usize, String)> {
    let mut tokens = Vec::new();
    let mut separator = String::new();
    let mut base = 0;
    for line in text.split_inclusive('\n') {
        let bytes = line.as_bytes();
        let lead = bytes.iter().take_while(|byte| is_space(**byte)).count();
        if bytes.get(lead) == Some(&b'#') {
            separator.push_str(&line[..lead]);
            base += line.len();
            continue;
        }
        let mut at = 0;
        while at < bytes.len() {
            if is_space(bytes[at]) {
                separator.push(bytes[at] as char);
                at += 1;
                continue;
            }
            let start = at;
            let tok = match bytes[at] {
                b'(' => {
                    at += 1;
                    Tok::Open
                }
                b')' => {
                    at += 1;
                    Tok::Close
                }
                b'"' => {
                    let (decoded, end) = quoted(bytes, at + 1).ok_or_else(|| {
                        (
                            base + start,
                            "a quoted string is not closed on its line".to_string(),
                        )
                    })?;
                    at = end;
                    Tok::Quoted(decoded)
                }
                _ => {
                    while at < bytes.len()
                        && !matches!(bytes[at], b'(' | b')')
                        && !is_space(bytes[at])
                    {
                        at += 1;
                    }
                    Tok::Bare(line[start..at].to_string())
                }
            };
            tokens.push(Token {
                tok,
                start: base + start,
                end: base + at,
                separator: std::mem::take(&mut separator),
            });
        }
        base += line.len();
    }
    Ok(tokens)
}

/// A quoted string's text from just after its opening quote, with the escapes
/// `DSNLEXER` decodes, and the offset after its closing quote; `None` when
/// the line ends first.
fn quoted(bytes: &[u8], mut at: usize) -> Option<(String, usize)> {
    let mut text = Vec::new();
    loop {
        match *bytes.get(at)? {
            b'"' => return Some((String::from_utf8_lossy(&text).into_owned(), at + 1)),
            b'\\' => {
                let escaped = *bytes.get(at + 1)?;
                at += 2;
                match escaped {
                    b'"' | b'\\' => text.push(escaped),
                    b'a' => text.push(0x07),
                    b'b' => text.push(0x08),
                    b'f' => text.push(0x0c),
                    b'n' => text.push(b'\n'),
                    b'r' => text.push(b'\r'),
                    b't' => text.push(b'\t'),
                    b'v' => text.push(0x0b),
                    b'x' => {
                        let digits = bytes[at..]
                            .iter()
                            .take(2)
                            .take_while(|byte| byte.is_ascii_hexdigit())
                            .count();
                        let value = std::str::from_utf8(&bytes[at..at + digits]).ok()?;
                        text.push(u8::from_str_radix(value, 16).unwrap_or(b'x'));
                        at += digits;
                    }
                    _ => {
                        // An octal escape of up to three digits; with none,
                        // the backslash stands and the next character is read
                        // as itself.
                        at -= 1;
                        let digits = bytes[at..]
                            .iter()
                            .take(3)
                            .take_while(|byte| (b'0'..=b'7').contains(*byte))
                            .count();
                        if digits == 0 {
                            text.push(b'\\');
                        } else {
                            let value = std::str::from_utf8(&bytes[at..at + digits]).ok()?;
                            text.push(u32::from_str_radix(value, 8).ok()? as u8);
                            at += digits;
                        }
                    }
                }
            }
            byte => {
                text.push(byte);
                at += 1;
            }
        }
    }
}

/// Where a rule sits in the file text, from its `(` to its `)`.
#[derive(Debug, Clone)]
struct RuleSpan {
    name: String,
    start: usize,
    end: usize,
}

#[derive(Debug, Default)]
struct Checked {
    rules: Vec<RuleSpan>,
    /// Each fault, naming its rule and line.
    faults: Vec<String>,
    /// Property names the conditions use, which KiCad resolves at run time.
    properties: BTreeSet<String>,
}

/// Check `text` as KiCad's rule parser reads it, against the board's
/// canonical and user layer names. KiCad stops at the first error; the check
/// carries on to the next rule, so every faulty rule is named.
fn check_rules(text: &str, user_layers: &[String]) -> Checked {
    let mut checked = Checked::default();
    let tokens = match lex(text) {
        Ok(tokens) => tokens,
        Err((offset, fault)) => {
            checked
                .faults
                .push(format!("line {}: {fault}", line_of(text, offset)));
            return checked;
        }
    };
    if let Some(variable) = tokens.iter().find(|token| token.tok.text().contains("${")) {
        checked.faults.push(format!(
            "line {}: '{}' holds a text variable, which KiCad expands from the board and project \
             before it reads the rules; Copperpour does not",
            line_of(text, variable.start),
            variable.tok.text()
        ));
        return checked;
    }
    let mut parser = RuleParser {
        tokens: &tokens,
        at: 0,
        user_layers,
        properties: &mut checked.properties,
    };
    let mut have_version = false;
    while parser.at < tokens.len() {
        let open = parser.at;
        let line = line_of(text, tokens[open].start);
        if parser.next() != Some(Tok::Open) {
            checked.faults.push(format!(
                "line {line}: expected '(' to open a rule or the version"
            ));
            return checked;
        }
        let head = parser.next();
        let is_version = head.as_ref().is_some_and(|tok| tok.is_keyword("version"));
        if !have_version && !is_version {
            checked
                .faults
                .push(format!("line {line}: the file must open with (version 1)"));
            return checked;
        }
        if is_version {
            have_version = true;
            if let Err(fault) = parser.version() {
                checked.faults.push(format!("line {line}: {fault}"));
                return checked;
            }
            continue;
        }
        if !head.as_ref().is_some_and(|tok| tok.is_keyword("rule")) {
            checked.faults.push(format!(
                "line {line}: expected (rule …) or (version …), found '{}'",
                head.as_ref().map_or("the end of the file", Tok::text)
            ));
            return checked;
        }
        let Some(close) = closing(&tokens, open) else {
            checked
                .faults
                .push(format!("line {line}: the rule opened here is never closed"));
            return checked;
        };
        match parser.rule() {
            Ok(name) => checked.rules.push(RuleSpan {
                name,
                start: tokens[open].start,
                end: tokens[close].end,
            }),
            Err((name, fault)) => {
                let what = match &name {
                    Some(name) => format!("rule '{name}', line {line}"),
                    None => format!("line {line}"),
                };
                checked.faults.push(format!("{what}: {fault}"));
                if let Some(name) = name {
                    checked.rules.push(RuleSpan {
                        name,
                        start: tokens[open].start,
                        end: tokens[close].end,
                    });
                }
            }
        }
        parser.at = close + 1;
    }
    checked
}

/// The index of the `)` that closes the `(` at `open`.
fn closing(tokens: &[Token], open: usize) -> Option<usize> {
    let mut depth = 0usize;
    for (index, token) in tokens.iter().enumerate().skip(open) {
        match token.tok {
            Tok::Open => depth += 1,
            Tok::Close => {
                depth -= 1;
                if depth == 0 {
                    return Some(index);
                }
            }
            _ => {}
        }
    }
    None
}

struct RuleParser<'a> {
    tokens: &'a [Token],
    at: usize,
    user_layers: &'a [String],
    properties: &'a mut BTreeSet<String>,
}

impl RuleParser<'_> {
    fn next(&mut self) -> Option<Tok> {
        let tok = self.tokens.get(self.at).map(|token| token.tok.clone());
        self.at += 1;
        tok
    }

    fn close(&mut self, what: &str) -> Result<(), String> {
        match self.next() {
            Some(Tok::Close) => Ok(()),
            _ => Err(format!("{what} is missing its ')'")),
        }
    }

    fn version(&mut self) -> Result<(), String> {
        match self.next() {
            Some(Tok::Bare(number)) if is_number(&number) => self.close("the version"),
            _ => Err("the version needs a number, as (version 1)".to_string()),
        }
    }

    /// One rule, after its `(rule`: its name, or the fault and the name when
    /// it was read.
    fn rule(&mut self) -> Result<String, (Option<String>, String)> {
        let name = match self.next() {
            Some(tok) if tok.is_symbol() => tok.text().to_string(),
            _ => return Err((None, "the rule has no name".to_string())),
        };
        let mut types = Vec::new();
        let mut has_layer = false;
        loop {
            let fault = match self.next() {
                Some(Tok::Close) => return Ok(name),
                Some(Tok::Open) => match self.next() {
                    Some(tok) if tok.is_keyword("constraint") => self.constraint(&mut types),
                    Some(tok) if tok.is_keyword("condition") => self.condition(),
                    Some(tok) if tok.is_keyword("layer") => {
                        if has_layer {
                            Err("the rule has a second layer clause".to_string())
                        } else {
                            has_layer = true;
                            self.layer()
                        }
                    }
                    Some(tok) if tok.is_keyword("severity") => self.severity(),
                    other => Err(format!(
                        "'{}' is not a rule clause; KiCad takes constraint, condition, layer \
                         and severity",
                        other.as_ref().map_or("", Tok::text)
                    )),
                },
                Some(other) => Err(format!("expected '(' before '{}'", other.text())),
                None => Err("the file ends inside the rule".to_string()),
            };
            if let Err(fault) = fault {
                return Err((Some(name), fault));
            }
        }
    }

    fn condition(&mut self) -> Result<(), String> {
        match self.next() {
            Some(tok) if tok.is_symbol() => {
                check_expression(tok.text(), Resolver::Pcb, self.properties)
                    .map_err(|fault| format!("condition \"{}\": {fault}", tok.text()))?;
                self.close("the condition")
            }
            _ => Err("the condition needs a quoted expression".to_string()),
        }
    }

    fn layer(&mut self) -> Result<(), String> {
        match self.next() {
            None | Some(Tok::Close) => Err("the layer clause names no layer".to_string()),
            Some(tok) if tok.is_keyword("outer") || tok.is_keyword("inner") => {
                self.close("the layer clause")
            }
            Some(tok) => {
                let pattern = tok.text();
                let found = copperpour_sexp::layers::canonical_names()
                    .chain(self.user_layers.iter().cloned())
                    .any(|name| wildcard_match(pattern, &name));
                if !found {
                    return Err(format!(
                        "'{pattern}' is not a layer name, 'outer' or 'inner' on this board"
                    ));
                }
                self.close("the layer clause")
            }
        }
    }

    fn severity(&mut self) -> Result<(), String> {
        match self.next() {
            Some(Tok::Bare(severity)) if SEVERITIES.contains(&severity.as_str()) => {
                self.close("the severity clause")
            }
            other => Err(format!(
                "'{}' is not a severity; KiCad takes ignore, warning, error and exclusion",
                other.as_ref().map_or("", Tok::text)
            )),
        }
    }

    fn constraint(&mut self, types: &mut Vec<String>) -> Result<(), String> {
        let kind = match self.next() {
            Some(Tok::Bare(name)) => {
                let name = RENAMED_TYPES
                    .iter()
                    .find(|(old, _)| *old == name)
                    .map_or(name.as_str(), |(_, new)| new)
                    .to_string();
                if !FILE_TYPES.contains(&name.as_str()) {
                    return Err(format!("'{name}' is not a constraint type"));
                }
                name
            }
            Some(Tok::Close) | None => return Err("the constraint has no type".to_string()),
            Some(other) => return Err(format!("'{}' is not a constraint type", other.text())),
        };
        if types.contains(&kind) {
            return Err(format!("the rule has a second '{kind}' constraint"));
        }
        types.push(kind.clone());
        match kind.as_str() {
            "disallow" => loop {
                match self.next() {
                    Some(Tok::Close) => return Ok(()),
                    Some(Tok::Bare(item) | Tok::Quoted(item))
                        if DISALLOW_ITEMS.contains(&item.as_str()) => {}
                    Some(other) => {
                        return Err(format!("disallow does not take '{}'", other.text()))
                    }
                    None => return Err("disallow is missing its ')'".to_string()),
                }
            },
            "zone_connection" => match self.next() {
                Some(Tok::Bare(mode) | Tok::Quoted(mode))
                    if ["solid", "thermal_reliefs", "none"].contains(&mode.as_str()) =>
                {
                    self.close("zone_connection")
                }
                _ => Err("zone_connection takes solid, thermal_reliefs or none".to_string()),
            },
            "min_resolved_spokes" => match self.next() {
                Some(Tok::Bare(number)) if is_number(&number) => self.close("min_resolved_spokes"),
                _ => Err("min_resolved_spokes takes a number".to_string()),
            },
            "assertion" => match self.next() {
                Some(tok) if tok.is_symbol() => {
                    check_expression(tok.text(), Resolver::Pcb, self.properties)
                        .map_err(|fault| format!("assertion \"{}\": {fault}", tok.text()))?;
                    self.close("the assertion")
                }
                _ => Err("the assertion needs a quoted expression".to_string()),
            },
            _ => self.values(&kind),
        }
    }

    /// `(min …)`, `(opt …)`, `(max …)` and skew's `(within_diff_pairs)`, up
    /// to the constraint's `)`.
    fn values(&mut self, kind: &str) -> Result<(), String> {
        let resolver = if UNITLESS_TYPES.contains(&kind) {
            Resolver::Unitless
        } else {
            Resolver::Pcb
        };
        let mut previous: Option<Quantity> = None;
        loop {
            match self.next() {
                Some(Tok::Close) => return Ok(()),
                Some(Tok::Open) => {}
                Some(other) => return Err(format!("expected '(' before '{}'", other.text())),
                None => return Err(format!("{kind} is missing its ')'")),
            }
            match self.next() {
                Some(tok) if tok.is_keyword("within_diff_pairs") => {
                    if kind != "skew" {
                        return Err(format!("{kind} does not take within_diff_pairs"));
                    }
                    self.close("within_diff_pairs")?;
                }
                Some(Tok::Bare(key)) if ["min", "opt", "max"].contains(&key.as_str()) => {
                    let value = self.value_expression();
                    if value.is_empty() {
                        return Err(format!("{kind}'s {key} has no value"));
                    }
                    let mut properties = BTreeSet::new();
                    let quantity = check_expression(&value, resolver, &mut properties)
                        .map_err(|fault| format!("{kind}'s {key} '{value}': {fault}"))?;
                    if quantity == Quantity::Time && !matches!(kind, "length" | "skew") {
                        return Err(format!(
                            "{kind}'s {key} '{value}' is a time; only length and skew take one"
                        ));
                    }
                    if previous.is_some_and(|previous| previous != quantity) {
                        return Err(format!("{kind} mixes units across its values"));
                    }
                    previous = Some(quantity);
                }
                other => {
                    return Err(format!(
                        "{kind} does not take '{}'; KiCad takes min, opt and max",
                        other.as_ref().map_or("", Tok::text)
                    ))
                }
            }
        }
    }

    /// `DRC_RULES_PARSER::parseExpression`: the tokens up to the `)` that
    /// closes the value, joined by what separated them.
    fn value_expression(&mut self) -> String {
        let mut text = String::new();
        let mut depth = 1;
        while let Some(token) = self.tokens.get(self.at) {
            self.at += 1;
            match token.tok {
                Tok::Open => depth += 1,
                Tok::Close => {
                    depth -= 1;
                    if depth == 0 {
                        break;
                    }
                }
                _ => {}
            }
            if !text.is_empty() {
                text.push_str(&token.separator);
            }
            text.push_str(token.tok.text());
        }
        text
    }
}

/// `wxString::Matches`: `*` stands for any run of characters, `?` for one.
fn wildcard_match(pattern: &str, name: &str) -> bool {
    let pattern: Vec<char> = pattern.chars().collect();
    let name: Vec<char> = name.chars().collect();
    let (mut p, mut n) = (0, 0);
    let mut star: Option<(usize, usize)> = None;
    while n < name.len() {
        if p < pattern.len() && (pattern[p] == '?' || pattern[p] == name[n]) {
            p += 1;
            n += 1;
        } else if p < pattern.len() && pattern[p] == '*' {
            star = Some((p, n));
            p += 1;
        } else if let Some((star_p, star_n)) = star {
            p = star_p + 1;
            n = star_n + 1;
            star = Some((star_p, star_n + 1));
        } else {
            return false;
        }
    }
    pattern[p..].iter().all(|c| *c == '*')
}

// ─── Conditions and values, as KiCad's expression compiler reads them ───────

/// Which units the expression's lexer knows: a value of one of the parser's
/// unitless types takes none.
#[derive(Clone, Copy, PartialEq)]
enum Resolver {
    Pcb,
    Unitless,
}

/// What a value measures, as `UNITS_PROVIDER::GetTypeFromUnits` sorts its
/// units; KiCad refuses a constraint whose values mix them.
#[derive(Clone, Copy, PartialEq, Debug)]
enum Quantity {
    Distance,
    Time,
    Unitless,
}

#[derive(Debug, Clone, PartialEq)]
enum Lexeme {
    Value,
    Unit(usize),
    Str,
    Ident(String),
    Op(&'static str),
    Open,
    Close,
    Comma,
    Dot,
    Semicolon,
    End,
}

fn match_ahead(chars: &[char], at: usize, text: &str, stop: impl Fn(char) -> bool) -> bool {
    let wanted: Vec<char> = text.chars().collect();
    chars.len() >= at + wanted.len()
        && chars[at..at + wanted.len()] == wanted[..]
        && chars.get(at + wanted.len()).is_none_or(|next| stop(*next))
}

/// `COMPILER::lexDefault`: only a space separates tokens.
fn lex_expression(text: &str, resolver: Resolver) -> Result<Vec<Lexeme>, String> {
    let chars: Vec<char> = text.chars().collect();
    let decimal = |c: char| c == '.' || c == ',';
    let mut lexemes = Vec::new();
    let mut at = 0;
    'next: loop {
        while chars.get(at) == Some(&' ') {
            at += 1;
        }
        let Some(&c) = chars.get(at) else {
            lexemes.push(Lexeme::End);
            return Ok(lexemes);
        };
        if c.is_ascii_digit() {
            // One decimal separator at most; a second ends the number.
            let mut separator = false;
            while let Some(&c) = chars.get(at) {
                if !(c.is_ascii_digit() || decimal(c)) || (decimal(c) && separator) {
                    break;
                }
                separator |= decimal(c);
                at += 1;
            }
            lexemes.push(Lexeme::Value);
            continue;
        }
        if resolver == Resolver::Pcb {
            for (index, unit) in PCB_UNITS.iter().enumerate() {
                if match_ahead(&chars, at, unit, |next| !next.is_ascii_alphanumeric()) {
                    lexemes.push(Lexeme::Unit(index));
                    at += unit.len();
                    continue 'next;
                }
            }
        }
        if c == '\'' {
            // To the next unescaped quote, or to the end: KiCad does not
            // complain of an unclosed string.
            at += 1;
            while let Some(&c) = chars.get(at) {
                if c == '\'' {
                    break;
                }
                if c == '\\' && chars.get(at + 1) == Some(&'\'') {
                    at += 1;
                }
                at += 1;
            }
            at += 1;
            lexemes.push(Lexeme::Str);
            continue;
        }
        if c.is_ascii_alphabetic() || c == '_' {
            let start = at;
            while chars
                .get(at)
                .is_some_and(|c| c.is_ascii_alphanumeric() || *c == '_')
            {
                at += 1;
            }
            lexemes.push(Lexeme::Ident(chars[start..at].iter().collect()));
            continue;
        }
        for (op, repeated) in [
            ("==", '='),
            ("!=", '='),
            ("<=", '='),
            (">=", '='),
            ("&&", '&'),
            ("||", '|'),
        ] {
            if match_ahead(&chars, at, op, |next| next != repeated) {
                lexemes.push(Lexeme::Op(op));
                at += 2;
                continue 'next;
            }
        }
        lexemes.push(match c {
            '+' => Lexeme::Op("+"),
            '!' => Lexeme::Op("!"),
            '-' => Lexeme::Op("-"),
            '*' => Lexeme::Op("*"),
            '/' => Lexeme::Op("/"),
            '<' => Lexeme::Op("<"),
            '>' => Lexeme::Op(">"),
            '(' => Lexeme::Open,
            ')' => Lexeme::Close,
            ';' => Lexeme::Semicolon,
            '.' => Lexeme::Dot,
            ',' => Lexeme::Comma,
            _ if match_ahead(&chars, at, "${", |next| next != '{') => {
                return Err("unresolved text variable".to_string())
            }
            _ => return Err(format!("unrecognized character '{c}'")),
        });
        at += 1;
    }
}

#[derive(Debug)]
enum Node {
    Number(Option<usize>),
    /// Unary minus, which KiCad compiles as `0 - x`.
    Negate(Box<Node>),
    Not(Box<Node>),
    Str,
    Ident(String),
    Call(String, Vec<Node>),
    Member(Box<Node>, Box<Node>),
    Binary(&'static str, Box<Node>, Box<Node>),
}

/// The precedence `grammar.lemon` gives a binary operator, lowest first, and
/// whether it is non-associative.
fn binary(lexeme: &Lexeme) -> Option<(&'static str, u8, bool)> {
    Some(match lexeme {
        Lexeme::Comma => (",", 1, false),
        Lexeme::Op("&&") => ("&&", 2, false),
        Lexeme::Op("||") => ("||", 3, false),
        Lexeme::Op(op @ ("<" | ">" | "<=" | ">=" | "==" | "!=")) => (op, 5, true),
        Lexeme::Op(op @ ("+" | "-")) => (op, 7, false),
        Lexeme::Op(op @ ("*" | "/")) => (op, 8, false),
        Lexeme::Dot => (".", 9, true),
        _ => return None,
    })
}

const NOT_BINDS: u8 = 6;
const SIGN_BINDS: u8 = 7;

struct ExpressionParser {
    lexemes: Vec<Lexeme>,
    at: usize,
}

impl ExpressionParser {
    fn peek(&self) -> &Lexeme {
        self.lexemes.get(self.at).unwrap_or(&Lexeme::End)
    }

    fn take(&mut self) -> Lexeme {
        let lexeme = self.peek().clone();
        self.at += 1;
        lexeme
    }

    fn expression(&mut self, lowest: u8) -> Result<Node, String> {
        let mut left = self.prefix()?;
        while let Some((op, binds, non_associative)) = binary(self.peek()) {
            if binds < lowest {
                break;
            }
            self.at += 1;
            let right = self.expression(binds + 1)?;
            left = match op {
                "." => Node::Member(Box::new(left), Box::new(right)),
                _ => Node::Binary(op, Box::new(left), Box::new(right)),
            };
            if non_associative && binary(self.peek()).is_some_and(|(_, next, _)| next == binds) {
                return Err("syntax error".to_string());
            }
        }
        Ok(left)
    }

    fn prefix(&mut self) -> Result<Node, String> {
        Ok(match self.take() {
            Lexeme::Op("!") => Node::Not(Box::new(self.expression(NOT_BINDS + 1)?)),
            Lexeme::Op("-") => Node::Negate(Box::new(self.expression(SIGN_BINDS + 1)?)),
            Lexeme::Op("+") => self.expression(SIGN_BINDS + 1)?,
            Lexeme::Value => match self.peek() {
                Lexeme::Unit(unit) => {
                    let unit = *unit;
                    self.at += 1;
                    Node::Number(Some(unit))
                }
                _ => Node::Number(None),
            },
            Lexeme::Str => Node::Str,
            Lexeme::Ident(name) if *self.peek() == Lexeme::Open => {
                self.at += 1;
                let mut arguments = Vec::new();
                if *self.peek() == Lexeme::Close {
                    self.at += 1;
                } else {
                    // `squashParamList`: the left spine of a comma list.
                    let mut list = self.expression(1)?;
                    while let Node::Binary(",", rest, last) = list {
                        arguments.push(*last);
                        list = *rest;
                    }
                    arguments.push(list);
                    arguments.reverse();
                    if self.take() != Lexeme::Close {
                        return Err("syntax error".to_string());
                    }
                }
                Node::Call(name, arguments)
            }
            Lexeme::Ident(name) => Node::Ident(name),
            Lexeme::Open => {
                let inner = self.expression(1)?;
                if self.take() != Lexeme::Close {
                    return Err("syntax error".to_string());
                }
                inner
            }
            _ => return Err("syntax error".to_string()),
        })
    }
}

/// `PCBEXPR_UCODE::CreateVarRef`'s items.
fn is_item(name: &str) -> bool {
    name.eq_ignore_ascii_case("null") || matches!(name, "A" | "B" | "AB" | "L")
}

/// What `COMPILER::generateUCode` reports, short of property names.
struct Codegen<'a> {
    numbers: usize,
    bare_number: bool,
    first_unit: Option<usize>,
    properties: &'a mut BTreeSet<String>,
}

impl Codegen<'_> {
    fn visit(&mut self, node: &Node) -> Result<(), String> {
        match node {
            Node::Number(unit) => {
                self.numbers += 1;
                match unit {
                    Some(unit) => {
                        self.first_unit.get_or_insert(*unit);
                    }
                    None => self.bare_number = true,
                }
                Ok(())
            }
            Node::Negate(inner) => {
                self.numbers += 1;
                self.visit(inner)
            }
            Node::Not(inner) => self.visit(inner),
            Node::Str => Ok(()),
            Node::Ident(name) if is_item(name) => Ok(()),
            Node::Ident(name) => Err(format!(
                "'{name}' is not an item; KiCad's are A, B, AB, L and null"
            )),
            Node::Call(name, _) => Err(format!("{name}() is called on no item, as in A.{name}()")),
            Node::Binary(_, left, right) => {
                self.visit(left)?;
                self.visit(right)
            }
            Node::Member(item, field) => {
                let Node::Ident(item) = &**item else {
                    return Err("a property or function must follow an item, as in A.Type".into());
                };
                match &**field {
                    Node::Ident(property) => {
                        let item_field = ITEM_FIELDS
                            .iter()
                            .any(|field| field.eq_ignore_ascii_case(property));
                        if item.eq_ignore_ascii_case("null") {
                            Ok(())
                        } else if item_field && is_item(item) {
                            if matches!(item.as_str(), "A" | "B") {
                                Ok(())
                            } else {
                                Err(format!("'{item}.{property}' is read for A and B only"))
                            }
                        } else if is_item(item) {
                            self.properties.insert(property.clone());
                            Ok(())
                        } else {
                            Err(format!(
                                "'{item}' is not an item; KiCad's are A, B, AB, L and null"
                            ))
                        }
                    }
                    Node::Call(function, arguments) => {
                        if !is_item(item) {
                            return Err(format!(
                                "'{item}' is not an item; KiCad's are A, B, AB, L and null"
                            ));
                        }
                        let Some((_, takes)) = FUNCTIONS
                            .iter()
                            .find(|(name, _)| name.eq_ignore_ascii_case(function))
                        else {
                            return Err(format!("'{function}' is not a function KiCad 10 knows"));
                        };
                        if arguments.len() < *takes {
                            return Err(format!(
                                "{function}() takes {takes} argument{}",
                                if *takes == 1 { "" } else { "s" }
                            ));
                        }
                        arguments
                            .iter()
                            .try_for_each(|argument| self.visit(argument))
                    }
                    _ => Err(format!(
                        "only a property or a function can follow '{item}.'"
                    )),
                }
            }
        }
    }
}

/// Check one condition, assertion or value as KiCad's compiler reads it, and
/// say what it measures.
fn check_expression(
    text: &str,
    resolver: Resolver,
    properties: &mut BTreeSet<String>,
) -> Result<Quantity, String> {
    let lexemes = lex_expression(text, resolver)?;
    if lexemes.first() == Some(&Lexeme::End) {
        return Ok(Quantity::Unitless);
    }
    let mut parser = ExpressionParser { lexemes, at: 0 };
    let tree = parser.expression(1)?;
    if *parser.peek() != Lexeme::End {
        return Err("syntax error".to_string());
    }
    let mut codegen = Codegen {
        numbers: 0,
        bare_number: false,
        first_unit: None,
        properties,
    };
    codegen.visit(&tree)?;
    // KiCad flags a lone number without units, which it would read in nm.
    if resolver == Resolver::Pcb && codegen.numbers == 1 && codegen.bare_number {
        return Err("the number needs units: mm, in, mil, deg, fs or ps".to_string());
    }
    Ok(match codegen.first_unit.map(|unit| PCB_UNITS[unit]) {
        Some("mil" | "mm" | "in") => Quantity::Distance,
        Some("fs" | "ps") => Quantity::Time,
        _ => Quantity::Unitless,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mcp::handler::McpHandler;
    use crate::tools::{write_hook, ServerConfig};

    /// Rules of our own in the forms KiCad 10's parser reads: comment lines,
    /// unquoted and escaped names, a renamed constraint, the types that take
    /// other arguments, a time, a wildcard and a user layer name.
    const RULES: &str = r#"(version 1)

# Fan-out under the BGA.
(rule "fanout"
	(condition "A.hasNetclass('Fanout') && A.intersectsArea('bga')")
	(constraint track_width (min 0.1mm) (opt 0.15mm) (max 0.2mm))
	(constraint diff_pair_gap (min 0.1mm) (opt 0.15mm)))

(rule keepout
  (layer "*.Cu")
  (condition "A.Type == 'Via' && A.memberOfFootprint('J3')")
  (constraint disallow track via)
  (constraint hole (min 0.3mm)))

(rule "slot \"edge\""
  (layer "F.Silkscreen")
  (constraint edge_clearance (min 0.4mm))
  (constraint zone_connection solid)
  (constraint min_resolved_spokes 2)
  (constraint assertion "A.Width > 0.1mm")
  (constraint skew (max 5ps) (within_diff_pairs))
  (constraint track_angle (min 45))
  (severity exclusion))
"#;

    const KEEPOUT: &str = r#"(rule keepout
  (layer "*.Cu")
  (condition "A.Type == 'Via' && A.memberOfFootprint('J3')")
  (constraint disallow track via)
  (constraint hole (min 0.3mm)))"#;

    /// A real KiCad 10 board (`specctra_two_resistors.kicad_pcb`, see its
    /// README), whose layer table names F.SilkS "F.Silkscreen", with `rules`
    /// as its `.kicad_dru` when given.
    fn board_with(rules: Option<&str>) -> (tempfile::TempDir, std::path::PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let board = dir.path().join("board.kicad_pcb");
        std::fs::copy(
            Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("tests/fixtures/specctra_two_resistors.kicad_pcb"),
            &board,
        )
        .unwrap();
        if let Some(rules) = rules {
            std::fs::write(board.with_extension("kicad_dru"), rules).unwrap();
        }
        (dir, board)
    }

    fn rules_of(board: &Path) -> Option<String> {
        std::fs::read_to_string(board.with_extension("kicad_dru")).ok()
    }

    /// A DRC report in which the probe rule fails, as kicad-cli writes one
    /// when it loaded the rules, trimmed to the fields the parser reads. No
    /// character in it is special to `cmd.exe`'s `echo`.
    const PROBE_HIT: &str = r#"{"violations":[{"type":"assertion_failure","severity":"error","description":"Assertion failure rule copperpour_probe","items":[]}]}"#;

    /// What kicad-cli reports when it dropped the rules: no probe failure.
    const PROBE_MISS: &str = r#"{"violations":[]}"#;

    /// A kicad-cli that writes `report` to its `--output` path, `$4` in
    /// `drc_args`, and exits with `exit_code`.
    fn fake_kicad_cli(dir: &Path, report: Option<&str>, exit_code: i32) -> std::path::PathBuf {
        let unix_report = report
            .map(|contents| format!("printf '%s' '{contents}' > \"$4\"\n"))
            .unwrap_or_default();
        let windows_report = report
            .map(|contents| format!("> \"%~4\" echo {contents}\r\n"))
            .unwrap_or_default();
        crate::tools::cli::test_support::write_script(
            dir,
            "kicad-cli",
            &format!("#!/bin/sh\n{unix_report}exit {exit_code}\n"),
            &format!("@echo off\r\n{windows_report}exit /b {exit_code}\r\n"),
        )
    }

    /// The tool, called with a kicad-cli that loads the rules.
    async fn call(arguments: serde_json::Value) -> serde_json::Value {
        let dir = tempfile::tempdir().unwrap();
        let cli = fake_kicad_cli(dir.path(), Some(PROBE_HIT), 0);
        call_with(&cli.display().to_string(), arguments).await
    }

    async fn call_with(kicad_cli: &str, arguments: serde_json::Value) -> serde_json::Value {
        let handler = McpHandler::new(ServerConfig {
            eager_toolsets: true,
            kicad_cli: kicad_cli.to_string(),
            ..Default::default()
        })
        .await
        .unwrap();
        let response = handler
            .handle_message(json!({
                "jsonrpc": "2.0", "id": 108, "method": "tools/call",
                "params": {"name": "set_custom_rule", "arguments": arguments}
            }))
            .await
            .unwrap()
            .result
            .unwrap();
        let mut body: serde_json::Value =
            serde_json::from_str(response["content"][0]["text"].as_str().unwrap()).unwrap();
        body["is_error"] = response["isError"].clone();
        body
    }

    fn dp_pairs(board: &Path) -> serde_json::Value {
        json!({
            "board": board, "name": "dp",
            "condition": "A.hasNetclass('DP_Diff')",
            "constraints": [{"type": "track_width", "min": 0.254, "opt": 0.254, "max": 0.254}]
        })
    }

    const DP_PAIRS: &str = "(rule \"dp\"\n  (condition \"A.hasNetclass('DP_Diff')\")\n  \
                            (constraint track_width (min 0.254mm) (opt 0.254mm) (max 0.254mm)))";

    #[test]
    fn every_form_kicad_reads_here_passes() {
        let (_dir, board) = board_with(None);
        let checked = check_rules(RULES, &read_board(&board).unwrap().user_layers);
        assert_eq!(checked.faults, Vec::<String>::new());
        assert_eq!(
            checked
                .rules
                .iter()
                .map(|rule| rule.name.as_str())
                .collect::<Vec<_>>(),
            ["fanout", "keepout", "slot \"edge\""]
        );
        assert_eq!(
            &RULES[checked.rules[1].start..checked.rules[1].end],
            KEEPOUT
        );
        assert_eq!(checked.properties, BTreeSet::from(["Width".to_string()]));
    }

    #[tokio::test]
    async fn a_new_rule_goes_last_and_the_rest_stays_as_written() {
        let (_dir, board) = board_with(Some(RULES));
        let body = call(dp_pairs(&board)).await;
        assert_ne!(body["is_error"], true, "{body}");
        assert_eq!(
            rules_of(&board).unwrap(),
            format!("{}\n\n{DP_PAIRS}\n", RULES.trim_end())
        );
        assert_eq!(
            (
                &body["created"],
                &body["changed"],
                &body["previous"],
                &body["current"]
            ),
            (&json!(false), &json!(true), &json!(null), &json!(DP_PAIRS))
        );
        // kicad-cli loaded the file, `Width` in it included.
        assert_eq!(
            (&body["kicad_checked"], &body["unchecked_properties"]),
            (&json!(true), &json!([]))
        );
    }

    #[tokio::test]
    async fn a_rule_of_the_same_name_is_replaced_where_it_stands() {
        let (_dir, board) = board_with(Some(RULES));
        let body = call(json!({
            "board": board, "name": "keepout", "layer": "outer", "severity": "warning",
            "constraints": [{"type": "clearance", "min": 0.5}, {"type": "via_count", "max": 2}]
        }))
        .await;
        assert_ne!(body["is_error"], true, "{body}");
        let replaced =
            "(rule \"keepout\"\n  (layer outer)\n  (constraint clearance (min 0.5mm))\n  \
                        (constraint via_count (max 2))\n  (severity warning))";
        assert_eq!(rules_of(&board).unwrap(), RULES.replace(KEEPOUT, replaced));
        assert_eq!(
            (&body["previous"], &body["current"]),
            (&json!(KEEPOUT), &json!(replaced))
        );
    }

    #[tokio::test]
    async fn a_missing_file_is_created_with_its_version() {
        let (_dir, board) = board_with(None);
        let body = call(dp_pairs(&board)).await;
        assert_ne!(body["is_error"], true, "{body}");
        assert_eq!(
            rules_of(&board).unwrap(),
            format!("(version 1)\n\n{DP_PAIRS}\n")
        );
        assert_eq!(
            (&body["created"], &body["changed"]),
            (&json!(true), &json!(true))
        );
    }

    #[tokio::test]
    async fn the_same_rule_again_writes_nothing() {
        let (_dir, board) = board_with(Some(RULES));
        call(dp_pairs(&board)).await;
        let written = rules_of(&board).unwrap();
        let body = call(dp_pairs(&board)).await;
        assert_eq!(
            (&body["changed"], &body["previous"]),
            (&json!(false), &json!(DP_PAIRS))
        );
        assert_eq!(rules_of(&board).unwrap(), written);
    }

    /// Each fault refuses the call and leaves the file as it was, whether it
    /// is in the rule asked for or in one already there. The first two are
    /// the rules #108 measured disabling every custom rule in `kicad-cli`.
    #[tokio::test]
    async fn a_fault_in_any_rule_refuses_and_writes_nothing() {
        let unclosed = "\n(rule \"unclosed\"\n  (constraint clearance (min 20mm))\n";
        for (added, arguments, says) in [
            (unclosed, json!({}), "never closed"),
            (
                "\n(rule \"odd\" (constraint no_such_thing (min 1mm)))\n",
                json!({}),
                "'no_such_thing' is not a constraint type",
            ),
            (
                "\n(rule \"s\" (constraint clearance (min 1mm)) (severity fatal))\n",
                json!({}),
                "'fatal' is not a severity",
            ),
            (
                "\n(rule \"u\" (constraint clearance (min 0.2)))\n",
                json!({}),
                "needs units",
            ),
            (
                "\n(rule \"t\" (constraint clearance (min 10ps)))\n",
                json!({}),
                "is a time",
            ),
            (
                "\n(rule \"v\" (constraint via_count (max 2mm)))\n",
                json!({}),
                "syntax error",
            ),
            (
                "\n(rule \"c\" # a note\n  (constraint clearance (min 1mm)))\n",
                json!({}),
                "expected '(' before '#'",
            ),
            (
                "",
                json!({"condition": "A.hasNetclass('DP'"}),
                "syntax error",
            ),
            (
                "",
                json!({"condition": "A.hasNetClas('DP')"}),
                "'hasNetClas' is not a function",
            ),
            ("", json!({"layer": "Top"}), "'Top' is not a layer name"),
            ("", json!({"layer": "F.Cuu"}), "'F.Cuu' is not a layer name"),
            (
                "",
                json!({"condition": "A.NetName == '${NET}'"}),
                "holds a text variable",
            ),
            (
                "\n(rule \"twice\" (constraint clearance (min 1mm)) \
                 (constraint clearance (min 2mm)))\n",
                json!({}),
                "second 'clearance' constraint",
            ),
            (
                "\n(rule \"layers\" (layer outer) (layer inner) \
                 (constraint clearance (min 1mm)))\n",
                json!({}),
                "second layer clause",
            ),
        ] {
            let before = format!("{RULES}{added}");
            let (_dir, board) = board_with(Some(&before));
            let mut request = dp_pairs(&board);
            for (key, value) in arguments.as_object().unwrap() {
                request[key] = value.clone();
            }
            let body = call(request).await;
            assert_eq!(body["error"]["kind"], "plan_blocked", "{says}: {body}");
            assert!(
                body["error"]["reasons"].to_string().contains(says),
                "{says}: {body}"
            );
            assert_eq!(rules_of(&board).unwrap(), before, "{says}");
        }
    }

    #[tokio::test]
    async fn replacing_a_faulty_rule_mends_the_file() {
        let faulty = "(rule \"dp\" (constraint track_width (min 0.254)))";
        let (_dir, board) = board_with(Some(&format!("{RULES}\n{faulty}\n")));
        let body = call(dp_pairs(&board)).await;
        assert_ne!(body["is_error"], true, "{body}");
        assert_eq!(body["previous"], json!(faulty));
        assert_eq!(rules_of(&board).unwrap(), format!("{RULES}\n{DP_PAIRS}\n"));
    }

    #[tokio::test]
    async fn two_rules_of_the_name_are_not_guessed_between() {
        let twice = format!("{RULES}\n{DP_PAIRS}\n\n{DP_PAIRS}\n");
        let (_dir, board) = board_with(Some(&twice));
        let body = call(dp_pairs(&board)).await;
        assert_eq!(body["error"]["kind"], "ambiguous_target", "{body}");
        assert_eq!(body["error"]["candidates"], json!(["line 25", "line 29"]));
        assert_eq!(rules_of(&board).unwrap(), twice);
    }

    #[tokio::test]
    async fn a_file_saved_after_the_read_is_left_as_saved() {
        let (_dir, board) = board_with(Some(RULES));
        let saved = format!("{RULES}\n(rule \"theirs\" (constraint clearance (min 1mm)))\n");
        write_hook::set(saved.clone());
        let body = call(dp_pairs(&board)).await;
        assert!(!write_hook::armed());
        assert_eq!(body["error"]["kind"], "conflict", "{body}");
        assert_eq!(rules_of(&board).unwrap(), saved);
    }

    /// No file at the read, one there at the write: the create leaves it.
    #[tokio::test]
    async fn a_file_created_after_the_read_is_left_as_created() {
        let (_dir, board) = board_with(None);
        let saved = "(version 1)\n(rule \"theirs\" (constraint clearance (min 1mm)))\n";
        write_hook::set(saved.to_string());
        let body = call(dp_pairs(&board)).await;
        assert!(!write_hook::armed());
        assert_eq!(body["error"]["kind"], "conflict", "{body}");
        assert_eq!(rules_of(&board).unwrap(), saved);
    }

    #[tokio::test]
    async fn a_constraint_type_with_other_arguments_is_refused_by_the_schema() {
        let (_dir, board) = board_with(Some(RULES));
        let mut request = dp_pairs(&board);
        request["constraints"] = json!([{"type": "disallow"}]);
        let body = call(request).await;
        assert_eq!(body["error"]["kind"], "invalid_argument", "{body}");
        assert_eq!(rules_of(&board).unwrap(), RULES);
    }

    #[tokio::test]
    async fn a_name_on_two_lines_is_refused_as_an_argument() {
        let (_dir, board) = board_with(Some(RULES));
        let mut request = dp_pairs(&board);
        request["name"] = json!("dp\npairs");
        let body = call(request).await;
        assert_eq!(body["error"]["kind"], "invalid_argument", "{body}");
        assert!(body.to_string().contains("on one line"), "{body}");
        assert_eq!(rules_of(&board).unwrap(), RULES);
    }

    /// The copied check passes the file, kicad-cli does not load it: the
    /// write is refused, naming the property names only KiCad could check.
    #[tokio::test]
    async fn a_file_kicad_cli_does_not_load_is_refused() {
        let (_dir, board) = board_with(Some(RULES));
        let cli_dir = tempfile::tempdir().unwrap();
        let cli = fake_kicad_cli(cli_dir.path(), Some(PROBE_MISS), 0);
        let body = call_with(&cli.display().to_string(), dp_pairs(&board)).await;
        assert_eq!(body["error"]["kind"], "plan_blocked", "{body}");
        let reasons = body["error"]["reasons"].to_string();
        assert!(reasons.contains("did not load the file"), "{body}");
        assert!(reasons.contains("'Width'"), "{body}");
        assert_eq!(rules_of(&board).unwrap(), RULES);
    }

    #[tokio::test]
    async fn a_kicad_cli_that_fails_refuses_the_write() {
        let (_dir, board) = board_with(Some(RULES));
        let cli_dir = tempfile::tempdir().unwrap();
        let cli = fake_kicad_cli(cli_dir.path(), None, 3);
        let body = call_with(&cli.display().to_string(), dp_pairs(&board)).await;
        assert_eq!(body["error"]["kind"], "plan_blocked", "{body}");
        assert!(
            body["error"]["reasons"]
                .to_string()
                .contains("could not check the file"),
            "{body}"
        );
        assert_eq!(rules_of(&board).unwrap(), RULES);
    }

    /// With no item on the board the probe rule has nothing to fail on, so a
    /// file kicad-cli loaded would look like one it dropped. The copied check
    /// alone decides, and the reply names what it left unchecked. kicad-cli is
    /// not run: this one would fail if it were.
    #[tokio::test]
    async fn a_board_with_no_item_is_written_on_the_check_alone() {
        let dir = tempfile::tempdir().unwrap();
        let board = dir.path().join("empty.kicad_pcb");
        std::fs::write(
            &board,
            "(kicad_pcb\n\t(version 20241229)\n\t(generator \"pcbnew\")\n\t(general\n\t\t(thickness 1.6)\n\t)\n\t(paper \"A4\")\n\t(layers\n\t\t(0 \"F.Cu\" signal)\n\t\t(2 \"B.Cu\" signal)\n\t)\n\t(setup)\n)\n",
        )
        .unwrap();
        let cli = fake_kicad_cli(dir.path(), None, 9);
        let mut request = dp_pairs(&board);
        request["condition"] = json!("A.Width > 0.1mm");
        let body = call_with(&cli.display().to_string(), request).await;
        assert_ne!(body["is_error"], true, "{body}");
        assert_eq!(
            (&body["kicad_checked"], &body["unchecked_properties"]),
            (&json!(false), &json!(["Width"]))
        );
        assert!(rules_of(&board).unwrap().contains("A.Width > 0.1mm"));
    }

    /// Live, against a real kicad-cli on the fixture board: a known property
    /// is written, an unknown one, which only KiCad can judge, is refused.
    ///
    ///     KICAD_CLI=/usr/bin/kicad-cli \
    ///     cargo test -p copperpour-core --lib custom_rules::tests::live -- --ignored
    #[tokio::test]
    #[ignore = "needs a real kicad-cli"]
    async fn live_kicad_cli_judges_property_names() {
        let cli = std::env::var("KICAD_CLI").unwrap_or_else(|_| "kicad-cli".to_string());
        let (_dir, board) = board_with(None);
        let mut request = dp_pairs(&board);
        request["condition"] = json!("A.Width > 0.1mm");
        let body = call_with(&cli, request.clone()).await;
        assert_ne!(body["is_error"], true, "{body}");
        let written = rules_of(&board).unwrap();
        request["condition"] = json!("A.No_Such_Property == 'x'");
        let body = call_with(&cli, request).await;
        assert_eq!(body["error"]["kind"], "plan_blocked", "{body}");
        assert!(
            body["error"]["reasons"]
                .to_string()
                .contains("'No_Such_Property'"),
            "{body}"
        );
        assert_eq!(rules_of(&board).unwrap(), written);
    }

    #[test]
    fn expressions_are_read_as_kicads_compiler_reads_them() {
        for accepted in [
            "",
            "A.NetClass == 'HV'",
            "A.Type == 'Via' && !A.isPlated()",
            "A.Width > 0.2mm",
            "(A.Width + 1mm) * 2 < 3mm",
            "-0.1mm",
            "A.fromTo('U1-1', 'U2-1')",
            "B.memberOfSheet('/a/') || A.existsOnLayer('F.Cu')",
            "A.hasnetclass('x')",
            "AB.Foo == 1mm",
            "null.Foo == 1mm",
            "A.Name == 'unclosed",
        ] {
            let result = check_expression(accepted, Resolver::Pcb, &mut BTreeSet::new());
            assert!(result.is_ok(), "{accepted}: {result:?}");
        }
        for (refused, says) in [
            ("A.Width < 1mm < 2mm", "syntax error"),
            ("A.b.c == 'x'", "syntax error"),
            ("A.Type == 'Via';", "syntax error"),
            ("A.Width > 0.2", "needs units"),
            ("isPlated()", "called on no item"),
            ("C.Type == 'Via'", "'C' is not an item"),
            ("L.NetClass == 'x'", "A and B only"),
            ("A.fromTo('U1')", "takes 2 arguments"),
            ("A.noSuch('x')", "'noSuch' is not a function"),
            ("'x'.Type == 'y'", "must follow an item"),
            ("A.'x' == 'y'", "only a property or a function"),
            ("A.Width\t> 1mm", "unrecognized character"),
            ("A.Type = 'Via'", "unrecognized character '='"),
            ("A.Name == '${X}'", ""),
            ("${X}", "text variable"),
        ] {
            let result = check_expression(refused, Resolver::Pcb, &mut BTreeSet::new());
            match says {
                // Inside a string KiCad's compiler takes `${` as text.
                "" => assert!(result.is_ok(), "{refused}: {result:?}"),
                _ => assert!(
                    result.as_ref().is_err_and(|fault| fault.contains(says)),
                    "{refused}: {result:?}"
                ),
            }
        }
        let mut none = BTreeSet::new();
        assert_eq!(
            check_expression("2", Resolver::Unitless, &mut none),
            Ok(Quantity::Unitless)
        );
        assert!(check_expression("2mm", Resolver::Unitless, &mut none).is_err());
        assert_eq!(
            check_expression("5ps", Resolver::Pcb, &mut none),
            Ok(Quantity::Time)
        );
    }
}
