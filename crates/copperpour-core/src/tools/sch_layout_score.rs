//! Compare two snapshots of one saved sheet, independent of the editor.

use super::sch_analysis::{
    orphan_findings, overlap_findings, pin_text_findings, ORPHAN_TOLERANCE, OVERLAP_TOLERANCE,
};
use super::sch_block_move::{snapshot_net_changes, NetComparison};
use super::sch_connectivity::{drawn_net_graph_for, ConnectivityIndex, COINCIDENT_TOLERANCE};
use super::sch_pin_contacts::default_contact_counts;
use super::{get_path, invalid_arg, opt_str_list, resolved_placed_pins_by_reference, ToolContext};
use crate::mcp::{error::ToolErrorKind, protocol::CallToolResult};
use copperpour_sexp::geometry::points_coincident;
use copperpour_sexp::schematic::{
    extract_all_net_labels, extract_buses, extract_labels, extract_wires, find_lib_symbol,
    symbol_instance, LabelKind, Wire,
};
use copperpour_sexp::{parse_sexp, SexpNode};
use serde_json::{json, Value};
use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::path::Path;

#[path = "sch_layout_library.rs"]
mod library;
#[path = "sch_layout_properties.rs"]
mod properties;

#[cfg(test)]
#[path = "sch_layout_score_tests.rs"]
mod tests;

const TIERS: [&[&str]; 4] = [
    &[
        "component_overlap",
        "label_overlap",
        "wire_overlap",
        "dangling_wire_end",
        "floating_label",
        "unconnected_pin",
    ],
    &[
        "text_overlap_text",
        "text_overlap_body",
        "wire_field_overlap",
        "wire_pin_text_overlap",
        "pin_contact",
        "short_pin_wire",
    ],
    &[
        "diagonal_wires",
        "collinear_overlaps",
        "wire_bends",
        "split_runs",
        "redundant_power_symbols",
    ],
    &["wire_length_mm"],
];

const LENGTH: &str = "wire_length_mm";

// Store length in micrometres so comparison and the displayed delta agree.
type Terms = BTreeMap<&'static str, Option<i64>>;

fn terms() -> impl Iterator<Item = &'static str> {
    TIERS.into_iter().flatten().copied()
}

fn term_json(key: &str, number: Option<i64>) -> Value {
    match number {
        Some(n) if key == LENGTH => json!(n as f64 / 1000.0),
        _ => json!(number),
    }
}

pub(super) struct Score {
    terms: Terms,
    bounds_unresolved: Vec<String>,
    pin_text_unresolved: Vec<Value>,
    wire_segments: usize,
    power_symbols: Option<usize>,
}

impl Score {
    pub(super) fn json(&self) -> Value {
        let mut result = json!({
            "bounds_unresolved": self.bounds_unresolved,
            "pin_text_unresolved": self.pin_text_unresolved,
            "wire_segments": self.wire_segments,
            "power_symbols": self.power_symbols,
        });
        for (&key, &number) in &self.terms {
            result[key] = term_json(key, number);
        }
        result
    }
}

fn increment(score: &mut Terms, key: &str) {
    if let Some(Some(count)) = score.get_mut(key) {
        *count += 1;
    }
}

pub(super) fn measure(tree: &SexpNode) -> Score {
    measure_with_stub_exclusions(tree, &BTreeSet::new())
}

fn measure_with_stub_exclusions(tree: &SexpNode, required_stubs: &BTreeSet<String>) -> Score {
    let wires = extract_wires(tree);
    let buses = extract_buses(tree);
    let labels = extract_all_net_labels(tree);
    let index = ConnectivityIndex::build(tree, &wires, &buses, &labels, ORPHAN_TOLERANCE);
    let pins = resolved_placed_pins_by_reference(tree);
    let (mut overlaps, _, bounds_unresolved) = overlap_findings(tree, &wires, OVERLAP_TOLERANCE);
    let pin_text = pin_text_findings(tree, &wires);
    overlaps.extend(pin_text.findings);

    let mut score: Terms = terms().map(|key| (key, Some(0))).collect();
    let contact_counts = default_contact_counts(tree, &wires);
    score.insert(
        "pin_contact",
        contact_counts.map(|(contacts, _)| contacts as i64),
    );
    score.insert(
        "short_pin_wire",
        contact_counts.map(|(_, short)| short as i64),
    );
    if !bounds_unresolved.is_empty() {
        for key in ["component_overlap", "wire_overlap", "text_overlap_body"] {
            score.insert(key, None);
        }
    }
    // Pin text that could not be measured leaves only its own term unknown:
    // the body terms rest on the symbol drawings, which it does not change.
    if !pin_text.unresolved.is_empty() {
        score.insert("wire_pin_text_overlap", None);
    }
    if pins.is_none() {
        for key in [
            "dangling_wire_end",
            "floating_label",
            "unconnected_pin",
            "wire_bends",
            "split_runs",
            "redundant_power_symbols",
        ] {
            score.insert(key, None);
        }
    }
    for finding in overlaps.iter().chain(&orphan_findings(&index)) {
        if let Some(kind) = finding["type"].as_str() {
            let key = match kind {
                "text_overlap" if finding["b"]["kind"] == "symbol_body" => "text_overlap_body",
                "text_overlap" => "text_overlap_text",
                other => other,
            };
            increment(&mut score, key);
        }
    }

    let length: f64 = wires
        .iter()
        .filter(|wire| {
            !wire
                .uuid
                .as_ref()
                .is_some_and(|uuid| required_stubs.contains(uuid))
        })
        .map(|wire| (wire.x2 - wire.x1).hypot(wire.y2 - wire.y1))
        .sum();
    score.insert(LENGTH, Some((length * 1000.0).round() as i64));
    for (i, a) in wires.iter().enumerate() {
        if (a.x2 - a.x1).abs() > 1e-6 && (a.y2 - a.y1).abs() > 1e-6 {
            increment(&mut score, "diagonal_wires");
        }
        for b in &wires[i + 1..] {
            if collinear_overlap(a, b) {
                increment(&mut score, "collinear_overlaps");
            }
        }
    }
    if pins.is_some() {
        let junction_index =
            ConnectivityIndex::build(tree, &wires, &buses, &labels, COINCIDENT_TOLERANCE);
        count_wire_joints(&mut score, &wires, &junction_index);
        score.insert(
            "redundant_power_symbols",
            Some(redundant_power_symbols(
                tree,
                &wires,
                &labels,
                &junction_index,
            )),
        );
    }

    let lib_symbols = tree
        .find("lib_symbols")
        .map(|node| node.find_all("symbol"))
        .unwrap_or_default();
    let power_symbols = pins.map(|instances| {
        instances
            .iter()
            .filter(|(inst, _)| {
                find_lib_symbol(&lib_symbols, inst).is_some_and(|lib| lib.find("power").is_some())
            })
            .count()
    });
    Score {
        terms: score,
        bounds_unresolved,
        pin_text_unresolved: pin_text.unresolved,
        wire_segments: wires.len(),
        power_symbols,
    }
}

fn ends(wire: &Wire) -> [((f64, f64), (f64, f64)); 2] {
    let (a, b) = ((wire.x1, wire.y1), (wire.x2, wire.y2));
    [(a, b), (b, a)]
}

pub(crate) fn collinear_overlap(a: &Wire, b: &Wire) -> bool {
    let a_len = (a.x2 - a.x1).hypot(a.y2 - a.y1);
    let b_len = (b.x2 - b.x1).hypot(b.y2 - b.y1);
    // Both axes must agree: a result must not depend on record order.
    a_len > 1e-4 && b_len > 1e-4 && projects_over(a, b, a_len) && projects_over(b, a, b_len)
}

fn projects_over(a: &Wire, b: &Wire, length: f64) -> bool {
    let (dx, dy) = (a.x2 - a.x1, a.y2 - a.y1);
    let tol = 1e-4;
    let (ux, uy) = (dx / length, dy / length);
    let project = |x: f64, y: f64| {
        let (x, y) = (x - a.x1, y - a.y1);
        ((x * uy - y * ux).abs() <= tol).then_some(x * ux + y * uy)
    };
    let (Some(t1), Some(t2)) = (project(b.x1, b.y1), project(b.x2, b.y2)) else {
        return false;
    };
    t1.max(t2).min(length) - t1.min(t2).max(0.0) > tol
}

fn count_wire_joints(score: &mut Terms, wires: &[Wire], index: &ConnectivityIndex<'_>) {
    for (i, wire) in wires.iter().enumerate() {
        for ((x, y), (ax, ay)) in ends(wire) {
            if index.wire_ends_at(x, y) != 2
                || index.has_pin(x, y)
                || index.has_sheet_pin(x, y)
                || index.has_label(x, y)
                || index.has_junction(x, y)
                || index.has_no_connect(x, y)
                || index.on_bus(x, y)
                || index.has_bus_entry_wire_end(x, y)
                || index.on_wire_interior(x, y)
            {
                continue;
            }
            let other = wires[i + 1..]
                .iter()
                .flat_map(ends)
                .find(|&((bx, by), _)| points_coincident(x, y, bx, by, COINCIDENT_TOLERANCE));
            if let Some(((x2, y2), (bx, by))) = other {
                let (dx, dy) = (ax - x, ay - y);
                let (ex, ey) = (bx - x2, by - y2);
                let scale = dx.hypot(dy) * ex.hypot(ey);
                if scale == 0.0 {
                    continue;
                }
                if (dx * ey - ex * dy).abs() <= 1e-6 * scale {
                    // Collinear returns on the same side are stacked, not a split run.
                    if dx * ex + dy * ey < 0.0 {
                        increment(score, "split_runs");
                    }
                } else {
                    increment(score, "wire_bends");
                }
            }
        }
    }
}

fn redundant_power_symbols(
    tree: &SexpNode,
    wires: &[Wire],
    labels: &[copperpour_sexp::schematic::Label],
    index: &ConnectivityIndex<'_>,
) -> i64 {
    let drawn_labels = extract_labels(tree);
    let mut drawn = drawn_net_graph_for(tree, wires, &drawn_labels);
    let mut seen = HashSet::new();
    let mut duplicates = 0;
    let mut counted = HashSet::new();
    for label in labels
        .iter()
        .filter(|label| label.kind == LabelKind::PowerSymbol)
    {
        if (index.has_wire_end(label.x, label.y)
            || (index.on_wire(label.x, label.y) && index.has_junction(label.x, label.y)))
            && counted.insert((
                label.uuid.as_deref(),
                drawn.root_at(label.x, label.y),
                &label.net,
            ))
            && !seen.insert((drawn.root_at(label.x, label.y), &label.net))
        {
            duplicates += 1;
        }
    }
    duplicates
}

fn identities(tree: &SexpNode) -> Option<BTreeMap<&str, &SexpNode>> {
    let mut result = BTreeMap::new();
    for symbol in tree.find_all("symbol") {
        let uuid = symbol.find_str("uuid")?;
        symbol.find_str("lib_id")?;
        if result.insert(uuid, symbol).is_some() {
            return None;
        }
    }
    Some(result)
}

pub(super) fn cleanup_gate(
    before: &SexpNode,
    after: &SexpNode,
    requested: &[String],
) -> Vec<String> {
    gate(before, after, requested, NetComparison::Cleanup)
}

fn gate(
    before: &SexpNode,
    after: &SexpNode,
    requested: &[String],
    comparison: NetComparison,
) -> Vec<String> {
    let (Some(was), Some(now)) = (identities(before), identities(after)) else {
        return vec!["a placed symbol has missing or duplicate identity".to_owned()];
    };
    let lib_symbols = before
        .find("lib_symbols")
        .map(|node| node.find_all("symbol"))
        .unwrap_or_default();
    let mut approved = BTreeSet::new();
    let mut reasons = Vec::new();
    let sources = matches!(comparison, NetComparison::Layout)
        .then(|| properties::Source::comparison(before, after));
    for uuid in requested
        .iter()
        .map(String::as_str)
        .collect::<BTreeSet<_>>()
    {
        let is_power = was
            .get(uuid)
            .and_then(|node| symbol_instance(node))
            .and_then(|inst| find_lib_symbol(&lib_symbols, &inst))
            .is_some_and(|lib| lib.find("power").is_some());
        if is_power && !now.contains_key(uuid) {
            approved.insert(uuid);
        } else {
            reasons.push(format!("{uuid} is not a removed power symbol"));
        }
    }
    for (&uuid, &symbol) in &was {
        match now.get(uuid) {
            Some(kept) => {
                let instance_changed = if sources.is_some() {
                    properties::instance_identity(kept) != properties::instance_identity(symbol)
                } else {
                    kept.find("instances") != symbol.find("instances")
                };
                if kept.find("lib_id") != symbol.find("lib_id") || instance_changed {
                    reasons.push(format!("symbol {uuid} changed its lib_id or instances"));
                }
                if sources.is_some()
                    && properties::reference(kept) != properties::reference(symbol)
                    && !(properties::saved_root_reference(symbol, before).is_some()
                        && properties::saved_root_reference(kept, after).is_some())
                {
                    reasons.push(format!("symbol {uuid} changed its placed Reference"));
                }
                let on_board = |node: &SexpNode| node.find_str("on_board") != Some("no");
                if on_board(kept) != on_board(symbol) {
                    reasons.push(format!("symbol {uuid} changed its PCB membership"));
                }
                if let Some((old, new)) = &sources {
                    let reference = properties::reference(symbol).unwrap_or("?");
                    properties::changes(
                        &format!("symbol {uuid} ({reference})"),
                        (symbol, old),
                        (kept, new),
                        &mut reasons,
                    );
                }
            }
            None if !approved.contains(uuid) => reasons.push(format!("symbol {uuid} was removed")),
            _ => {}
        }
    }
    for uuid in now.keys().filter(|uuid| !was.contains_key(*uuid)) {
        reasons.push(format!("symbol {uuid} was added"));
    }
    if let Some((old, new)) = &sources {
        properties::sheet_changes((before, old), (after, new), &mut reasons);
    }
    reasons.extend(snapshot_net_changes(before, after, &approved, comparison));
    if reasons.is_empty() {
        if let Some((old, new)) = &sources {
            properties::uncertain_changes((old, new), &mut reasons);
            library::append_changes((before, &was), (after, &now), &mut reasons);
        }
    }
    reasons
}

fn verdict(delta: &Terms, reasons: &[String]) -> &'static str {
    if !reasons.is_empty() {
        return "blocked";
    }
    let changes = |tier: &'static [&'static str]| tier.iter().filter_map(|key| delta[key]);
    if TIERS[..2]
        .iter()
        .any(|tier| changes(tier).any(|change| change > 0))
    {
        return "not_improved";
    }
    if TIERS
        .iter()
        .all(|tier| changes(tier).all(|change| change == 0))
    {
        return "unchanged";
    }
    if TIERS
        .iter()
        .map(|tier| changes(tier).sum::<i64>())
        .find(|sum| *sum != 0)
        .is_some_and(|sum| sum < 0)
    {
        "improved"
    } else {
        "not_improved"
    }
}

pub(super) fn cleanup_rank(baseline: &Score, tree: &SexpNode) -> Option<[i64; TIERS.len()]> {
    let delta = measure(tree).delta_from(baseline);
    (verdict(&delta, &[]) == "improved")
        .then(|| TIERS.map(|tier| tier.iter().filter_map(|key| delta[key]).sum()))
}

pub(super) fn layout_verdict(before: &SexpNode, after: &SexpNode) -> &'static str {
    LayoutBaseline::new(before).verdict(after)
}

/// Reuse the served layout comparison's score and gate for multiple trials.
pub(super) struct LayoutBaseline<'a> {
    tree: &'a SexpNode,
    score: Score,
}

impl<'a> LayoutBaseline<'a> {
    pub(super) fn new(tree: &'a SexpNode) -> Self {
        Self {
            tree,
            score: measure(tree),
        }
    }

    pub(super) fn verdict(&self, after: &SexpNode) -> &'static str {
        verdict(
            &measure(after).delta_from(&self.score),
            &gate(self.tree, after, &[], NetComparison::Layout),
        )
    }
}

impl Score {
    fn delta_from(&self, baseline: &Self) -> Terms {
        terms()
            .map(|key| {
                let change = self.terms[key].zip(baseline.terms[key]);
                (key, change.map(|(now, was)| now - was))
            })
            .collect()
    }
}

pub(super) struct LayoutComparison {
    score: Score,
    baseline_score: Option<Score>,
    delta: Terms,
    pub(super) verdict: Option<&'static str>,
    pub(super) reasons: Vec<String>,
}

impl LayoutComparison {
    pub(super) fn json(&self) -> Value {
        let not_compared = terms()
            .filter(|key| self.delta[key].is_none())
            .collect::<Vec<_>>();
        let delta = self.baseline_score.as_ref().map(|_| {
            self.delta
                .iter()
                .map(|(&key, &change)| (key.to_owned(), term_json(key, change)))
                .collect::<Value>()
        });
        json!({
            "score": self.score.json(),
            "baseline_score": self.baseline_score.as_ref().map(Score::json),
            "delta": delta,
            "verdict": self.verdict,
            "reasons": self.reasons,
            "not_compared": not_compared,
        })
    }
}

/// Compare complete in-memory snapshots. Callers may exclude verified mandatory lead
/// lengths from ranking, but those wires still participate in all other checks.
pub(super) fn compare_layout(
    sheet: &SexpNode,
    baseline: Option<&SexpNode>,
    removed: &[String],
    required_stubs: [&BTreeSet<String>; 2],
) -> LayoutComparison {
    let [baseline_stubs, sheet_stubs] = required_stubs;
    let score = measure_with_stub_exclusions(sheet, sheet_stubs);
    let baseline_score = baseline.map(|tree| measure_with_stub_exclusions(tree, baseline_stubs));
    // A single-sheet score reports which terms could be measured.
    let delta = baseline_score
        .as_ref()
        .map_or_else(|| score.terms.clone(), |before| score.delta_from(before));
    let reasons = baseline.map_or_else(Vec::new, |before| {
        gate(before, sheet, removed, NetComparison::Layout)
    });
    let verdict = baseline.map(|_| verdict(&delta, &reasons));
    LayoutComparison {
        score,
        baseline_score,
        delta,
        verdict,
        reasons,
    }
}

pub(super) fn read_saved_sheet(path: &Path) -> Result<(String, SexpNode), CallToolResult> {
    let source = decode_saved_source(path, read_saved_bytes(path)?)?;
    let tree = parse_saved_sheet(path, &source)?;
    Ok((source, tree))
}

fn malformed_sheet(path: &Path, reason: String) -> CallToolResult {
    let message = format!("Cannot read schematic {}: {reason}", path.display());
    CallToolResult::error_kind(
        ToolErrorKind::MalformedFile {
            path: path.display().to_string(),
            reason,
        },
        message,
    )
}

pub(super) fn read_saved_bytes(path: &Path) -> Result<Vec<u8>, CallToolResult> {
    // The writers replace atomically; a single open reads one saved version
    // without creating the cooperative lockfile used by read_schematic.
    std::fs::read(path).map_err(|err| unreadable_sheet(path, err))
}

pub(super) fn unreadable_sheet(path: &Path, error: std::io::Error) -> CallToolResult {
    CallToolResult::error_kind(
        ToolErrorKind::file_not_found(path),
        format!("Cannot read schematic {}: {error}", path.display()),
    )
}

pub(super) fn decode_saved_source(path: &Path, bytes: Vec<u8>) -> Result<String, CallToolResult> {
    String::from_utf8(bytes).map_err(|err| malformed_sheet(path, err.utf8_error().to_string()))
}

pub(super) fn parse_saved_sheet(path: &Path, source: &str) -> Result<SexpNode, CallToolResult> {
    let tree = parse_sexp(source).map_err(|err| malformed_sheet(path, err.to_string()))?;
    if tree.head() != Some("kicad_sch") || tree.find_str("version").is_none() {
        return Err(malformed_sheet(
            path,
            "expected a KiCad schematic sheet".to_owned(),
        ));
    }
    Ok(tree)
}

pub(super) async fn handle_compare_schematic_layout(
    args: &Value,
    _ctx: &ToolContext,
) -> anyhow::Result<CallToolResult> {
    let path = get_path(args, "schematic")?;
    let baseline = if args["baseline"].is_null() {
        None
    } else {
        Some(get_path(args, "baseline")?)
    };
    let removed = match opt_str_list(args, "removed_power_symbol_uuids") {
        Ok(list) => list.unwrap_or_default(),
        Err(error) => return Ok(error),
    };
    if baseline.is_none() && !removed.is_empty() {
        return Ok(invalid_arg(
            "removed_power_symbol_uuids",
            "requires a baseline",
        ));
    }
    let sheet = match read_saved_sheet(&path) {
        Ok((_, sheet)) => sheet,
        Err(error) => return Ok(error),
    };
    let baseline = match baseline.as_deref().map(read_saved_sheet).transpose() {
        Ok(tree) => tree,
        Err(error) => return Ok(error),
    };
    let no_stubs = BTreeSet::new();
    let comparison = compare_layout(
        &sheet,
        baseline.as_ref().map(|(_, tree)| tree),
        &removed,
        [&no_stubs; 2],
    );
    Ok(CallToolResult::json(&comparison.json()))
}
