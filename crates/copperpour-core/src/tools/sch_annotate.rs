//! `annotate_schematic` (#454): Konnect's own annotator, because kicad-cli has
//! no `sch annotate`.
//!
//! The semantics are eeschema's Tools → Annotate defaults, pinned against
//! eeschema 10.0.5 on `tests/fixtures/annotate_duplicates*.kicad_sch`:
//!
//! - a `?` designator is numbered with the first free number for its prefix
//!   on its sheet instance, in ascending X (then Y);
//! - a designator that is already numbered is kept, so two symbols that share
//!   one are **reported**, never renumbered — unless `resolve_duplicates`
//!   asks for it, in which case the first in ascending X keeps the number and
//!   the rest get the first free one, which is what eeschema's "Reset existing
//!   annotations" produced on the fixture;
//! - a designator lives in two places, `(property "Reference" …)` and the
//!   `(instances …)` block, and both are written, as eeschema writes both;
//! - a `#`-prefixed designator is spelled the way eeschema spells it,
//!   `#PWR01`, `#PWR010`, `#PWR0100`: the prefix, a `0`, then the number.
//!
//! Every count in the response is derived from the committed file's readback,
//! never from the plan.
//!
//! With `schematic_scope: "hierarchy"` on a project's root (#42) the same
//! planner runs over every sheet instance of the saved hierarchy at once, as
//! eeschema's default "Entire schematic" scope does: records are taken by
//! path, the order is eeschema's sheet number (the page, as it repairs pages
//! on load) before X and Y, the Reference property follows each file's first
//! instance in that order, and every changed file is committed in one
//! transaction.

use crate::mcp::{error::ToolErrorKind, protocol::CallToolResult};
use crate::outcome::{self, OutcomeStatus};
use crate::tools::sch_hierarchy::{load_saved_hierarchy, SavedHierarchy};
use copperpour_sexp::{
    commit_file_transaction, inspect_file_transactions, parse_sexp, recover_file_transactions,
    schematic::parse_subsymbol_unit,
    writer::{
        apply_edits, find_enclosing_block, kicad_editor_lock_path, read_consistent,
        write_atomic_if_unchanged, SexpEdit,
    },
    FileTransition, SexpError, SexpNode,
};
use serde::Serialize;
use serde_json::{json, Value};
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use tracing::info;

const OPERATION: &str = "annotate_schematic";

/// An optional boolean argument: absent or `null` is `false`; anything else
/// that is not a boolean is refused by name rather than read as a default.
pub(crate) fn opt_bool(args: &Value, key: &str) -> Result<bool, CallToolResult> {
    match args.get(key) {
        None | Some(Value::Null) => Ok(false),
        Some(Value::Bool(value)) => Ok(*value),
        Some(_) => Err(CallToolResult::error_kind(
            ToolErrorKind::InvalidArgument {
                field: key.to_string(),
                reason: "must be a boolean".to_string(),
            },
            format!("Argument '{key}' must be a boolean"),
        )),
    }
}

// ─── Model ────────────────────────────────────────────────────────────────────

/// One `(instances (project … (path … (reference …))))` entry of a placed symbol.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PathReference {
    pub project: String,
    pub path: String,
    pub reference: String,
}

/// A placed symbol as the annotator sees it: KiCad's ordering key, the property
/// designator, and the per-sheet-instance designators in its instances block.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct PlacedSymbol {
    pub uuid: String,
    /// The embedded definition the placement draws (`lib_name`, else `lib_id`),
    /// by which `unit_count` is read.
    pub lib_symbol: String,
    /// The library item `lib_id` names, without its library nickname: what
    /// eeschema compares to tell the units of one package apart from other
    /// parts. An embedded copy KiCad saved as `Name_1` is still that item.
    pub lib_item: String,
    pub unit: u32,
    /// Units the embedded library definition declares (`Name_U_S` sub-symbols);
    /// `None` when the definition is missing from `lib_symbols`, in which case
    /// package identity cannot be proven.
    pub unit_count: Option<u32>,
    pub value: String,
    pub x: f64,
    pub y: f64,
    pub property_reference: String,
    pub paths: Vec<PathReference>,
}

/// Every placed symbol of a schematic, in file order.
///
/// Only the root's direct `symbol` children are placements; the definitions
/// under `lib_symbols` are nested one level down and carry no `lib_id`.
/// Unit count per embedded library definition, read from the `Name_U_S`
/// sub-symbol names KiCad writes under `lib_symbols`; a derived definition
/// (`extends`) takes its parent's count. A definition with no unit
/// sub-symbols is a single-unit part.
fn lib_symbol_units(tree: &SexpNode) -> BTreeMap<String, u32> {
    let mut declared: BTreeMap<String, Option<u32>> = BTreeMap::new();
    let mut extends: BTreeMap<String, String> = BTreeMap::new();
    if let Some(lib) = tree.find("lib_symbols") {
        for definition in lib.find_all("symbol") {
            let Some(name) = definition.get(1).and_then(SexpNode::as_str) else {
                continue;
            };
            let max_unit = definition
                .find_all("symbol")
                .iter()
                .filter_map(|sub| sub.get(1).and_then(SexpNode::as_str))
                .filter_map(parse_subsymbol_unit)
                .filter(|&unit| unit >= 1)
                .max();
            if let Some(parent) = definition.find_str("extends") {
                extends.insert(name.to_string(), parent.to_string());
            }
            declared.insert(name.to_string(), max_unit);
        }
    }
    let mut out = BTreeMap::new();
    for name in declared.keys() {
        let mut current = name.clone();
        let mut seen = BTreeSet::new();
        let count = loop {
            if !seen.insert(current.clone()) {
                break 1;
            }
            match declared.get(&current) {
                Some(Some(units)) => break *units,
                Some(None) => match extends.get(&current) {
                    Some(parent) => current = parent.clone(),
                    None => break 1,
                },
                None => break 1,
            }
        };
        out.insert(name.clone(), count);
    }
    out
}

pub(crate) fn placed_symbols(tree: &SexpNode) -> Result<Vec<PlacedSymbol>, String> {
    let lib_units = lib_symbol_units(tree);
    let mut out = Vec::new();
    for node in tree.find_all("symbol") {
        let Some(lib_id) = node.find_str("lib_id") else {
            continue;
        };
        let lib_symbol = node.find_str("lib_name").unwrap_or(lib_id).to_string();
        let lib_item = lib_id
            .split_once(':')
            .map_or(lib_id, |(_, item)| item)
            .to_string();
        let at = node
            .find("at")
            .ok_or_else(|| format!("a placed '{lib_id}' has no (at …)"))?;
        let (x, y) = match (at.get_f64(1), at.get_f64(2)) {
            (Some(x), Some(y)) => (x, y),
            _ => return Err(format!("a placed '{lib_id}' has a malformed (at …)")),
        };
        let unit = node.find_f64("unit").map(|u| u as u32).unwrap_or(1);
        let uuid = node
            .find_str("uuid")
            .ok_or_else(|| format!("the '{lib_id}' at ({x}, {y}) has no uuid"))?
            .to_string();
        let property = |name: &str| {
            node.find_all("property")
                .into_iter()
                .find(|property| property.get(1).and_then(SexpNode::as_str) == Some(name))
                .and_then(|property| property.get(2))
                .and_then(SexpNode::as_str)
                .map(str::to_string)
        };
        let property_reference = property("Reference")
            .ok_or_else(|| format!("symbol {uuid} has no Reference property"))?;
        let value = property("Value").unwrap_or_default();
        let mut paths = Vec::new();
        if let Some(instances) = node.find("instances") {
            for project in instances.find_all("project") {
                let project_name = project
                    .get(1)
                    .and_then(SexpNode::as_str)
                    .unwrap_or_default()
                    .to_string();
                for path in project.find_all("path") {
                    let Some(path_name) = path.get(1).and_then(SexpNode::as_str) else {
                        continue;
                    };
                    let reference = path
                        .find_str("reference")
                        .ok_or_else(|| {
                            format!("symbol {uuid} has an instance path without a reference")
                        })?
                        .to_string();
                    paths.push(PathReference {
                        project: project_name.clone(),
                        path: path_name.to_string(),
                        reference,
                    });
                }
            }
        }
        out.push(PlacedSymbol {
            uuid,
            unit_count: lib_units.get(&lib_symbol).copied(),
            lib_symbol,
            lib_item,
            unit,
            value,
            x,
            y,
            property_reference,
            paths,
        });
    }
    Ok(out)
}

// ─── Designators ──────────────────────────────────────────────────────────────

/// A designator split the way eeschema splits it: the letters (and any `#`)
/// in front, the number behind. `None` is unannotated: a trailing `?`, or no
/// number at all.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Designator {
    pub prefix: String,
    pub number: Option<u32>,
}

pub(crate) fn parse_designator(reference: &str) -> Designator {
    let unannotated = reference.ends_with('?');
    let trimmed = reference.trim_end_matches('?');
    let prefix_len = trimmed.trim_end_matches(|c: char| c.is_ascii_digit()).len();
    let number = if unannotated {
        None
    } else {
        trimmed[prefix_len..].parse::<u32>().ok()
    };
    Designator {
        prefix: trimmed[..prefix_len].to_string(),
        number,
    }
}

/// Spell a designator the way eeschema does. Power-style prefixes get a `0`
/// between prefix and number: eeschema writes `#PWR01`, `#PWR010`, `#PWR0100`
/// (every KiCad demo agrees), never `#PWR1` or `#PWR001`.
pub(crate) fn format_designator(prefix: &str, number: u32) -> String {
    if prefix.starts_with('#') {
        format!("{prefix}0{number}")
    } else {
        format!("{prefix}{number}")
    }
}

// ─── Plan ─────────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct Assignment {
    pub uuid: String,
    pub unit: u32,
    pub project: String,
    pub path: String,
    pub from: String,
    pub to: String,
}

/// Hierarchy scope: one record of a designator group, where it lives.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct Member {
    pub schematic: String,
    pub path: String,
    pub uuid: String,
    pub unit: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct DuplicateGroup {
    pub reference: String,
    pub project: String,
    pub paths: Vec<String>,
    pub uuids: Vec<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub members: Vec<Member>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct Unresolved {
    pub reference: String,
    pub project: String,
    pub paths: Vec<String>,
    pub uuids: Vec<String>,
    pub reason: String,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub members: Vec<Member>,
}

/// A symbol whose instance records name other projects but not the selected
/// one: outside this annotation, and left exactly as found.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct OutsideProject {
    pub uuid: String,
    pub reference: String,
    pub projects: Vec<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub(crate) struct AnnotationPlan {
    pub project: String,
    pub paths: Vec<String>,
    pub unannotated_before: usize,
    pub duplicates_before: Vec<DuplicateGroup>,
    pub assignments: Vec<Assignment>,
    pub unresolved: Vec<Unresolved>,
    pub outside_project: Vec<OutsideProject>,
    /// Hierarchy scope, per file: the file's first sheet instance in
    /// eeschema's order, whose designator the Reference property shows.
    #[serde(skip)]
    pub ordinal_path: Option<String>,
}

const REASON_DUPLICATE: &str = "duplicate designator in this project; set resolve_duplicates \
                                to keep the first in ascending X and renumber the rest";
const REASON_AMBIGUOUS: &str = "shared by different library symbols, at least one of them \
                                multi-unit; whether these are separate parts or units of one \
                                package is not proven, so nothing is renumbered";
const REASON_UNPROVEN_PACKAGE: &str =
    "units of one multi-unit part share this designator but do not form one package (a unit \
     repeats or is out of range, the values differ, or their definitions declare different \
     unit counts); which units belong together is not proven, so nothing is renumbered";
/// Hierarchy scope's keeper is eeschema's first, by page and then X.
const REASON_DUPLICATE_IN_HIERARCHY: &str =
    "duplicate designator in this project; set resolve_duplicates to keep the first by page, \
     then ascending X, and renumber the rest";
const REASON_UNPROVEN_NEW_PACKAGE: &str =
    "unannotated units of one multi-unit part on this sheet instance do not form one package \
     (a unit repeats or is out of range, or their definitions declare different unit counts); \
     which units belong together is not proven, so they stay unannotated";
const REASON_NO_DEFINITION: &str =
    "the library definition is missing from lib_symbols, so the unit count and package \
     identity cannot be proven; nothing is renumbered";
const REASON_NO_INSTANCE: &str =
    "no (instances …) entry ties this symbol to a sheet instance, so it \
                                  is left alone";
const REASON_SPLIT_PACKAGE: &str =
    "unannotated units of one multi-unit part sit on different sheet instances, and an \
     earlier one lacks a unit these have; eeschema would pack them into one package, which \
     units belong together is not proven, so they stay unannotated";
const REASON_JOINS_PACKAGE: &str =
    "a numbered package of this multi-unit part lacks a unit these unannotated units have; \
     eeschema would add them to it, which is not reproduced, so they stay unannotated";

/// What a set of placements sharing one designator provably is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Identity {
    /// The units of one multi-unit package: not a duplicate.
    OnePackage,
    /// Distinct single-unit parts: duplicates that can be renumbered
    /// placement by placement without dismantling anything.
    SeparateParts,
    /// Neither is proven; renumbering could split a package.
    Unproven(&'static str),
}

fn package_identity(members: &[&PlacedSymbol]) -> Identity {
    let Some(first) = members.first() else {
        return Identity::OnePackage;
    };
    let libraries: BTreeSet<&str> = members.iter().map(|m| m.lib_item.as_str()).collect();
    let units: BTreeSet<u32> = members.iter().map(|m| m.unit).collect();
    let all_single = members
        .iter()
        .all(|m| m.unit_count == Some(1) && m.unit == 1);
    if libraries.len() > 1 {
        return if all_single {
            Identity::SeparateParts
        } else {
            Identity::Unproven(REASON_AMBIGUOUS)
        };
    }
    match first.unit_count {
        None => Identity::Unproven(REASON_NO_DEFINITION),
        Some(1) => {
            if all_single {
                Identity::SeparateParts
            } else {
                Identity::Unproven(REASON_UNPROVEN_PACKAGE)
            }
        }
        Some(count) => {
            let same_value = members.iter().all(|m| m.value == first.value);
            let same_count = members.iter().all(|m| m.unit_count == first.unit_count);
            let in_range = units.iter().all(|unit| (1..=count).contains(unit));
            if same_value && same_count && in_range && units.len() == members.len() {
                Identity::OnePackage
            } else {
                Identity::Unproven(REASON_UNPROVEN_PACKAGE)
            }
        }
    }
}

/// eeschema's annotation order: ascending X, then Y; the uuid only breaks an
/// exact tie so the result is deterministic.
fn kicad_order(a: &PlacedSymbol, b: &PlacedSymbol) -> std::cmp::Ordering {
    a.x.total_cmp(&b.x)
        .then(a.y.total_cmp(&b.y))
        .then_with(|| a.uuid.cmp(&b.uuid))
}

fn distinct_paths(symbols: &[PlacedSymbol], entries: &[(usize, usize)]) -> Vec<String> {
    entries
        .iter()
        .map(|&(si, pi)| symbols[si].paths[pi].path.clone())
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect()
}

/// (symbol index, path index) into the placed symbols.
type Entry = (usize, usize);
/// One number to hand out: the sheet instance path, the leftmost member's
/// symbol index for ordering, and every entry that receives the number.
type Allocation = (String, usize, Vec<Entry>);
/// (path, prefix, library item, value): the unannotated placements that
/// could be the units of one package.
type NewGroupKey = (String, String, String, String);
/// (prefix, library item, value): one part, on any sheet instance.
type PartKey = (String, String, String);
/// One sheet instance's unannotated units of a part: its group, its
/// leftmost entry for ordering, and the units it holds.
type PartGroup<'a> = (&'a NewGroupKey, Entry, BTreeSet<u32>);

/// Plan the annotation of one project's instance records.
///
/// References are unique across the whole project (KiCad's flat list), so
/// numbers are reserved per project across every sheet instance in the file,
/// and a designator shared across instances is a duplicate unless it is the
/// units of one package. Instance records of other projects are never planned
/// or edited. Numbers used on the project's other sheets arrive through
/// `reserved` (see [`reserved_on_other_sheets`]); duplicates that already
/// exist across files are found only when every file is planned together
/// (see [`SheetOrder`]).
#[cfg(test)]
pub(crate) fn plan_annotation(
    symbols: &[PlacedSymbol],
    project: &str,
    resolve_duplicates: bool,
) -> AnnotationPlan {
    plan_annotation_reserving(symbols, project, &Reserved::default(), resolve_duplicates)
}

/// Numbers in use per prefix on the project's other sheets, so the file being
/// annotated cannot hand out a number the root or a sibling sheet already
/// owns.
pub(crate) type Reserved = BTreeMap<String, BTreeSet<u32>>;

/// What hierarchy scope gives the planner: every live sheet instance path
/// with eeschema's sheet number for it and the schematic holding it. The
/// planner then takes a record by its path, whatever project it names, and
/// orders by sheet number, X, Y and uuid, as eeschema's annotation sort does.
#[derive(Debug, Default)]
pub(crate) struct SheetOrder {
    pub sheet_number: BTreeMap<String, i64>,
    pub schematic: BTreeMap<String, String>,
}

pub(crate) fn plan_annotation_reserving(
    symbols: &[PlacedSymbol],
    project: &str,
    reserved: &Reserved,
    resolve_duplicates: bool,
) -> AnnotationPlan {
    plan_annotation_in(symbols, project, reserved, resolve_duplicates, None)
}

fn plan_annotation_in(
    symbols: &[PlacedSymbol],
    project: &str,
    reserved: &Reserved,
    resolve_duplicates: bool,
    sheets: Option<&SheetOrder>,
) -> AnnotationPlan {
    let mut plan = AnnotationPlan {
        project: project.to_string(),
        ..AnnotationPlan::default()
    };
    let is_own = |entry: &PathReference| match sheets {
        Some(sheets) => sheets.sheet_number.contains_key(&entry.path),
        None => entry.project == project,
    };
    // File scope orders sheet instances by path; hierarchy scope by eeschema's
    // sheet number, with the path last only to make ties deterministic.
    let order_at = |(path_a, si_a): (&String, usize), (path_b, si_b): (&String, usize)| {
        let placement = kicad_order(&symbols[si_a], &symbols[si_b]);
        match sheets {
            Some(sheets) => sheets.sheet_number[path_a]
                .cmp(&sheets.sheet_number[path_b])
                .then(placement)
                .then_with(|| path_a.cmp(path_b)),
            None => path_a.cmp(path_b).then(placement),
        }
    };
    let order = |a: Entry, b: Entry| {
        order_at(
            (&symbols[a.0].paths[a.1].path, a.0),
            (&symbols[b.0].paths[b.1].path, b.0),
        )
    };
    let members = |entries: &[Entry]| -> Vec<Member> {
        let Some(sheets) = sheets else {
            return Vec::new();
        };
        entries
            .iter()
            .map(|&(si, pi)| Member {
                schematic: sheets.schematic[&symbols[si].paths[pi].path].clone(),
                path: symbols[si].paths[pi].path.clone(),
                uuid: symbols[si].uuid.clone(),
                unit: symbols[si].unit,
            })
            .collect()
    };

    // Entries of the selected project: (symbol index, path index), in path
    // order then file order.
    let mut entries: Vec<(usize, usize)> = Vec::new();
    for (si, symbol) in symbols.iter().enumerate() {
        if symbol.paths.is_empty() {
            plan.unresolved.push(Unresolved {
                reference: symbol.property_reference.clone(),
                project: project.to_string(),
                paths: Vec::new(),
                uuids: vec![symbol.uuid.clone()],
                reason: REASON_NO_INSTANCE.to_string(),
                members: Vec::new(),
            });
            continue;
        }
        let mut own = false;
        for (pi, entry) in symbol.paths.iter().enumerate() {
            if is_own(entry) {
                entries.push((si, pi));
                own = true;
            }
        }
        if !own {
            plan.outside_project.push(OutsideProject {
                uuid: symbol.uuid.clone(),
                reference: symbol.property_reference.clone(),
                projects: symbol
                    .paths
                    .iter()
                    .map(|p| p.project.clone())
                    .collect::<BTreeSet<_>>()
                    .into_iter()
                    .collect(),
            });
        }
    }
    entries.sort_by(|a, b| {
        symbols[a.0].paths[a.1]
            .path
            .cmp(&symbols[b.0].paths[b.1].path)
            .then(a.cmp(b))
    });
    plan.paths = distinct_paths(symbols, &entries);

    // Numbers in use anywhere in the project, per prefix: the other sheets'
    // first, then every instance in this file.
    let mut used: BTreeMap<String, BTreeSet<u32>> = reserved.clone();
    let mut groups: BTreeMap<&str, Vec<(usize, usize)>> = BTreeMap::new();
    let mut unannotated: Vec<(usize, usize)> = Vec::new();
    for &(si, pi) in &entries {
        let reference = symbols[si].paths[pi].reference.as_str();
        match parse_designator(reference).number {
            Some(number) => {
                used.entry(parse_designator(reference).prefix)
                    .or_default()
                    .insert(number);
                groups.entry(reference).or_default().push((si, pi));
            }
            None => {
                plan.unannotated_before += 1;
                unannotated.push((si, pi));
            }
        }
    }

    // An allocation takes one number: a single placement, or every unit of a
    // proven package. Ordered by path, then eeschema's X order of the
    // leftmost member.
    let mut allocations: Vec<Allocation> = Vec::new();

    for (reference, group) in &groups {
        if group.len() < 2 {
            continue;
        }
        let placed: Vec<&PlacedSymbol> = group.iter().map(|&(si, _)| &symbols[si]).collect();
        let identity = package_identity(&placed);
        if identity == Identity::OnePackage {
            continue;
        }
        let mut ordered = group.clone();
        ordered.sort_by(|&a, &b| order(a, b));
        let uuids: Vec<String> = ordered
            .iter()
            .map(|&(si, _)| symbols[si].uuid.clone())
            .collect();
        let paths = distinct_paths(symbols, &ordered);
        plan.duplicates_before.push(DuplicateGroup {
            reference: reference.to_string(),
            project: project.to_string(),
            paths: paths.clone(),
            uuids: uuids.clone(),
            members: members(&ordered),
        });
        match identity {
            Identity::Unproven(reason) => plan.unresolved.push(Unresolved {
                reference: reference.to_string(),
                project: project.to_string(),
                paths,
                uuids,
                reason: reason.to_string(),
                members: members(&ordered),
            }),
            Identity::SeparateParts if resolve_duplicates => {
                for (si, pi) in ordered.into_iter().skip(1) {
                    allocations.push((symbols[si].paths[pi].path.clone(), si, vec![(si, pi)]));
                }
            }
            Identity::SeparateParts => plan.unresolved.push(Unresolved {
                reference: reference.to_string(),
                project: project.to_string(),
                paths,
                uuids,
                reason: if sheets.is_some() {
                    REASON_DUPLICATE_IN_HIERARCHY
                } else {
                    REASON_DUPLICATE
                }
                .to_string(),
                members: members(&ordered),
            }),
            Identity::OnePackage => unreachable!("handled above"),
        }
    }

    // Unannotated placements: the units of one multi-unit part on one sheet
    // instance (same definition and value) are one package when their units
    // are distinct; otherwise which units belong together is a guess, and
    // eeschema's guess is not reproduced here.
    let mut new_groups: BTreeMap<NewGroupKey, Vec<Entry>> = BTreeMap::new();
    for (si, pi) in unannotated {
        let symbol = &symbols[si];
        let key = (
            symbol.paths[pi].path.clone(),
            parse_designator(&symbol.paths[pi].reference).prefix,
            symbol.lib_item.clone(),
            symbol.value.clone(),
        );
        new_groups.entry(key).or_default().push((si, pi));
    }
    // Hierarchy scope reaches units of one part on different sheet instances.
    // eeschema numbers each new unit with the lowest package of that part
    // lacking it, across sheets; where an earlier instance's units lack one of
    // a later instance's, that pairing is not reproduced, so the later group
    // stays unannotated rather than opening a package eeschema would not.
    let mut split: BTreeSet<NewGroupKey> = BTreeSet::new();
    let mut joins: BTreeSet<NewGroupKey> = BTreeSet::new();
    if sheets.is_some() {
        let mut by_part: BTreeMap<PartKey, Vec<PartGroup>> = BTreeMap::new();
        for (key, group) in &new_groups {
            if symbols[group[0].0].unit_count.is_none_or(|count| count < 2) {
                continue;
            }
            let leftmost = group
                .iter()
                .copied()
                .min_by(|&a, &b| order(a, b))
                .unwrap_or(group[0]);
            let units = group.iter().map(|&(si, _)| symbols[si].unit).collect();
            by_part
                .entry((key.1.clone(), key.2.clone(), key.3.clone()))
                .or_default()
                .push((key, leftmost, units));
        }
        for groups in by_part.values_mut() {
            groups.sort_by(|a, b| order(a.1, b.1));
            for (index, (key, _, units)) in groups.iter().enumerate() {
                if groups[..index]
                    .iter()
                    .any(|(_, _, earlier)| units.iter().any(|unit| !earlier.contains(unit)))
                {
                    split.insert((*key).clone());
                }
            }
        }
        // Nor is it reproduced for a package already numbered, eeschema's
        // first choice: numbering the earlier group alone would otherwise
        // leave the later one, on the next call, to open a package eeschema
        // would not.
        for (reference, placed) in &groups {
            let first = &symbols[placed[0].0];
            if placed.iter().any(|&(si, _)| {
                symbols[si].lib_item != first.lib_item || symbols[si].value != first.value
            }) {
                continue;
            }
            let part = (
                parse_designator(reference).prefix,
                first.lib_item.clone(),
                first.value.clone(),
            );
            let held: BTreeSet<u32> = placed.iter().map(|&(si, _)| symbols[si].unit).collect();
            for (key, _, units) in by_part.get(&part).into_iter().flatten() {
                if units.iter().any(|unit| !held.contains(unit)) {
                    joins.insert((*key).clone());
                }
            }
        }
    }
    for (key, group) in new_groups {
        let path = key.0.clone();
        let count = symbols[group[0].0].unit_count;
        let units: BTreeSet<u32> = group.iter().map(|&(si, _)| symbols[si].unit).collect();
        let one_package = match count {
            Some(count) if count >= 2 => {
                units.len() == group.len()
                    && units.iter().all(|u| (1..=count).contains(u))
                    && group
                        .iter()
                        .all(|&(si, _)| symbols[si].unit_count == Some(count))
            }
            _ => false,
        };
        if one_package && !split.contains(&key) && !joins.contains(&key) {
            let leftmost = group
                .iter()
                .map(|&(si, _)| si)
                .min_by(|&a, &b| kicad_order(&symbols[a], &symbols[b]))
                .unwrap_or(group[0].0);
            allocations.push((path, leftmost, group));
        } else if count.is_some_and(|count| count >= 2) {
            let mut ordered = group.clone();
            ordered.sort_by(|a, b| kicad_order(&symbols[a.0], &symbols[b.0]));
            let reason = if split.contains(&key) {
                REASON_SPLIT_PACKAGE
            } else if joins.contains(&key) {
                REASON_JOINS_PACKAGE
            } else {
                REASON_UNPROVEN_NEW_PACKAGE
            };
            plan.unresolved.push(Unresolved {
                reference: symbols[ordered[0].0].paths[ordered[0].1].reference.clone(),
                project: project.to_string(),
                paths: vec![path.clone()],
                uuids: ordered
                    .iter()
                    .map(|&(si, _)| symbols[si].uuid.clone())
                    .collect(),
                reason: reason.to_string(),
                members: members(&ordered),
            });
        } else {
            for (si, pi) in group {
                allocations.push((path.clone(), si, vec![(si, pi)]));
            }
        }
    }

    allocations.sort_by(|a, b| order_at((&a.0, a.1), (&b.0, b.1)));
    for (path, _, group) in allocations {
        let prefix = parse_designator(&symbols[group[0].0].paths[group[0].1].reference).prefix;
        let taken = used.entry(prefix.clone()).or_default();
        let number = (1u32..).find(|n| !taken.contains(n)).unwrap_or(1);
        taken.insert(number);
        let to = format_designator(&prefix, number);
        for (si, pi) in group {
            let symbol = &symbols[si];
            plan.assignments.push(Assignment {
                uuid: symbol.uuid.clone(),
                unit: symbol.unit,
                // The record's own project name: in hierarchy scope a record
                // is the project's by its path, and the edit addresses the
                // block it is written in.
                project: symbol.paths[pi].project.clone(),
                path: path.clone(),
                from: symbol.paths[pi].reference.clone(),
                to: to.clone(),
            });
        }
    }
    plan
}

// ─── Edits ────────────────────────────────────────────────────────────────────

/// Byte range of the placed `(symbol …)` block carrying `uuid`.
pub(crate) fn symbol_block(content: &str, uuid: &str) -> Result<(usize, usize), String> {
    let needle = format!("(uuid \"{uuid}\")");
    let pos = content
        .find(&needle)
        .ok_or_else(|| format!("symbol {uuid} is not in the file"))?;
    let (start, end) = find_enclosing_block(content, "symbol", pos)
        .ok_or_else(|| format!("symbol {uuid} has no enclosing (symbol …) block"))?;
    if !content[start..end].contains("(lib_id ") {
        return Err(format!(
            "symbol {uuid} resolved to a library definition, not a placement"
        ));
    }
    Ok((start, end))
}

/// The property a symbol shows after its assignments.
///
/// In a reused sheet the property carries the designator of whichever sheet
/// instance eeschema last displayed, which need not be the first instance
/// path in the file (the `multichannel` demo has `#PWR016` in the property
/// and `#PWR024` on its first path). So the property follows the instance
/// whose designator it currently shows, and is otherwise left alone.
///
/// Across a whole hierarchy the instance is known: eeschema saves the
/// property from the file's first sheet instance in its hierarchy order
/// (`ordinal`), so an assigned symbol's property takes that record's
/// designator.
fn property_after(
    symbol: &PlacedSymbol,
    assignments: &[&Assignment],
    ordinal: Option<&str>,
) -> String {
    if let Some(ordinal) = ordinal.filter(|_| !assignments.is_empty()) {
        return assignments
            .iter()
            .find(|a| a.path == ordinal)
            .map(|a| a.to.clone())
            .or_else(|| {
                symbol
                    .paths
                    .iter()
                    .find(|p| p.path == ordinal)
                    .map(|p| p.reference.clone())
            })
            .unwrap_or_else(|| symbol.property_reference.clone());
    }
    assignments
        .iter()
        .find(|a| a.from == symbol.property_reference)
        .map(|a| a.to.clone())
        .unwrap_or_else(|| symbol.property_reference.clone())
}

/// Replace the quoted value that follows `needle` inside `block` (an absolute
/// byte range of `content`), demanding exactly one occurrence.
fn replace_quoted_after(
    content: &str,
    block: (usize, usize),
    needle: &str,
    replacement: &str,
    what: &str,
) -> Result<SexpEdit, String> {
    let text = &content[block.0..block.1];
    let mut hits = text.match_indices(needle);
    let (rel, _) = hits
        .next()
        .ok_or_else(|| format!("{what}: '{needle}' not found in the symbol block"))?;
    if hits.next().is_some() {
        return Err(format!(
            "{what}: '{needle}' occurs more than once in the symbol block"
        ));
    }
    let value_start = block.0 + rel + needle.len();
    let value_len = content[value_start..]
        .find('"')
        .ok_or_else(|| format!("{what}: unterminated value after '{needle}'"))?;
    Ok(SexpEdit::replace(
        value_start,
        value_start + value_len,
        replacement.to_string(),
    ))
}

/// The byte edits that realise `plan` on `content`: for every assigned symbol,
/// the `(reference …)` of each assigned sheet instance and, when it changes,
/// the `(property "Reference" …)`.
pub(crate) fn plan_edits(
    content: &str,
    symbols: &[PlacedSymbol],
    plan: &AnnotationPlan,
) -> Result<Vec<SexpEdit>, String> {
    let mut by_uuid: BTreeMap<&str, Vec<&Assignment>> = BTreeMap::new();
    for assignment in &plan.assignments {
        by_uuid
            .entry(assignment.uuid.as_str())
            .or_default()
            .push(assignment);
    }
    let mut edits = Vec::new();
    for (uuid, assignments) in by_uuid {
        let symbol = symbols
            .iter()
            .find(|s| s.uuid == uuid)
            .ok_or_else(|| format!("assignment for unknown symbol {uuid}"))?;
        let block = symbol_block(content, uuid)?;
        for assignment in &assignments {
            // The instance entry is addressed by project first, then path: two
            // projects sharing a sheet share its path strings.
            let project_needle = format!("(project \"{}\"", assignment.project);
            let rel = content[block.0..block.1]
                .find(&project_needle)
                .ok_or_else(|| {
                    format!(
                        "symbol {uuid} has no instance records for project '{}'",
                        assignment.project
                    )
                })?;
            let project_block = find_enclosing_block(content, "project", block.0 + rel + 1)
                .ok_or_else(|| format!("symbol {uuid}: instance project block not found"))?;
            let path_needle = format!("(path \"{}\"", assignment.path);
            let rel = content[project_block.0..project_block.1]
                .find(&path_needle)
                .ok_or_else(|| {
                    format!(
                        "symbol {uuid} has no instance path '{}' in project '{}'",
                        assignment.path, assignment.project
                    )
                })?;
            let path_block = find_enclosing_block(content, "path", project_block.0 + rel + 1)
                .ok_or_else(|| format!("symbol {uuid}: instance path block not found"))?;
            edits.push(replace_quoted_after(
                content,
                path_block,
                "(reference \"",
                &assignment.to,
                &format!("symbol {uuid} instance '{}'", assignment.path),
            )?);
        }
        let property = property_after(symbol, &assignments, plan.ordinal_path.as_deref());
        if property != symbol.property_reference {
            edits.push(replace_quoted_after(
                content,
                block,
                "(property \"Reference\" \"",
                &property,
                &format!("symbol {uuid} Reference property"),
            )?);
        }
    }
    Ok(edits)
}

/// Prove that `after` is `before` with exactly the plan's assignments applied:
/// every assigned instance shows its new designator, every other instance and
/// every property is as expected, and no symbol appeared or vanished.
pub(crate) fn verify_applied(
    before: &[PlacedSymbol],
    after: &[PlacedSymbol],
    plan: &AnnotationPlan,
) -> Result<(), String> {
    if before.len() != after.len() {
        return Err(format!(
            "symbol count changed from {} to {}",
            before.len(),
            after.len()
        ));
    }
    let assigned: BTreeMap<(&str, &str, &str), &Assignment> = plan
        .assignments
        .iter()
        .map(|a| ((a.uuid.as_str(), a.project.as_str(), a.path.as_str()), a))
        .collect();
    for symbol in before {
        let observed = after
            .iter()
            .find(|s| s.uuid == symbol.uuid)
            .ok_or_else(|| format!("symbol {} vanished", symbol.uuid))?;
        if observed.paths.len() != symbol.paths.len() {
            return Err(format!(
                "symbol {} lost or gained an instance path",
                symbol.uuid
            ));
        }
        for entry in &symbol.paths {
            let expected = assigned
                .get(&(
                    symbol.uuid.as_str(),
                    entry.project.as_str(),
                    entry.path.as_str(),
                ))
                .map(|a| a.to.as_str())
                .unwrap_or(entry.reference.as_str());
            let got = observed
                .paths
                .iter()
                .find(|p| p.project == entry.project && p.path == entry.path)
                .map(|p| p.reference.as_str())
                .ok_or_else(|| {
                    format!(
                        "symbol {} lost instance path '{}' of project '{}'",
                        symbol.uuid, entry.path, entry.project
                    )
                })?;
            if got != expected {
                return Err(format!(
                    "symbol {} on '{}' ({}) reads '{got}', expected '{expected}'",
                    symbol.uuid, entry.path, entry.project
                ));
            }
        }
        let own: Vec<&Assignment> = plan
            .assignments
            .iter()
            .filter(|a| a.uuid == symbol.uuid)
            .collect();
        let expected_property = property_after(symbol, &own, plan.ordinal_path.as_deref());
        if observed.property_reference != expected_property {
            return Err(format!(
                "symbol {} Reference property reads '{}', expected '{expected_property}'",
                symbol.uuid, observed.property_reference
            ));
        }
    }
    Ok(())
}

// ─── Run ──────────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Default)]
pub(crate) struct AnnotateOptions {
    pub resolve_duplicates: bool,
    pub dry_run: bool,
    /// Project whose instance records are annotated; `None` resolves it from
    /// the schematic's owner (its `.kicad_pro`) or the only project named.
    pub project: Option<String>,
}

pub(crate) fn opt_string(args: &Value, key: &str) -> Result<Option<String>, CallToolResult> {
    match args.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(value)) => Ok(Some(value.clone())),
        Some(_) => Err(CallToolResult::error_kind(
            ToolErrorKind::InvalidArgument {
                field: key.to_string(),
                reason: "must be a string".to_string(),
            },
            format!("Argument '{key}' must be a string"),
        )),
    }
}

/// The project whose instance records this call annotates: the explicit
/// argument, else the schematic's owning project (#189's sheet-tree rule),
/// else the only project the file names. Anything ambiguous is refused with
/// the candidates, so another project's records are never edited by accident.
/// The selected project and, when ownership is proven through a `.kicad_pro`
/// and its sheet tree, the root schematic that tree hangs from.
fn select_project(
    path: &Path,
    symbols: &[PlacedSymbol],
    requested: Option<&str>,
) -> Result<(String, Option<std::path::PathBuf>), CallToolResult> {
    let named: Vec<String> = symbols
        .iter()
        .flat_map(|s| s.paths.iter().map(|p| p.project.clone()))
        .filter(|p| !p.is_empty())
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect();
    let refuse = |reason: String| {
        CallToolResult::error_kind(
            ToolErrorKind::InvalidArgument {
                field: "project".to_string(),
                reason: reason.clone(),
            },
            format!(
                "Cannot decide which project's instance records to annotate: {reason}. \
                 Pass project to choose one of {named:?}."
            ),
        )
    };
    if named.is_empty() && requested.is_none() {
        // Nothing carries an instance record; the plan reports every symbol
        // as untied to a sheet instance.
        return Ok((String::new(), None));
    }
    // Ownership is consulted even for an explicit project: it is what makes
    // the other sheets' numbers visible.
    let owner = match crate::tools::resolve_schematic_ownership(path) {
        Ok(owner) => owner,
        Err(error) if requested.is_some() => {
            info!("[annotate] project ownership not proven ({error}); only this file's numbers are reserved");
            None
        }
        Err(error) => {
            return Err(refuse(format!("project ownership is not proven: {error}")));
        }
    };
    let owner_stem = owner.as_ref().map(|owner| {
        owner
            .project_file
            .file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_default()
    });
    let root_for = |project: &str| {
        owner
            .as_ref()
            .filter(|_| owner_stem.as_deref() == Some(project))
            .map(|owner| owner.root_schematic.clone())
    };
    if let Some(requested) = requested {
        return if named.iter().any(|n| n == requested) {
            Ok((requested.to_string(), root_for(requested)))
        } else {
            Err(refuse(format!(
                "'{requested}' is not among the projects named by {}'s instance records",
                path.display()
            )))
        };
    }
    match (owner_stem.as_deref(), named.len()) {
        (Some(stem), _) if named.iter().any(|n| n == stem) => {
            let root = root_for(stem);
            Ok((stem.to_string(), root))
        }
        (Some(stem), _) => Err(refuse(format!(
            "the schematic belongs to project '{stem}' but its instance records name {named:?}"
        ))),
        (None, 1) => Ok((named[0].clone(), None)),
        (None, _) => Err(refuse(format!(
            "no .kicad_pro owns this schematic and its instance records name several \
             projects: {named:?}"
        ))),
    }
}

/// Numbered instance references of `project` on every sheet of the tree
/// under `root` except `target` itself, plus the files consulted. A sheet
/// that cannot be read is skipped and named in the second list, so a caller
/// knows which numbers stayed invisible.
pub(crate) fn reserved_on_other_sheets(
    root: &Path,
    target: &Path,
    project: &str,
) -> (Reserved, Vec<String>, Vec<String>) {
    let mut reserved = Reserved::new();
    let mut consulted = Vec::new();
    let mut unreadable = Vec::new();
    let mut stack = vec![(root.to_path_buf(), 0usize)];
    let mut seen: BTreeSet<std::path::PathBuf> = BTreeSet::new();
    let canonical = |p: &Path| std::fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf());
    let target_canonical = canonical(target);
    while let Some((file, depth)) = stack.pop() {
        if depth > crate::tools::sch_hierarchy::MAX_HIERARCHY_DEPTH
            || !seen.insert(canonical(&file))
        {
            continue;
        }
        let content = match read_consistent(&file) {
            Ok(content) => content,
            Err(_) => {
                unreadable.push(file.display().to_string());
                continue;
            }
        };
        let tree = match parse_sexp(&content) {
            Ok(tree) => tree,
            Err(_) => {
                unreadable.push(file.display().to_string());
                continue;
            }
        };
        let directory = file.parent().unwrap_or_else(|| Path::new("."));
        for child in copperpour_sexp::schematic::sheet_files(&tree) {
            stack.push((directory.join(child), depth + 1));
        }
        if canonical(&file) == target_canonical {
            continue;
        }
        let Ok(symbols) = placed_symbols(&tree) else {
            unreadable.push(file.display().to_string());
            continue;
        };
        consulted.push(file.display().to_string());
        for symbol in &symbols {
            for entry in symbol.paths.iter().filter(|p| p.project == project) {
                let designator = parse_designator(&entry.reference);
                if let Some(number) = designator.number {
                    reserved
                        .entry(designator.prefix)
                        .or_default()
                        .insert(number);
                }
            }
        }
    }
    (reserved, consulted, unreadable)
}

fn parse_file(path: &Path, content: &str) -> Result<Vec<PlacedSymbol>, String> {
    let tree = parse_sexp(content).map_err(|e| format!("{}: {e}", path.display()))?;
    placed_symbols(&tree)
}

fn retry_for(plan: &AnnotationPlan, resolve_duplicates: bool) -> Option<Value> {
    if plan.unresolved.is_empty() {
        return None;
    }
    let only_resolvable = plan
        .unresolved
        .iter()
        .all(|u| u.reason == REASON_DUPLICATE || u.reason == REASON_DUPLICATE_IN_HIERARCHY);
    if only_resolvable && !resolve_duplicates {
        Some(json!({
            "safe": true,
            "scope": "unresolved_duplicates",
            "instruction": "call again with resolve_duplicates: true to renumber the listed duplicate groups; nothing already assigned needs repeating"
        }))
    } else {
        Some(json!({
            "safe": false,
            "scope": "unresolved_designators",
            "instruction": "the listed groups need a decision the tool will not guess at; edit the designators (edit_schematic_component) and call again"
        }))
    }
}

fn body(
    path: &Path,
    options: &AnnotateOptions,
    plan: &AnnotationPlan,
    sheets: (&[String], &[String]),
    written: bool,
) -> Value {
    json!({
        "schematic": path.display().to_string(),
        "schematic_scope": "file",
        "dry_run": options.dry_run,
        "resolve_duplicates": options.resolve_duplicates,
        "written": written,
        "project": plan.project,
        "paths": plan.paths,
        "outside_project": plan.outside_project,
        "other_sheets_consulted": sheets.0,
        "other_sheets_unreadable": sheets.1,
        "unannotated_before": plan.unannotated_before,
        "duplicates_before": plan.duplicates_before,
        "assigned": plan.assignments,
        "unresolved": plan.unresolved,
    })
}

/// Annotate the schematic at `path` and report exactly what happened.
pub(crate) fn annotate_file(path: &Path, options: AnnotateOptions) -> CallToolResult {
    annotate_file_with_persistence(path, options, write_atomic_if_unchanged)
}

/// Keep persistence injectable so tests can exercise failures after an actual
/// replacement without racing an external editor. Production uses the shared
/// conditional atomic writer above.
fn annotate_file_with_persistence(
    path: &Path,
    options: AnnotateOptions,
    persist: impl FnOnce(&Path, &str, &str) -> Result<(), SexpError>,
) -> CallToolResult {
    let content = match read_consistent(path) {
        Ok(content) => content,
        Err(SexpError::Io(error)) if error.kind() == std::io::ErrorKind::NotFound => {
            return CallToolResult::error_kind(
                ToolErrorKind::FileNotFound {
                    path: path.display().to_string(),
                },
                format!("Schematic not found: {}", path.display()),
            );
        }
        Err(error) => {
            return CallToolResult::error_kind(
                ToolErrorKind::HandlerError {
                    reason: error.to_string(),
                },
                format!("Could not read {}: {error}", path.display()),
            );
        }
    };
    let parsed = parse_sexp(&content)
        .map_err(|e| format!("{}: {e}", path.display()))
        .and_then(|tree| Ok((placed_symbols(&tree)?, tree.find_all("sheet").len())));
    let (before, child_sheets) = match parsed {
        Ok(parsed) => parsed,
        Err(reason) => {
            return CallToolResult::error_kind(
                ToolErrorKind::HandlerError {
                    reason: reason.clone(),
                },
                format!("Could not read the schematic's symbols: {reason}"),
            );
        }
    };
    let (project, root) = match select_project(path, &before, options.project.as_deref()) {
        Ok(selected) => selected,
        Err(refusal) => return refusal,
    };
    let (reserved, other_sheets, unreadable_sheets) = match &root {
        Some(root) => reserved_on_other_sheets(root, path, &project),
        None => (Reserved::new(), Vec::new(), Vec::new()),
    };
    let plan = plan_annotation_reserving(&before, &project, &reserved, options.resolve_duplicates);
    let target = path.display().to_string();
    let requested = plan.assignments.len() + plan.unresolved.len();

    if options.dry_run || plan.assignments.is_empty() {
        let status = if plan.unresolved.is_empty() {
            OutcomeStatus::Complete
        } else {
            OutcomeStatus::Partial
        };
        let source = if options.dry_run {
            "dry_run_plan"
        } else {
            "saved_file"
        };
        let mut response = body(
            path,
            &options,
            &plan,
            (&other_sheets, &unreadable_sheets),
            false,
        );
        response["message"] = json!(if options.dry_run {
            format!(
                "Dry run: nothing was written; assigned lists what a real run would write.{}",
                child_sheet_note(child_sheets)
            )
        } else if plan.unresolved.is_empty() && child_sheets > 0 {
            format!(
                "Nothing to annotate in this file: every designator it carries is numbered and \
                 unique on its sheet instances.{}",
                child_sheet_note(child_sheets)
            )
        } else if plan.unresolved.is_empty() {
            "Nothing to annotate: every designator is numbered and unique on every sheet instance."
                .to_string()
        } else {
            format!(
                "Nothing was written: no designator needed a number, but the listed groups are \
                 unresolved.{}",
                child_sheet_note(child_sheets)
            )
        });
        return outcome::attach(
            CallToolResult::json(&response),
            outcome::summary(
                status,
                target,
                source,
                requested,
                plan.assignments.len(),
                plan.unresolved.len(),
                retry_for(&plan, options.resolve_duplicates),
            ),
        );
    }

    // Preflight: the exact prospective text must show every assignment and
    // nothing else changed, or nothing is written.
    let edits = match plan_edits(&content, &before, &plan) {
        Ok(edits) => edits,
        Err(reason) => return refused(path, &plan, &options, reason),
    };
    let prospective = apply_edits(content.clone(), edits);
    match parse_file(path, &prospective).and_then(|after| verify_applied(&before, &after, &plan)) {
        Ok(()) => {}
        Err(reason) => return refused(path, &plan, &options, reason),
    }

    match persist(path, &content, &prospective) {
        Ok(()) => {}
        Err(error) => {
            // The shared writer can return Conflict before replacement OR
            // after replacement when its readback differs. Without timing
            // evidence, no persistence error proves that nothing was applied.
            let result = crate::tools::mutation_outcome_uncertain(
                path,
                OPERATION,
                format!("schematic persistence failed: {error}"),
            );
            return outcome::attach(
                result,
                outcome::summary(
                    OutcomeStatus::Uncertain,
                    target,
                    "saved_file_readback",
                    requested,
                    0,
                    requested,
                    Some(outcome::inspect_before_retry()),
                ),
            );
        }
    }

    // Readback: every count below comes from the committed file.
    let committed = match read_consistent(path)
        .map_err(|e| e.to_string())
        .and_then(|content| parse_file(path, &content))
    {
        Ok(after) => after,
        Err(reason) => {
            let result = crate::tools::mutation_outcome_uncertain(
                path,
                OPERATION,
                format!("saved schematic could not be reloaded: {reason}"),
            );
            return outcome::attach(
                result,
                outcome::summary(
                    OutcomeStatus::Uncertain,
                    target,
                    "saved_file_readback",
                    requested,
                    0,
                    requested,
                    Some(outcome::inspect_before_retry()),
                ),
            );
        }
    };
    if let Err(reason) = verify_applied(&before, &committed, &plan) {
        let result = crate::tools::mutation_outcome_uncertain(
            path,
            OPERATION,
            format!("the saved schematic does not show the annotation as planned: {reason}"),
        );
        return outcome::attach(
            result,
            outcome::summary(
                OutcomeStatus::Uncertain,
                target,
                "saved_file_readback",
                requested,
                0,
                requested,
                Some(outcome::inspect_before_retry()),
            ),
        );
    }
    // What still needs a decision after the write, read from the file.
    let remaining =
        plan_annotation_reserving(&committed, &project, &reserved, options.resolve_duplicates);
    let status = if remaining.assignments.is_empty() && remaining.unresolved.is_empty() {
        OutcomeStatus::Complete
    } else {
        OutcomeStatus::Partial
    };
    let mut response = body(
        path,
        &options,
        &plan,
        (&other_sheets, &unreadable_sheets),
        true,
    );
    response["remaining_unannotated"] = json!(remaining.unannotated_before);
    response["message"] = json!(match status {
        OutcomeStatus::Complete if child_sheets > 0 => format!(
            "Annotated this file; every designator it carries is numbered and unique on its \
             sheet instances.{}",
            child_sheet_note(child_sheets)
        ),
        OutcomeStatus::Complete =>
            "Annotated; every designator is numbered and unique on every sheet instance."
                .to_string(),
        _ => format!(
            "Annotated what could be decided; the listed groups are unresolved.{}",
            child_sheet_note(child_sheets)
        ),
    });
    outcome::attach(
        CallToolResult::json(&response),
        outcome::summary(
            status,
            target,
            "saved_file_readback",
            requested,
            plan.assignments.len(),
            plan.unresolved.len(),
            retry_for(&remaining, options.resolve_duplicates),
        ),
    )
}

/// What a file-scope call says of the sheets a file places; nothing when it
/// places none.
fn child_sheet_note(child_sheets: usize) -> String {
    if child_sheets == 0 {
        return String::new();
    }
    format!(
        " This file places {child_sheets} sheet(s), which this call did not annotate; call it on \
         the project's root with schematic_scope 'hierarchy' to annotate every sheet instance."
    )
}

/// A hierarchy's placed symbols per file, and the planner's view of them.
struct HierarchyPlan {
    symbols: Vec<Vec<PlacedSymbol>>,
    /// The file holding each live sheet instance path.
    file_of: BTreeMap<String, usize>,
    plan: AnnotationPlan,
}

enum HierarchyPlanError {
    Unreadable(String),
    Records(String),
}

/// Every placed symbol must record each live sheet instance of its file
/// exactly once and no project path that is not one: the instance-metadata
/// rule placement keeps (docs/PROJECT_OWNERSHIP.md). A record is the
/// project's when its path starts at the root — eeschema keys records by path
/// and saves those under the project's name — so it may carry the project's
/// name or none. Other projects' records are left alone.
fn instance_record_problems(
    hierarchy: &SavedHierarchy,
    symbols: &[Vec<PlacedSymbol>],
    project: &str,
) -> Vec<String> {
    let root = format!("/{}", hierarchy.root_uuid);
    let below_root = format!("{root}/");
    let mut live: Vec<BTreeSet<&str>> = vec![BTreeSet::new(); hierarchy.files.len()];
    for instance in &hierarchy.instances {
        live[instance.file].insert(&instance.path);
    }
    let mut problems = Vec::new();
    for (file, placed) in symbols.iter().enumerate() {
        let name = hierarchy.files[file].path.display();
        let mut uuids = BTreeSet::new();
        for symbol in placed {
            let who = format!("{name}: {} ({})", symbol.property_reference, symbol.uuid);
            if !uuids.insert(symbol.uuid.as_str()) {
                problems.push(format!(
                    "{who} shares its uuid with another symbol of the file"
                ));
            }
            let mut recorded: BTreeMap<&str, usize> = BTreeMap::new();
            for record in &symbol.paths {
                if record.path != root && !record.path.starts_with(&below_root) {
                    if record.project == project {
                        problems.push(format!(
                            "{who} records '{}', which does not start at the root",
                            record.path
                        ));
                    }
                    continue;
                }
                if !live[file].contains(record.path.as_str()) {
                    problems.push(format!(
                        "{who} records '{}', which is not a sheet instance of this file",
                        record.path
                    ));
                    continue;
                }
                if !record.project.is_empty() && record.project != project {
                    problems.push(format!(
                        "{who} records '{}' under project '{}', not '{project}'",
                        record.path, record.project
                    ));
                }
                *recorded.entry(record.path.as_str()).or_default() += 1;
            }
            for path in &live[file] {
                match recorded.get(path) {
                    None => {
                        problems.push(format!("{who} has no record for sheet instance '{path}'"))
                    }
                    Some(1) => {}
                    Some(_) => problems.push(format!("{who} records '{path}' more than once")),
                }
            }
        }
    }
    problems
}

fn plan_hierarchy(
    hierarchy: &SavedHierarchy,
    project: &str,
    resolve_duplicates: bool,
) -> Result<HierarchyPlan, HierarchyPlanError> {
    let mut symbols = Vec::new();
    for file in &hierarchy.files {
        symbols.push(placed_symbols(&file.tree).map_err(|reason| {
            HierarchyPlanError::Unreadable(format!("{}: {reason}", file.path.display()))
        })?);
    }
    let problems = instance_record_problems(hierarchy, &symbols, project);
    if !problems.is_empty() {
        const SHOWN: usize = 5;
        let more = problems.len().saturating_sub(SHOWN);
        return Err(HierarchyPlanError::Records(format!(
            "{} instance record problem(s) in the hierarchy: {}{}. eeschema creates missing \
             records, prunes obsolete ones and renames the project's when it opens and saves the \
             project; do that, or repair the records, then call again",
            problems.len(),
            problems[..problems.len().min(SHOWN)].join("; "),
            if more > 0 {
                format!("; and {more} more")
            } else {
                String::new()
            }
        )));
    }
    let mut order = SheetOrder::default();
    let mut file_of = BTreeMap::new();
    for instance in &hierarchy.instances {
        order
            .sheet_number
            .insert(instance.path.clone(), instance.sheet_number);
        order.schematic.insert(
            instance.path.clone(),
            hierarchy.files[instance.file].path.display().to_string(),
        );
        file_of.insert(instance.path.clone(), instance.file);
    }
    let flat: Vec<PlacedSymbol> = symbols.iter().flatten().cloned().collect();
    let plan = plan_annotation_in(
        &flat,
        project,
        &Reserved::new(),
        resolve_duplicates,
        Some(&order),
    );
    Ok(HierarchyPlan {
        symbols,
        file_of,
        plan,
    })
}

/// One file's share of a hierarchy plan, for its edits and their proof.
fn file_plan(hierarchy: &SavedHierarchy, planned: &HierarchyPlan, file: usize) -> AnnotationPlan {
    AnnotationPlan {
        assignments: planned
            .plan
            .assignments
            .iter()
            .filter(|a| planned.file_of.get(&a.path) == Some(&file))
            .cloned()
            .collect(),
        ordinal_path: hierarchy
            .instances
            .iter()
            .find(|instance| instance.file == file)
            .map(|instance| instance.path.clone()),
        ..AnnotationPlan::default()
    }
}

fn hierarchy_body(
    path: &Path,
    options: &AnnotateOptions,
    plan: &AnnotationPlan,
    hierarchy: &SavedHierarchy,
    written: bool,
) -> Value {
    let others: Vec<String> = hierarchy.files[1..]
        .iter()
        .map(|file| file.path.display().to_string())
        .collect();
    let mut response = body(path, options, plan, (&others, &[]), written);
    response["schematic_scope"] = json!("hierarchy");
    let schematic_of: BTreeMap<&str, String> = hierarchy
        .instances
        .iter()
        .map(|instance| {
            let file = &hierarchy.files[instance.file];
            (instance.path.as_str(), file.path.display().to_string())
        })
        .collect();
    if let Some(assigned) = response["assigned"].as_array_mut() {
        for entry in assigned {
            let schematic = entry["path"]
                .as_str()
                .and_then(|p| schematic_of.get(p))
                .cloned();
            entry["schematic"] = json!(schematic);
        }
    }
    response["sheets"] = hierarchy
        .instances
        .iter()
        .map(|instance| {
            let mut sheet = json!({
                "path": instance.path,
                "schematic": hierarchy.files[instance.file].path.display().to_string(),
                "page": instance.page,
                "sheet_number": instance.sheet_number,
            });
            if instance.page_in_file.as_deref() != Some(instance.page.as_str()) {
                sheet["page_in_file"] = json!(instance.page_in_file);
            }
            sheet
        })
        .collect();
    response
}

/// `schematic_scope: "hierarchy"`: annotate every sheet instance of the
/// project whose root `path` is, and report exactly what happened.
pub(crate) fn annotate_hierarchy(path: &Path, options: AnnotateOptions) -> CallToolResult {
    annotate_hierarchy_with_persistence(path, options, |directory, transitions| {
        commit_file_transaction(directory, transitions).map(|_| ())
    })
}

/// Whether `root` is its project's only top-level sheet, as eeschema 10 reads
/// `schematic.top_level_sheets`: none listed means the project's own
/// schematic, and an entry's uuid, unless nil, replaces the sheet's own.
fn sole_top_level_sheet(project_file: &Path, root: &Path, root_uuid: &str) -> Result<(), String> {
    const NIL: &str = "00000000-0000-0000-0000-000000000000";
    let shown = project_file.display();
    let settings: Value = std::fs::read_to_string(project_file)
        .map_err(|error| format!("{shown} cannot be read: {error}"))
        .and_then(|text| {
            serde_json::from_str(&text).map_err(|error| format!("{shown} is not JSON: {error}"))
        })?;
    let sheets = match settings.pointer("/schematic/top_level_sheets") {
        None | Some(Value::Null) => return Ok(()),
        Some(Value::Array(sheets)) if sheets.is_empty() => return Ok(()),
        Some(Value::Array(sheets)) => sheets,
        Some(_) => {
            return Err(format!(
                "{shown}'s schematic.top_level_sheets is not a list"
            ))
        }
    };
    let directory = project_file.parent().unwrap_or_else(|| Path::new(""));
    let files: Vec<&str> = sheets
        .iter()
        .map(|sheet| sheet["filename"].as_str().unwrap_or("?"))
        .collect();
    match sheets.as_slice() {
        [sheet]
            if sheet["filename"].as_str().is_some_and(|file| {
                crate::tools::same_schematic_document(&directory.join(file), root)
            }) =>
        {
            match sheet["uuid"].as_str() {
                None => Ok(()),
                Some(uuid) if uuid == NIL || uuid == root_uuid => Ok(()),
                Some(uuid) => Err(format!(
                    "{shown} gives the root the uuid {uuid}, not its own {root_uuid}, so its \
                     sheet instances are not the paths its files record; nothing was written"
                )),
            }
        }
        _ => Err(format!(
            "{shown} lists the top-level sheets {files:?}; eeschema annotates them as one \
             hierarchy and this walks only {}, so nothing was written",
            root.display()
        )),
    }
}

/// Annotate every sheet instance under the project root `path` as one
/// change. Persistence is injectable as in [`annotate_file_with_persistence`];
/// production commits one transaction journalled in the root's directory.
fn annotate_hierarchy_with_persistence(
    path: &Path,
    options: AnnotateOptions,
    persist: impl FnOnce(&Path, Vec<FileTransition>) -> Result<(), SexpError>,
) -> CallToolResult {
    let invalid = |field: &str, reason: String| {
        CallToolResult::error_kind(
            ToolErrorKind::InvalidArgument {
                field: field.to_string(),
                reason: reason.clone(),
            },
            reason,
        )
    };
    let conflict = |paths: Vec<String>, message: String| {
        CallToolResult::error_kind(ToolErrorKind::Conflict { paths }, message)
    };
    let uncertain = |reason: String, requested: usize| {
        outcome::attach(
            crate::tools::mutation_outcome_uncertain(path, OPERATION, reason),
            outcome::summary(
                OutcomeStatus::Uncertain,
                path.display().to_string(),
                "saved_file_readback",
                requested,
                0,
                requested,
                Some(outcome::inspect_before_retry()),
            ),
        )
    };

    // The hierarchy is the one the project's root proves. Nothing here falls
    // back to annotating the file alone.
    let ownership = match crate::tools::resolve_schematic_ownership(path) {
        Ok(Some(ownership)) => ownership,
        Ok(None) => {
            return invalid(
                "schematic_scope",
                format!(
                    "no .kicad_pro owns {}, so it roots no project hierarchy; use \
                     schematic_scope 'file' to annotate the file alone",
                    path.display()
                ),
            )
        }
        Err(error) => return error.into_tool_result(),
    };
    if !crate::tools::same_schematic_document(&ownership.root_schematic, path) {
        let root = ownership.root_schematic.display();
        return invalid(
            "schematic",
            format!(
                "{} is a sheet inside the project rooted at {root}; a hierarchy is annotated \
                 from its root, so call it on {root}",
                path.display()
            ),
        );
    }
    let project = ownership
        .project_file
        .file_stem()
        .map(|stem| stem.to_string_lossy().into_owned())
        .unwrap_or_default();
    if let Some(requested) = options.project.as_deref().filter(|r| *r != project) {
        return invalid(
            "project",
            format!(
                "'{requested}' is not the project that owns {}; hierarchy scope annotates \
                 '{project}'",
                path.display()
            ),
        );
    }

    // A transaction another writer left unfinished settles first, so the plan
    // is read from what the commit will be checked against.
    let directory = match path
        .canonicalize()
        .map(|p| p.parent().map(Path::to_path_buf))
    {
        Ok(Some(directory)) => directory,
        _ => {
            return CallToolResult::error_kind(
                ToolErrorKind::FileNotFound {
                    path: path.display().to_string(),
                },
                format!("Schematic not found: {}", path.display()),
            )
        }
    };
    let shown = directory.display();
    let journals = if options.dry_run {
        inspect_file_transactions(&directory).map(|pending| pending.len())
    } else {
        recover_file_transactions(&directory).map(|_| 0)
    };
    match journals {
        Ok(0) => {}
        Ok(pending) => {
            return conflict(
                vec![shown.to_string()],
                format!(
                    "{pending} unfinished transaction(s) in {shown}; a dry run writes nothing, so \
                     it does not settle them. Run without dry_run, which settles them first, or \
                     inspect them with `copperpour transaction status {shown}`."
                ),
            )
        }
        Err(error) => {
            return conflict(
                vec![shown.to_string()],
                format!(
                    "An unfinished transaction in {shown} could not be settled ({error}); \
                     inspect it with `copperpour transaction status {shown}`."
                ),
            )
        }
    }

    let hierarchy = match load_saved_hierarchy(path) {
        Ok(hierarchy) => hierarchy,
        Err(error) => return error.into_tool_result(),
    };
    // eeschema annotates every top-level sheet its project lists as one
    // hierarchy; this walks one root, so it refuses rather than number without
    // the others.
    if let Err(reason) = sole_top_level_sheet(&ownership.project_file, path, &hierarchy.root_uuid) {
        return invalid("schematic_scope", reason);
    }
    // eeschema locks the root it opened and holds every sheet in memory, so a
    // lock beside any file of the hierarchy refuses the whole call. Anything
    // at the lock path counts, as the writer counts it.
    let locked: Vec<String> = hierarchy
        .files
        .iter()
        .filter_map(|file| kicad_editor_lock_path(&file.path))
        .filter(|lock| {
            !matches!(std::fs::symlink_metadata(lock),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound)
        })
        .map(|lock| lock.display().to_string())
        .collect();
    if !locked.is_empty() {
        let message = format!(
            "Schematic hierarchy {} has a KiCad editor lock ({}). Close Eeschema, or resolve a \
             stale lock only after confirming no editor owns the file, then retry.",
            path.display(),
            locked.join(", ")
        );
        return conflict(locked, message);
    }
    let planned = match plan_hierarchy(&hierarchy, &project, options.resolve_duplicates) {
        Ok(planned) => planned,
        Err(HierarchyPlanError::Unreadable(reason)) => {
            return CallToolResult::error_kind(
                ToolErrorKind::HandlerError {
                    reason: reason.clone(),
                },
                format!("Could not read the schematic's symbols: {reason}"),
            )
        }
        Err(HierarchyPlanError::Records(reason)) => {
            return refused_hierarchy(path, &AnnotationPlan::default(), reason)
        }
    };
    let plan = &planned.plan;
    let target = path.display().to_string();
    let requested = plan.assignments.len() + plan.unresolved.len();
    let instances = hierarchy.instances.len();

    // Preflight, dry run included: each file's prospective text must show its
    // assignments and nothing else changed, and every file to be written must
    // sit where the journal can cover it.
    let mut transitions = Vec::new();
    for (index, file) in hierarchy.files.iter().enumerate() {
        let file_plan = file_plan(&hierarchy, &planned, index);
        if file_plan.assignments.is_empty() {
            continue;
        }
        if !file.canonical.starts_with(&directory) {
            return refused_hierarchy(
                path,
                plan,
                format!(
                    "{} lies outside {shown}, the project directory the transaction is \
                     journalled in; annotate it on its own with schematic_scope 'file', or move \
                     it under the project",
                    file.path.display()
                ),
            );
        }
        let edits = match plan_edits(&file.content, &planned.symbols[index], &file_plan) {
            Ok(edits) => edits,
            Err(reason) => return refused_hierarchy(path, plan, reason),
        };
        let prospective = apply_edits(file.content.clone(), edits);
        if let Err(reason) = parse_file(&file.path, &prospective)
            .and_then(|after| verify_applied(&planned.symbols[index], &after, &file_plan))
        {
            return refused_hierarchy(path, plan, reason);
        }
        transitions.push(FileTransition::replace(
            &file.canonical,
            file.content.clone(),
            prospective,
        ));
    }

    if options.dry_run || plan.assignments.is_empty() {
        let status = if plan.unresolved.is_empty() {
            OutcomeStatus::Complete
        } else {
            OutcomeStatus::Partial
        };
        let mut response = hierarchy_body(path, &options, plan, &hierarchy, false);
        response["message"] = json!(if options.dry_run {
            "Dry run: nothing was written; assigned lists what a real run would write.".to_string()
        } else if plan.unresolved.is_empty() {
            format!(
                "Nothing to annotate: every designator is numbered and unique across the \
                 hierarchy's {instances} sheet instance(s)."
            )
        } else if plan.unannotated_before == 0 {
            "Nothing was written: no designator needed a number, but the listed groups are \
             unresolved."
                .to_string()
        } else {
            "Nothing was written: no designator could be numbered, and the listed groups are \
             unresolved."
                .to_string()
        });
        return outcome::attach(
            CallToolResult::json(&response),
            outcome::summary(
                status,
                target,
                if options.dry_run {
                    "dry_run_plan"
                } else {
                    "saved_file"
                },
                requested,
                plan.assignments.len(),
                plan.unresolved.len(),
                retry_for(plan, options.resolve_duplicates),
            ),
        );
    }

    let written: Vec<String> = transitions
        .iter()
        .map(|transition| transition.path().display().to_string())
        .collect();
    if let Err(error) = persist(&directory, transitions) {
        // As in file scope, no persistence error proves that nothing was
        // applied; the journal says what was.
        return uncertain(
            format!(
                "the transaction over {} failed: {error}; `copperpour transaction status {shown}` \
                 shows what was applied",
                written.join(", ")
            ),
            requested,
        );
    }

    // Readback: the whole hierarchy, reloaded. The status and remaining_* come
    // from it, and every assignment is proven in it.
    let committed = match load_saved_hierarchy(path) {
        Ok(committed) => committed,
        Err(error) => {
            return uncertain(
                format!("the saved hierarchy could not be reloaded: {error:?}"),
                requested,
            )
        }
    };
    for (index, file) in hierarchy.files.iter().enumerate() {
        let file_plan = file_plan(&hierarchy, &planned, index);
        if file_plan.assignments.is_empty() {
            continue;
        }
        let proven = committed
            .files
            .iter()
            .find(|saved| saved.canonical == file.canonical)
            .ok_or_else(|| format!("{} left the hierarchy", file.path.display()))
            .and_then(|saved| placed_symbols(&saved.tree))
            .and_then(|after| verify_applied(&planned.symbols[index], &after, &file_plan));
        if let Err(reason) = proven {
            return uncertain(
                format!("the saved hierarchy does not show the annotation as planned: {reason}"),
                requested,
            );
        }
    }
    let remaining = match plan_hierarchy(&committed, &project, options.resolve_duplicates) {
        Ok(remaining) => remaining.plan,
        Err(HierarchyPlanError::Unreadable(reason) | HierarchyPlanError::Records(reason)) => {
            return uncertain(
                format!("the saved hierarchy no longer plans cleanly: {reason}"),
                requested,
            )
        }
    };
    let status = if remaining.assignments.is_empty() && remaining.unresolved.is_empty() {
        OutcomeStatus::Complete
    } else {
        OutcomeStatus::Partial
    };
    let mut response = hierarchy_body(path, &options, plan, &committed, true);
    response["remaining_unannotated"] = json!(remaining.unannotated_before);
    response["remaining_unresolved"] = json!(remaining.unresolved);
    response["message"] = json!(match status {
        OutcomeStatus::Complete => format!(
            "Annotated; every designator is numbered and unique across the hierarchy's \
             {instances} sheet instance(s)."
        ),
        _ => "Annotated what could be decided; remaining_unresolved lists what the hierarchy, \
              read back, still needs."
            .to_string(),
    });
    outcome::attach(
        CallToolResult::json(&response),
        outcome::summary(
            status,
            target,
            "saved_file_readback",
            requested,
            plan.assignments.len(),
            plan.unresolved.len(),
            retry_for(&remaining, options.resolve_duplicates),
        ),
    )
}

/// A refusal before any write: the prospective text did not prove the plan.
fn refused(
    path: &Path,
    plan: &AnnotationPlan,
    options: &AnnotateOptions,
    reason: String,
) -> CallToolResult {
    let _ = options;
    refused_unchanged(path, plan, reason, &format!("{} is", path.display()))
}

/// [`refused`] for a whole hierarchy: no file of it was written.
fn refused_hierarchy(path: &Path, plan: &AnnotationPlan, reason: String) -> CallToolResult {
    let unchanged = format!(
        "every file of the hierarchy rooted at {} is",
        path.display()
    );
    refused_unchanged(path, plan, reason, &unchanged)
}

fn refused_unchanged(
    path: &Path,
    plan: &AnnotationPlan,
    reason: String,
    unchanged: &str,
) -> CallToolResult {
    let requested = plan.assignments.len() + plan.unresolved.len();
    let result = CallToolResult::error_kind(
        ToolErrorKind::StaleTarget {
            target: path.display().to_string(),
            reason: reason.clone(),
        },
        format!("Annotation refused before writing; {unchanged} unchanged: {reason}"),
    );
    outcome::attach(
        result,
        outcome::summary(
            OutcomeStatus::Failed,
            path.display().to_string(),
            "saved_file",
            requested,
            0,
            requested,
            Some(outcome::inspect_before_retry()),
        ),
    )
}

// ─── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mcp::error::extract_error_kind;
    use crate::mcp::protocol::ToolContent;

    #[test]
    fn persistence_conflicts_require_inspection_before_retry() {
        for after_replacement in [true, false] {
            let (_dir, path) = write_fixture(BEFORE);
            let result = annotate_file_with_persistence(
                &path,
                AnnotateOptions::default(),
                |target, expected, prospective| {
                    if after_replacement {
                        // Execute the real conditional atomic replacement, then
                        // simulate the writer's post-write readback conflict.
                        write_atomic_if_unchanged(target, expected, prospective).unwrap();
                    }
                    Err(SexpError::Conflict {
                        path: target.to_path_buf(),
                    })
                },
            );
            let saved = std::fs::read_to_string(&path).unwrap();
            if after_replacement {
                assert_eq!(designators(&saved), designators(KICAD_KEEP));
            } else {
                assert_eq!(saved, BEFORE);
            }
            let body = response_json(&result);
            assert_eq!(body["outcome"]["status"], "uncertain", "{body}");
            assert_eq!(
                body["outcome"]["retry"],
                outcome::inspect_before_retry(),
                "{body}"
            );
            assert_eq!(
                extract_error_kind(&result).as_deref(),
                Some("mutation_outcome_uncertain")
            );
            assert!(!text_of(&result).contains("nothing was written"));
        }
    }

    /// Built through Copperpour's tools, re-serialised by `kicad-cli sch upgrade`
    /// (eeschema, version 20260306): three `R1` (1k at x=101.6, 2k at
    /// x=50.8, 3k at x=127.0) and two `C?` (100n at x=139.7, 1u at x=63.5).
    const BEFORE: &str = include_str!("../../tests/fixtures/annotate_duplicates.kicad_sch");
    /// The same file after eeschema 10.0.5 Tools → Annotate with its defaults
    /// (keep existing annotations, sort by X): `C1`/`C2` assigned, `R1`s
    /// untouched, two "Duplicate items R1" errors reported.
    const KICAD_KEEP: &str =
        include_str!("../../tests/fixtures/annotate_duplicates.kicad_keep_existing.kicad_sch");
    /// The same file after eeschema's "Reset existing annotations": the
    /// leftmost `R1` (2k) kept its number, 1k became `R2`, 3k `R3`.
    const KICAD_RESET: &str =
        include_str!("../../tests/fixtures/annotate_duplicates.kicad_reset.kicad_sch");
    /// eeschema-authored: U1 is three placed units of one dual triode.
    const MULTI_UNIT: &str = include_str!("../../tests/fixtures/ecc83_multiunit.kicad_sch");
    /// KiCad's `multichannel` demo child sheet, instantiated four times by
    /// its parent: every symbol carries four instance paths.
    const REUSED_SHEET: &str =
        include_str!("../../tests/fixtures/multichannel_channel_strip.kicad_sch");

    const UUID_1K: &str = "3e7f8c3e-69e6-41ab-a78f-765980213126";
    const UUID_2K: &str = "837d10e6-224a-4c0b-975a-139efaa126f3";
    const UUID_3K: &str = "e27496de-5f59-4cbf-a398-22e562bed8a2";
    const UUID_100N: &str = "4795413c-fead-4496-9988-5992855383e1";
    const UUID_1U: &str = "ac80bc97-8707-4117-b008-ef58f4ccc658";

    fn symbols_of(content: &str) -> Vec<PlacedSymbol> {
        placed_symbols(&parse_sexp(content).unwrap()).unwrap()
    }

    /// (uuid, property, [(path, reference)]) for every symbol — the shape a
    /// designator-only comparison needs, independent of serialisation.
    fn designators(content: &str) -> BTreeMap<String, (String, Vec<(String, String)>)> {
        symbols_of(content)
            .into_iter()
            .map(|s| {
                (
                    s.uuid,
                    (
                        s.property_reference,
                        s.paths.into_iter().map(|p| (p.path, p.reference)).collect(),
                    ),
                )
            })
            .collect()
    }

    fn write_fixture(content: &str) -> (tempfile::TempDir, std::path::PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("annotate.kicad_sch");
        std::fs::write(&path, content).unwrap();
        (dir, path)
    }

    fn response_json(result: &CallToolResult) -> Value {
        match result.content.first() {
            Some(ToolContent::Text { text }) => serde_json::from_str(text).unwrap(),
            other => panic!("expected text content, got {other:?}"),
        }
    }

    #[test]
    fn designators_split_and_spell_the_way_eeschema_does() {
        assert_eq!(
            parse_designator("R?"),
            Designator {
                prefix: "R".into(),
                number: None
            }
        );
        assert_eq!(
            parse_designator("R12"),
            Designator {
                prefix: "R".into(),
                number: Some(12)
            }
        );
        assert_eq!(
            parse_designator("R"),
            Designator {
                prefix: "R".into(),
                number: None
            }
        );
        assert_eq!(
            parse_designator("#PWR?"),
            Designator {
                prefix: "#PWR".into(),
                number: None
            }
        );
        assert_eq!(
            parse_designator("#PWR01"),
            Designator {
                prefix: "#PWR".into(),
                number: Some(1)
            }
        );
        assert_eq!(
            parse_designator("#PWR0100"),
            Designator {
                prefix: "#PWR".into(),
                number: Some(100)
            }
        );
        assert_eq!(
            parse_designator("#PWR001"),
            Designator {
                prefix: "#PWR".into(),
                number: Some(1)
            }
        );
        assert_eq!(format_designator("R", 7), "R7");
        assert_eq!(format_designator("#PWR", 1), "#PWR01");
        assert_eq!(format_designator("#PWR", 10), "#PWR010");
        assert_eq!(format_designator("#PWR", 100), "#PWR0100");
        assert_eq!(format_designator("#FLG", 2), "#FLG02");
    }

    #[test]
    fn default_run_matches_eeschema_keep_existing_and_reports_the_duplicates() {
        let (_dir, path) = write_fixture(BEFORE);
        let result = annotate_file(&path, AnnotateOptions::default());
        let body = response_json(&result);
        assert!(!result.is_error, "{body}");

        // The two places eeschema wrote, and nothing else.
        assert_eq!(
            designators(&std::fs::read_to_string(&path).unwrap()),
            designators(KICAD_KEEP)
        );

        assert_eq!(body["written"], true);
        assert_eq!(body["unannotated_before"], 2);
        let assigned = body["assigned"].as_array().unwrap();
        assert_eq!(assigned.len(), 2, "{body}");
        assert_eq!(
            assigned[0]["uuid"], UUID_1U,
            "1u at x=63.5 is annotated first"
        );
        assert_eq!(assigned[0]["to"], "C1");
        assert_eq!(assigned[1]["uuid"], UUID_100N);
        assert_eq!(assigned[1]["to"], "C2");

        let unresolved = body["unresolved"].as_array().unwrap();
        assert_eq!(unresolved.len(), 1, "{body}");
        assert_eq!(unresolved[0]["reference"], "R1");
        assert_eq!(
            unresolved[0]["uuids"],
            json!([UUID_2K, UUID_1K, UUID_3K]),
            "ascending X: 2k, 1k, 3k"
        );
        assert_eq!(body["outcome"]["status"], "partial", "{body}");
        assert_eq!(body["outcome"]["completed"], 2);
        assert_eq!(body["outcome"]["failed"], 1);
        assert_eq!(body["outcome"]["retry"]["scope"], "unresolved_duplicates");
    }

    #[test]
    fn resolve_duplicates_matches_eeschema_reset_on_the_duplicate_groups() {
        let (_dir, path) = write_fixture(BEFORE);
        let result = annotate_file(
            &path,
            AnnotateOptions {
                resolve_duplicates: true,
                dry_run: false,
                project: None,
            },
        );
        let body = response_json(&result);
        assert!(!result.is_error, "{body}");
        assert_eq!(
            designators(&std::fs::read_to_string(&path).unwrap()),
            designators(KICAD_RESET)
        );

        let assigned = body["assigned"].as_array().unwrap();
        let by_uuid: BTreeMap<&str, &str> = assigned
            .iter()
            .map(|a| (a["uuid"].as_str().unwrap(), a["to"].as_str().unwrap()))
            .collect();
        assert_eq!(by_uuid.get(UUID_1K), Some(&"R2"), "{body}");
        assert_eq!(by_uuid.get(UUID_3K), Some(&"R3"), "{body}");
        assert!(
            !by_uuid.contains_key(UUID_2K),
            "the leftmost R1 keeps its number"
        );
        assert_eq!(body["outcome"]["status"], "complete", "{body}");
        assert!(body["unresolved"].as_array().unwrap().is_empty());
        assert!(body["outcome"]["retry"].is_null());
    }

    #[test]
    fn the_property_is_written_alongside_the_instances_block() {
        // Defect 2 of #454: the old annotator rewrote only (instances …).
        let (_dir, path) = write_fixture(BEFORE);
        annotate_file(&path, AnnotateOptions::default());
        let after = designators(&std::fs::read_to_string(&path).unwrap());
        for uuid in [UUID_1U, UUID_100N] {
            let (property, paths) = &after[uuid];
            assert_eq!(
                property, &paths[0].1,
                "{uuid}: property and instances disagree"
            );
            assert!(property.starts_with('C') && !property.ends_with('?'));
        }
    }

    #[test]
    fn a_multi_unit_part_is_not_a_duplicate() {
        let symbols = symbols_of(MULTI_UNIT);
        let plan = plan_annotation(&symbols, &sole_project(&symbols), true);
        let u1_units = symbols
            .iter()
            .filter(|s| s.property_reference == "U1")
            .count();
        assert!(u1_units >= 2, "the fixture places several units of U1");
        assert!(plan.duplicates_before.is_empty(), "{plan:?}");
        assert!(plan.assignments.is_empty(), "{plan:?}");

        let (_dir, path) = write_fixture(MULTI_UNIT);
        let before = std::fs::read(&path).unwrap();
        let body = response_json(&annotate_file(&path, AnnotateOptions::default()));
        assert_eq!(body["written"], false, "{body}");
        assert_eq!(body["outcome"]["status"], "complete", "{body}");
        assert_eq!(
            std::fs::read(&path).unwrap(),
            before,
            "an annotated file is left alone"
        );
    }

    #[test]
    fn units_saved_with_a_copy_of_their_definition_are_still_one_package() {
        // KiCad embeds a copy of a unit's definition that differs from the
        // library's under `Name_1` and points the unit at it with `lib_name`,
        // as KiCad's vme-wren demo does for its FPGA. Same library item, same
        // value, distinct units: still one package, not a guess.
        let definition = MULTI_UNIT.find("(symbol \"ecc83-pp:ECC83\"").unwrap();
        let body_at = definition + MULTI_UNIT[definition..].find('\n').unwrap();
        let (start, end) = find_enclosing_block(MULTI_UNIT, "symbol", body_at).unwrap();
        let copy = MULTI_UNIT[start..end]
            .replace("\"ECC83_", "\"ECC83_1_")
            .replace("\"ecc83-pp:ECC83\"", "\"ECC83_1\"");
        let tube = symbols_of(MULTI_UNIT)
            .into_iter()
            .find(|s| s.property_reference == "U1" && s.unit == 2)
            .unwrap();
        let (placed, _) = symbol_block(MULTI_UNIT, &tube.uuid).unwrap();
        let lib_id = placed + MULTI_UNIT[placed..].find("(lib_id ").unwrap();
        let with_copy = |copy: &str| {
            format!(
                "{}{copy}\n\t\t{}(lib_name \"ECC83_1\")\n\t\t{}",
                &MULTI_UNIT[..end],
                &MULTI_UNIT[end..lib_id],
                &MULTI_UNIT[lib_id..]
            )
        };
        let derived = with_copy(&copy);
        let renamed = symbols_of(&derived)
            .into_iter()
            .find(|s| s.uuid == tube.uuid)
            .unwrap();
        assert_eq!(renamed.lib_symbol, "ECC83_1");
        assert_eq!(
            renamed.unit_count, tube.unit_count,
            "the copy declares its units"
        );
        let (_dir, path) = write_fixture(&derived);
        let body = response_json(&annotate_file(&path, AnnotateOptions::default()));
        assert_eq!(body["outcome"]["status"], "complete", "{body}");
        assert_eq!(body["unresolved"], json!([]), "{body}");

        // A copy that declares fewer units is not proven to be the same part.
        let unit3 = copy.find("(symbol \"ECC83_1_3_1\"").unwrap();
        let unit3_at = unit3 + copy[unit3..].find('\n').unwrap();
        let (start, end) = find_enclosing_block(&copy, "symbol", unit3_at).unwrap();
        let shorter = format!("{}{}", &copy[..start], &copy[end..]);
        let (_dir, path) = write_fixture(&with_copy(&shorter));
        let body = response_json(&annotate_file(&path, AnnotateOptions::default()));
        assert_eq!(body["outcome"]["status"], "partial", "{body}");
        assert_eq!(
            body["unresolved"][0]["reason"], REASON_UNPROVEN_PACKAGE,
            "{body}"
        );

        // Unannotated, the same three units take one number; with the
        // shorter copy they are left alone.
        let unannotated = reset_designators(&derived, |prefix| prefix == "U");
        let (_dir, path) = write_fixture(&unannotated);
        let body = response_json(&annotate_file(&path, AnnotateOptions::default()));
        let numbers: BTreeSet<&str> = body["assigned"]
            .as_array()
            .unwrap()
            .iter()
            .map(|a| a["to"].as_str().unwrap())
            .collect();
        assert_eq!(numbers, BTreeSet::from(["U1"]), "{body}");
        let unannotated = reset_designators(&with_copy(&shorter), |prefix| prefix == "U");
        let (_dir, path) = write_fixture(&unannotated);
        let body = response_json(&annotate_file(&path, AnnotateOptions::default()));
        assert_eq!(body["assigned"], json!([]), "{body}");
        assert_eq!(
            body["unresolved"][0]["reason"], REASON_UNPROVEN_NEW_PACKAGE,
            "{body}"
        );
    }

    #[test]
    fn a_reused_sheet_is_numbered_per_instance_path() {
        let symbols = symbols_of(REUSED_SHEET);
        assert!(
            symbols.iter().all(|s| s.paths.len() == 4),
            "four instances per symbol"
        );
        let plan = plan_annotation(&symbols, &sole_project(&symbols), false);
        assert_eq!(plan.paths.len(), 4);
        assert!(
            plan.duplicates_before.is_empty() && plan.assignments.is_empty(),
            "{plan:?}"
        );

        // Derived input, stated rather than hidden: on ONE instance path only,
        // the second symbol takes the first symbol's designator.
        let first = &symbols[0];
        let second = &symbols[1];
        let path0 = first.paths[0].path.clone();
        let clash = first.paths[0].reference.clone();
        let (start, end) = symbol_block(REUSED_SHEET, &second.uuid).unwrap();
        let block = &REUSED_SHEET[start..end];
        let path_needle = format!("(path \"{path0}\"");
        let rel = block.find(&path_needle).unwrap();
        let path_block = find_enclosing_block(REUSED_SHEET, "path", start + rel + 1).unwrap();
        let edit = replace_quoted_after(REUSED_SHEET, path_block, "(reference \"", &clash, "test")
            .unwrap();
        let derived = apply_edits(REUSED_SHEET.to_string(), vec![edit]);

        let plan = plan_annotation(
            &symbols_of(&derived),
            &sole_project(&symbols_of(&derived)),
            true,
        );
        assert_eq!(plan.duplicates_before.len(), 1, "{plan:?}");
        assert_eq!(plan.duplicates_before[0].paths, vec![path0.clone()]);
        assert_eq!(
            plan.assignments.len(),
            1,
            "only the clashing instance is renumbered"
        );
        assert_eq!(plan.assignments[0].path, path0);

        let (_dir, path) = write_fixture(&derived);
        let body = response_json(&annotate_file(
            &path,
            AnnotateOptions {
                resolve_duplicates: true,
                dry_run: false,
                project: None,
            },
        ));
        assert_eq!(body["outcome"]["status"], "complete", "{body}");
        let after = designators(&std::fs::read_to_string(&path).unwrap());
        let before = designators(REUSED_SHEET);
        for (uuid, (_, paths_before)) in &before {
            for (p, reference) in paths_before {
                if !(uuid == &second.uuid && p == &path0) {
                    let (_, paths_after) = &after[uuid];
                    let got = &paths_after.iter().find(|(q, _)| q == p).unwrap().1;
                    assert_eq!(got, reference, "{uuid} on {p} must be untouched");
                }
            }
        }
    }

    #[test]
    fn the_first_free_number_fills_a_gap_as_eeschema_documents() {
        // Derived from BEFORE: 1k becomes R3 in both places, 3k becomes R?.
        let mut derived = BEFORE.to_string();
        for (uuid, from, to) in [(UUID_1K, "R1", "R3"), (UUID_3K, "R1", "R?")] {
            let block = symbol_block(&derived, uuid).unwrap();
            let edits = vec![
                replace_quoted_after(&derived, block, "(property \"Reference\" \"", to, "t")
                    .unwrap(),
                replace_quoted_after(&derived, block, "(reference \"", to, "t").unwrap(),
            ];
            assert_eq!(&derived[edits[0].start..edits[0].end], from);
            derived = apply_edits(derived, edits);
        }
        let plan = plan_annotation(
            &symbols_of(&derived),
            &sole_project(&symbols_of(&derived)),
            false,
        );
        let r = plan
            .assignments
            .iter()
            .find(|a| a.uuid == UUID_3K)
            .expect("3k is assigned");
        assert_eq!(
            r.to, "R2",
            "R1 and R3 are taken, so the first free number is 2: {plan:?}"
        );
    }

    #[test]
    fn a_designator_shared_by_different_symbols_with_distinct_units_is_never_guessed() {
        // Derived from BEFORE: 2k becomes a Device:C unit 2, 3k a unit 3. The
        // R1 group now looks like a multi-unit part across two libraries.
        let mut derived = BEFORE.to_string();
        let (s, e) = symbol_block(&derived, UUID_2K).unwrap();
        let block = derived[s..e]
            .replacen("(lib_id \"Device:R\")", "(lib_id \"Device:C\")", 1)
            .replacen("(unit 1)", "(unit 2)", 1);
        derived.replace_range(s..e, &block);
        let (s, e) = symbol_block(&derived, UUID_3K).unwrap();
        let block = derived[s..e].replacen("(unit 1)", "(unit 3)", 1);
        derived.replace_range(s..e, &block);

        let plan = plan_annotation(
            &symbols_of(&derived),
            &sole_project(&symbols_of(&derived)),
            true,
        );
        assert_eq!(plan.unresolved.len(), 1, "{plan:?}");
        assert_eq!(plan.unresolved[0].reason, REASON_AMBIGUOUS);
        assert!(
            plan.assignments.iter().all(|a| a.from == "C?"),
            "only the C? symbols are assigned: {plan:?}"
        );
    }

    #[test]
    fn a_dry_run_writes_nothing_and_reports_the_same_plan() {
        let (_dir, path) = write_fixture(BEFORE);
        let before = std::fs::read(&path).unwrap();
        let body = response_json(&annotate_file(
            &path,
            AnnotateOptions {
                resolve_duplicates: true,
                dry_run: true,
                project: None,
            },
        ));
        assert_eq!(std::fs::read(&path).unwrap(), before);
        assert_eq!(body["dry_run"], true);
        assert_eq!(body["written"], false);
        assert_eq!(body["assigned"].as_array().unwrap().len(), 4, "{body}");
        assert_eq!(body["outcome"]["source"], "dry_run_plan");
        assert_eq!(body["outcome"]["status"], "complete");
    }

    #[test]
    fn a_missing_schematic_is_file_not_found() {
        let dir = tempfile::tempdir().unwrap();
        let result = annotate_file(
            &dir.path().join("absent.kicad_sch"),
            AnnotateOptions::default(),
        );
        assert!(result.is_error);
        assert_eq!(
            extract_error_kind(&result).as_deref(),
            Some("file_not_found")
        );
    }

    #[test]
    fn a_plan_that_does_not_survive_preflight_writes_nothing() {
        // A plan naming a symbol the file does not have cannot be realised;
        // the refusal must leave the file byte-identical.
        let (_dir, path) = write_fixture(BEFORE);
        let before = std::fs::read(&path).unwrap();
        let content = std::fs::read_to_string(&path).unwrap();
        let symbols = symbols_of(&content);
        let mut plan = plan_annotation(&symbols, &sole_project(&symbols), false);
        plan.assignments.push(Assignment {
            uuid: "00000000-0000-0000-0000-00000000dead".into(),
            unit: 1,
            project: "adj".into(),
            path: "/nowhere".into(),
            from: "X?".into(),
            to: "X1".into(),
        });
        let error = plan_edits(&content, &symbols, &plan).unwrap_err();
        assert!(error.contains("unknown symbol"), "{error}");
        let result = refused(&path, &plan, &AnnotateOptions::default(), error);
        assert_eq!(extract_error_kind(&result).as_deref(), Some("stale_target"));
        assert_eq!(std::fs::read(&path).unwrap(), before);
        let body = response_json(&result);
        assert_eq!(body["outcome"]["status"], "failed");
        assert_eq!(body["outcome"]["completed"], 0);
    }

    fn sole_project(symbols: &[PlacedSymbol]) -> String {
        let named: BTreeSet<&str> = symbols
            .iter()
            .flat_map(|s| s.paths.iter().map(|p| p.project.as_str()))
            .collect();
        assert_eq!(named.len(), 1, "fixture names one project: {named:?}");
        named.into_iter().next().unwrap().to_string()
    }

    /// The Reference property and every `(reference "…")` inside the symbol
    /// block carrying `uuid`, set to `reference`.
    fn set_every_reference(content: &str, uuid: &str, reference: &str) -> String {
        let (start, end) = symbol_block(content, uuid).unwrap();
        let mut block = String::new();
        let mut rest = &content[start..end];
        while let Some(i) = rest.find("(reference \"") {
            let value_start = i + "(reference \"".len();
            let value_len = rest[value_start..].find('"').unwrap();
            block.push_str(&rest[..value_start]);
            block.push_str(reference);
            rest = &rest[value_start + value_len..];
        }
        block.push_str(rest);
        let needle = "(property \"Reference\" \"";
        let i = block.find(needle).unwrap() + needle.len();
        let len = block[i..].find('"').unwrap();
        block.replace_range(i..i + len, reference);
        let mut derived = content.to_string();
        derived.replace_range(start..end, &block);
        derived
    }

    fn text_of(result: &CallToolResult) -> String {
        match result.content.first() {
            Some(ToolContent::Text { text }) => text.clone(),
            other => panic!("expected text content, got {other:?}"),
        }
    }

    #[test]
    fn unannotated_units_of_one_package_share_one_reference() {
        let symbols = symbols_of(MULTI_UNIT);
        let units: Vec<&PlacedSymbol> = symbols
            .iter()
            .filter(|s| s.property_reference == "U1")
            .collect();
        assert_eq!(units.len(), 3, "the ecc83 places three units of U1");
        assert_eq!(
            units[0].unit_count,
            Some(3),
            "the embedded ECC83 definition declares three units"
        );
        let mut derived = MULTI_UNIT.to_string();
        for unit in &units {
            derived = set_every_reference(&derived, &unit.uuid, "U?");
        }
        let derived_symbols = symbols_of(&derived);
        let plan = plan_annotation(&derived_symbols, &sole_project(&derived_symbols), false);
        assert_eq!(plan.unannotated_before, 3, "{plan:?}");
        let u: Vec<&Assignment> = plan.assignments.iter().filter(|a| a.from == "U?").collect();
        assert_eq!(u.len(), 3, "{plan:?}");
        assert!(
            u.iter().all(|a| a.to == "U1"),
            "one package takes one number: {plan:?}"
        );
        assert!(plan.unresolved.is_empty(), "{plan:?}");

        let (_dir, path) = write_fixture(&derived);
        let body = response_json(&annotate_file(&path, AnnotateOptions::default()));
        assert_eq!(body["outcome"]["status"], "complete", "{body}");
        assert_eq!(body["written"], true, "{body}");
        let after = symbols_of(&std::fs::read_to_string(&path).unwrap());
        let mut placed_units: Vec<u32> = Vec::new();
        for unit in &units {
            let observed = after.iter().find(|s| s.uuid == unit.uuid).unwrap();
            assert_eq!(observed.property_reference, "U1", "{}", unit.uuid);
            assert_eq!(observed.paths.len(), 1);
            assert_eq!(observed.paths[0].reference, "U1");
            placed_units.push(observed.unit);
        }
        placed_units.sort_unstable();
        assert_eq!(placed_units, vec![1, 2, 3], "the package keeps its units");
    }

    #[test]
    fn unannotated_units_forming_two_packages_are_not_guessed() {
        let symbols = symbols_of(MULTI_UNIT);
        let units: Vec<String> = symbols
            .iter()
            .filter(|s| s.property_reference == "U1")
            .map(|s| s.uuid.clone())
            .collect();
        let mut derived = MULTI_UNIT.to_string();
        for uuid in &units {
            derived = set_every_reference(&derived, uuid, "U?");
        }
        // The third unit becomes a second unit 1: two packages' worth of units.
        let third = symbols
            .iter()
            .find(|s| s.property_reference == "U1" && s.unit == 3)
            .unwrap();
        let (s, e) = symbol_block(&derived, &third.uuid).unwrap();
        let block = derived[s..e].replacen("(unit 3)", "(unit 1)", 1);
        derived.replace_range(s..e, &block);

        let derived_symbols = symbols_of(&derived);
        let plan = plan_annotation(&derived_symbols, &sole_project(&derived_symbols), false);
        assert!(
            plan.assignments.iter().all(|a| a.from != "U?"),
            "no unit is numbered: {plan:?}"
        );
        let unresolved = plan
            .unresolved
            .iter()
            .find(|u| u.reference == "U?")
            .expect("the units are reported");
        assert_eq!(unresolved.reason, REASON_UNPROVEN_NEW_PACKAGE);
        assert_eq!(unresolved.uuids.len(), 3);

        let (_dir, path) = write_fixture(&derived);
        let before = std::fs::read(&path).unwrap();
        let body = response_json(&annotate_file(&path, AnnotateOptions::default()));
        assert_eq!(body["outcome"]["status"], "partial", "{body}");
        assert_eq!(body["written"], false, "{body}");
        assert_eq!(std::fs::read(&path).unwrap(), before, "nothing is guessed");
    }

    #[test]
    fn a_reused_sheet_reserves_numbers_project_wide() {
        let symbols = symbols_of(REUSED_SHEET);
        let resistor = symbols
            .iter()
            .find(|s| s.property_reference.starts_with('R'))
            .unwrap();
        assert_eq!(resistor.paths.len(), 4, "four instances of the sheet");
        let derived = set_every_reference(REUSED_SHEET, &resistor.uuid, "R?");
        let derived_symbols = symbols_of(&derived);
        let project = sole_project(&derived_symbols);
        let taken: BTreeSet<String> = derived_symbols
            .iter()
            .filter(|s| s.uuid != resistor.uuid)
            .flat_map(|s| s.paths.iter().map(|p| p.reference.clone()))
            .collect();

        let plan = plan_annotation(&derived_symbols, &project, false);
        let own: Vec<&Assignment> = plan
            .assignments
            .iter()
            .filter(|a| a.uuid == resistor.uuid)
            .collect();
        assert_eq!(own.len(), 4, "one assignment per instance path: {plan:?}");
        let numbers: BTreeSet<&str> = own.iter().map(|a| a.to.as_str()).collect();
        assert_eq!(numbers.len(), 4, "four distinct numbers: {plan:?}");
        assert!(
            numbers.iter().all(|n| !taken.contains(*n)),
            "a number used on any instance is never reused: {plan:?}"
        );
        assert_eq!(plan.assignments.len(), 4, "{plan:?}");
        assert!(plan.unresolved.is_empty(), "{plan:?}");

        let (_dir, path) = write_fixture(&derived);
        let body = response_json(&annotate_file(&path, AnnotateOptions::default()));
        assert_eq!(body["outcome"]["status"], "complete", "{body}");
        assert_eq!(body["project"], project, "{body}");
        let after = symbols_of(&std::fs::read_to_string(&path).unwrap());
        let all_r: Vec<String> = after
            .iter()
            .filter(|s| s.property_reference.starts_with('R'))
            .flat_map(|s| s.paths.iter().map(|p| p.reference.clone()))
            .collect();
        let distinct: BTreeSet<&String> = all_r.iter().collect();
        let resistors = after
            .iter()
            .filter(|s| s.property_reference.starts_with('R'))
            .count();
        assert_eq!(
            all_r.len(),
            4 * resistors,
            "every resistor on four instances"
        );
        assert_eq!(
            distinct.len(),
            all_r.len(),
            "every resistor instance keeps a unique reference: {all_r:?}"
        );
    }

    #[test]
    fn separate_projects_sharing_a_path_are_not_duplicates() {
        // 2k's instance record moves to another project on the same sheet
        // path: it is no longer one of adj's R1s, and adj's run leaves it
        // byte-identical.
        let mut derived = BEFORE.to_string();
        let (s, e) = symbol_block(&derived, UUID_2K).unwrap();
        let block = derived[s..e].replacen("(project \"adj\"", "(project \"other\"", 1);
        derived.replace_range(s..e, &block);
        let (s, e) = symbol_block(&derived, UUID_2K).unwrap();
        let other_block = derived[s..e].to_string();

        let symbols = symbols_of(&derived);
        let plan = plan_annotation(&symbols, "adj", false);
        assert_eq!(plan.duplicates_before.len(), 1, "{plan:?}");
        assert_eq!(
            plan.duplicates_before[0].uuids.len(),
            2,
            "1k and 3k only: {plan:?}"
        );
        assert!(!plan.duplicates_before[0]
            .uuids
            .contains(&UUID_2K.to_string()));
        assert_eq!(plan.outside_project.len(), 1, "{plan:?}");
        assert_eq!(plan.outside_project[0].uuid, UUID_2K);
        assert_eq!(plan.outside_project[0].projects, vec!["other".to_string()]);
        let other = plan_annotation(&symbols, "other", true);
        assert!(
            other.duplicates_before.is_empty() && other.assignments.is_empty(),
            "{other:?}"
        );

        let (_dir, path) = write_fixture(&derived);
        let body = response_json(&annotate_file(
            &path,
            AnnotateOptions {
                resolve_duplicates: true,
                dry_run: false,
                project: Some("adj".into()),
            },
        ));
        assert_eq!(body["outcome"]["status"], "complete", "{body}");
        assert_eq!(body["project"], "adj", "{body}");
        let after = std::fs::read_to_string(&path).unwrap();
        let (s, e) = symbol_block(&after, UUID_2K).unwrap();
        assert_eq!(
            &after[s..e],
            other_block,
            "the other project's record is byte-identical"
        );
    }

    #[test]
    fn edits_address_the_selected_projects_instance_entry() {
        // 1k carries records for two projects on the same sheet path, the
        // other project's first. adj's entry is unannotated; only it is written.
        let mut derived = BEFORE.to_string();
        let (s, e) = symbol_block(&derived, UUID_1K).unwrap();
        let block = &derived[s..e];
        let project_start = block.find("(project \"adj\"").unwrap();
        let project_block = find_enclosing_block(block, "project", project_start + 1).unwrap();
        let adj_record = block[project_block.0..project_block.1].to_string();
        let other_record = adj_record
            .replacen("(project \"adj\"", "(project \"other\"", 1)
            .replacen("(reference \"R1\")", "(reference \"R9\")", 1);
        let adj_unannotated = adj_record.replacen("(reference \"R1\")", "(reference \"R?\")", 1);
        let mut new_block = block.to_string();
        new_block.replace_range(
            project_block.0..project_block.1,
            &format!("{other_record}{adj_unannotated}"),
        );
        let needle = "(property \"Reference\" \"R1\"";
        let at = new_block.find(needle).unwrap() + "(property \"Reference\" \"".len();
        new_block.replace_range(at..at + 2, "R?");
        derived.replace_range(s..e, &new_block);

        let symbols = symbols_of(&derived);
        let one_k = symbols.iter().find(|s| s.uuid == UUID_1K).unwrap();
        assert_eq!(one_k.paths.len(), 2);
        assert_eq!(one_k.paths[0].project, "other");
        let plan = plan_annotation(&symbols, "adj", false);
        let a = plan
            .assignments
            .iter()
            .find(|a| a.uuid == UUID_1K)
            .expect("adj's entry is assigned");
        assert_eq!(
            (a.project.as_str(), a.from.as_str(), a.to.as_str()),
            ("adj", "R?", "R2")
        );

        let (_dir, path) = write_fixture(&derived);
        let body = response_json(&annotate_file(
            &path,
            AnnotateOptions {
                resolve_duplicates: false,
                dry_run: false,
                project: Some("adj".into()),
            },
        ));
        assert_eq!(body["written"], true, "{body}");
        let after = symbols_of(&std::fs::read_to_string(&path).unwrap());
        let one_k = after.iter().find(|s| s.uuid == UUID_1K).unwrap();
        let by_project: BTreeMap<&str, &str> = one_k
            .paths
            .iter()
            .map(|p| (p.project.as_str(), p.reference.as_str()))
            .collect();
        assert_eq!(by_project.get("adj"), Some(&"R2"));
        assert_eq!(
            by_project.get("other"),
            Some(&"R9"),
            "the other project's entry on the same path is untouched"
        );
        assert_eq!(one_k.property_reference, "R2");
    }

    #[test]
    fn resolve_duplicates_never_splits_a_multi_unit_package() {
        let symbols = symbols_of(MULTI_UNIT);
        let third = symbols
            .iter()
            .find(|s| s.property_reference == "U1" && s.unit == 3)
            .unwrap();
        let mut derived = MULTI_UNIT.to_string();
        let (s, e) = symbol_block(&derived, &third.uuid).unwrap();
        let block = derived[s..e].replacen("(unit 3)", "(unit 1)", 1);
        derived.replace_range(s..e, &block);

        let derived_symbols = symbols_of(&derived);
        let plan = plan_annotation(&derived_symbols, &sole_project(&derived_symbols), true);
        let group = plan
            .unresolved
            .iter()
            .find(|u| u.reference == "U1")
            .expect("U1 is reported");
        assert_eq!(group.reason, REASON_UNPROVEN_PACKAGE);
        assert_eq!(group.uuids.len(), 3);
        assert!(
            plan.assignments.iter().all(|a| a.from != "U1"),
            "nothing of U1 is renumbered: {plan:?}"
        );

        let (_dir, path) = write_fixture(&derived);
        let before = std::fs::read(&path).unwrap();
        let body = response_json(&annotate_file(
            &path,
            AnnotateOptions {
                resolve_duplicates: true,
                dry_run: false,
                project: None,
            },
        ));
        assert_eq!(body["outcome"]["status"], "partial", "{body}");
        assert_eq!(std::fs::read(&path).unwrap(), before);
    }

    #[test]
    fn project_selection_is_refused_when_ambiguous_and_honoured_when_named() {
        let mut derived = BEFORE.to_string();
        let (s, e) = symbol_block(&derived, UUID_2K).unwrap();
        let block = derived[s..e].replacen("(project \"adj\"", "(project \"other\"", 1);
        derived.replace_range(s..e, &block);
        let (_dir, path) = write_fixture(&derived);
        let before = std::fs::read(&path).unwrap();

        let result = annotate_file(&path, AnnotateOptions::default());
        assert_eq!(
            extract_error_kind(&result).as_deref(),
            Some("invalid_argument"),
            "{result:?}"
        );
        let text = text_of(&result);
        assert!(
            text.contains("adj") && text.contains("other"),
            "the candidates are named: {text}"
        );
        assert_eq!(std::fs::read(&path).unwrap(), before);

        let wrong = annotate_file(
            &path,
            AnnotateOptions {
                resolve_duplicates: false,
                dry_run: true,
                project: Some("nope".into()),
            },
        );
        assert_eq!(
            extract_error_kind(&wrong).as_deref(),
            Some("invalid_argument")
        );

        let named = response_json(&annotate_file(
            &path,
            AnnotateOptions {
                resolve_duplicates: false,
                dry_run: true,
                project: Some("other".into()),
            },
        ));
        assert_eq!(named["project"], "other", "{named}");
        assert_eq!(
            named["outside_project"].as_array().map(Vec::len),
            Some(4),
            "1k, 3k and the two C? belong to adj only: {named}"
        );
    }

    #[test]
    fn numbers_used_on_the_projects_other_sheets_are_reserved() {
        // The multichannel demo's root sheet owns R1..R3; the child sheet,
        // annotated on its own, must not hand those numbers out.
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("multichannel_mixer.kicad_pro"),
            include_str!("../../tests/fixtures/multichannel_mixer.kicad_pro"),
        )
        .unwrap();
        std::fs::write(
            dir.path().join("multichannel_mixer.kicad_sch"),
            include_str!("../../tests/fixtures/multichannel_mixer.kicad_sch"),
        )
        .unwrap();
        let symbols = symbols_of(REUSED_SHEET);
        let resistor = symbols
            .iter()
            .find(|s| s.property_reference.starts_with('R'))
            .unwrap();
        let derived = set_every_reference(REUSED_SHEET, &resistor.uuid, "R?");
        let child = dir.path().join("channel_strip.kicad_sch");
        std::fs::write(&child, &derived).unwrap();

        let root_numbers: BTreeSet<String> = symbols_of(include_str!(
            "../../tests/fixtures/multichannel_mixer.kicad_sch"
        ))
        .iter()
        .flat_map(|s| s.paths.iter().map(|p| p.reference.clone()))
        .filter(|r| r.starts_with('R'))
        .collect();
        assert!(
            root_numbers.contains("R1"),
            "the root sheet owns R1: {root_numbers:?}"
        );
        let (reserved, consulted, unreadable) = reserved_on_other_sheets(
            &dir.path().join("multichannel_mixer.kicad_sch"),
            &child,
            "multichannel_mixer",
        );
        assert_eq!(consulted.len(), 1, "the root only: {consulted:?}");
        assert!(unreadable.is_empty(), "{unreadable:?}");
        assert!(reserved["R"].contains(&1) && reserved["R"].contains(&3));

        let body = response_json(&annotate_file(&child, AnnotateOptions::default()));
        assert_eq!(body["outcome"]["status"], "complete", "{body}");
        assert_eq!(body["project"], "multichannel_mixer", "{body}");
        assert_eq!(
            body["other_sheets_consulted"].as_array().map(Vec::len),
            Some(1),
            "{body}"
        );
        let assigned: Vec<String> = body["assigned"]
            .as_array()
            .unwrap()
            .iter()
            .map(|a| a["to"].as_str().unwrap().to_string())
            .collect();
        assert_eq!(assigned.len(), 4, "{body}");
        assert!(
            assigned.iter().all(|r| !root_numbers.contains(r)),
            "no number the root owns is handed out: {assigned:?}"
        );
    }

    // ─── Hierarchy scope (#42) ─────────────────────────────────────────────

    const NESTED_PRO: &str =
        include_str!("../../tests/fixtures/annotate_nested_hierarchy/nested_pages.kicad_pro");
    /// The nested fixture: the root places `bank` twice (`north` page 10,
    /// `south` page 2), `bank` places `cell` twice (`east` page 3 and `west`
    /// page 4 under `south`, `west` 11 and `east` 12 under `north`).
    const NESTED: [(&str, &str); 3] = [
        (
            "nested_pages.kicad_sch",
            include_str!("../../tests/fixtures/annotate_nested_hierarchy/nested_pages.kicad_sch"),
        ),
        (
            "bank.kicad_sch",
            include_str!("../../tests/fixtures/annotate_nested_hierarchy/bank.kicad_sch"),
        ),
        (
            "cell.kicad_sch",
            include_str!("../../tests/fixtures/annotate_nested_hierarchy/cell.kicad_sch"),
        ),
    ];
    const NESTED_ROOT: &str = "970f1adb-65bd-4ca9-bbaf-b939cab8549c";
    /// On the root, at one X: `north` (page 10, above, first in the file, its
    /// path sorting first) and `south` (page 2). In `bank`: `east` (right,
    /// first in the file) and `west` (left).
    const NORTH: &str = "8cca053b-d9dc-44e3-a2d7-e24a663aa548";
    const SOUTH: &str = "ed5b7508-65bd-450d-b053-99de75ca07ff";
    const EAST: &str = "380650f1-fcab-4acf-8bbe-d7c7771fd834";
    const WEST: &str = "69a89bb1-464e-4f4e-948b-24cc8540975b";

    fn instance(sheets: &[&str]) -> String {
        std::iter::once(NESTED_ROOT)
            .chain(sheets.iter().copied())
            .map(|uuid| format!("/{uuid}"))
            .collect()
    }

    /// The nested fixture as a project in a temporary directory, each sheet
    /// passed through `edit` (file name, content) on the way.
    fn nested_project(
        edit: impl Fn(&str, &str) -> String,
    ) -> (tempfile::TempDir, std::path::PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("nested_pages.kicad_pro"), NESTED_PRO).unwrap();
        for (name, content) in NESTED {
            std::fs::write(dir.path().join(name), edit(name, content)).unwrap();
        }
        let root = dir.path().join("nested_pages.kicad_sch");
        (dir, root)
    }

    /// `cell.kicad_sch` with one designator for a root call to make: the
    /// crystal's record on `north`/`west` (page 11, `Y3`) left `Y?`. Its
    /// `Reference` property shows `south`/`east` (page 3, `Y1`), the first of
    /// its instances in page order, and stays. Every other file as it is.
    fn unannotated_crystal(name: &str, content: &str) -> String {
        if name != "cell.kicad_sch" {
            return content.to_string();
        }
        let page11 = instance(&[NORTH, WEST]);
        let crystal = uuid_at(content, &page11, "Y3");
        let shown = symbols_of(content)
            .into_iter()
            .find(|s| s.uuid == crystal)
            .unwrap()
            .property_reference;
        assert_eq!(shown, "Y1", "the property is not page 11's");
        set_record(content, &crystal, &page11, "Y?")
    }

    /// The root with `C1` and `F1` recording their instance under the project
    /// name `""`.
    fn unnamed_project_records(name: &str, content: &str) -> String {
        if name != "nested_pages.kicad_sch" {
            return content.to_string();
        }
        let root = instance(&[]);
        let mut out = content.to_string();
        for reference in ["C1", "F1"] {
            let uuid = uuid_at(&out, &root, reference);
            let (start, end) = symbol_block(&out, &uuid).unwrap();
            let block = out[start..end].replace("(project \"nested_pages\"", "(project \"\"");
            assert_ne!(block, out[start..end], "{reference} has a record to rename");
            out.replace_range(start..end, &block);
        }
        out
    }

    fn saved(dir: &Path, names: &[&str]) -> Vec<(String, String)> {
        names
            .iter()
            .map(|name| {
                let content = std::fs::read_to_string(dir.join(name)).unwrap();
                (name.to_string(), content)
            })
            .collect()
    }

    fn nested_names() -> Vec<&'static str> {
        NESTED.iter().map(|(name, _)| *name).collect()
    }

    /// (file, uuid, path) → designator for every record, and (file, uuid,
    /// "Reference") → the property.
    fn hierarchy_designators(
        files: &[(String, String)],
    ) -> BTreeMap<(String, String, String), String> {
        let mut out = BTreeMap::new();
        for (name, content) in files {
            for symbol in symbols_of(content) {
                out.insert(
                    (name.clone(), symbol.uuid.clone(), "Reference".to_string()),
                    symbol.property_reference.clone(),
                );
                for record in symbol.paths {
                    out.insert(
                        (name.clone(), symbol.uuid.clone(), record.path),
                        record.reference,
                    );
                }
            }
        }
        out
    }

    /// What eeschema's annotation in the fixture says.
    fn nested_expected() -> BTreeMap<(String, String, String), String> {
        let files: Vec<(String, String)> = NESTED
            .iter()
            .map(|(name, content)| (name.to_string(), content.to_string()))
            .collect();
        hierarchy_designators(&files)
    }

    /// Every designator `reset` selects (by prefix), in the property and on
    /// every record of the placed symbols, set back to `prefix?`.
    fn reset_designators(content: &str, reset: impl Fn(&str) -> bool) -> String {
        let mut edits = Vec::new();
        for symbol in symbols_of(content) {
            let (start, end) = symbol_block(content, &symbol.uuid).unwrap();
            for needle in ["(property \"Reference\" \"", "(reference \""] {
                for (offset, _) in content[start..end].match_indices(needle) {
                    let value = start + offset + needle.len();
                    let len = content[value..].find('"').unwrap();
                    let prefix = parse_designator(&content[value..value + len]).prefix;
                    if reset(&prefix) {
                        edits.push(SexpEdit::replace(value, value + len, format!("{prefix}?")));
                    }
                }
            }
        }
        apply_edits(content.to_string(), edits)
    }

    fn uuid_at(content: &str, path: &str, reference: &str) -> String {
        symbols_of(content)
            .into_iter()
            .find(|s| {
                s.paths
                    .iter()
                    .any(|p| p.path == path && p.reference == reference)
            })
            .unwrap_or_else(|| panic!("no {reference} on {path}"))
            .uuid
    }

    /// Byte range of `uuid`'s record on `path`.
    fn record_block(content: &str, uuid: &str, path: &str) -> (usize, usize) {
        let (start, end) = symbol_block(content, uuid).unwrap();
        let at = start
            + content[start..end]
                .find(&format!("(path \"{path}\""))
                .unwrap();
        find_enclosing_block(content, "path", at + 1).unwrap()
    }

    fn set_record(content: &str, uuid: &str, path: &str, reference: &str) -> String {
        let (start, end) = record_block(content, uuid, path);
        let needle = "(reference \"";
        let value = start + content[start..end].find(needle).unwrap() + needle.len();
        let len = content[value..].find('"').unwrap();
        let mut out = content.to_string();
        out.replace_range(value..value + len, reference);
        out
    }

    fn remove_record(content: &str, uuid: &str, path: &str) -> String {
        let (start, end) = record_block(content, uuid, path);
        let mut out = content.to_string();
        out.replace_range(start..end, "");
        out
    }

    fn add_record(content: &str, uuid: &str, project: &str, path: &str, reference: &str) -> String {
        let (start, end) = symbol_block(content, uuid).unwrap();
        let at = start + content[start..end].find("(instances").unwrap() + "(instances".len();
        let mut out = content.to_string();
        out.insert_str(
            at,
            &format!(
                "\n\t\t\t(project \"{project}\"\n\t\t\t\t(path \"{path}\"\n\t\t\t\t\t(reference \
                 \"{reference}\")\n\t\t\t\t\t(unit 1)\n\t\t\t\t)\n\t\t\t)"
            ),
        );
        out
    }

    fn remove_symbol(content: &str, uuid: &str) -> String {
        let (start, end) = symbol_block(content, uuid).unwrap();
        let mut out = content.to_string();
        out.replace_range(start..end, "");
        out
    }

    /// The page `sheet` records under its parent's instance `parent`.
    fn set_sheet_page(content: &str, sheet: &str, parent: &str, page: &str) -> String {
        let at = content.find(&format!("(uuid \"{sheet}\")")).unwrap();
        let (start, end) = find_enclosing_block(content, "sheet", at).unwrap();
        let start = start
            + content[start..end]
                .find(&format!("(path \"{parent}\""))
                .unwrap();
        let needle = "(page \"";
        let value = start + content[start..end].find(needle).unwrap() + needle.len();
        let len = content[value..].find('"').unwrap();
        let mut out = content.to_string();
        out.replace_range(value..value + len, page);
        out
    }

    /// The first `(at …)` of the block `start..end`, which must read `(at
    /// {from})`, made `(at {to})`.
    fn move_block(content: &str, (start, end): (usize, usize), from: &str, to: &str) -> String {
        let at = start + content[start..end].find("(at ").unwrap();
        let close = at + content[at..].find(')').unwrap() + 1;
        assert_eq!(content[at..close], format!("(at {from})"));
        let mut out = content.to_string();
        out.replace_range(at..close, &format!("(at {to})"));
        out
    }

    /// `sheet` placed at `to` in place of `from`.
    fn move_sheet(content: &str, sheet: &str, from: &str, to: &str) -> String {
        let at = content.find(&format!("(uuid \"{sheet}\")")).unwrap();
        let block = find_enclosing_block(content, "sheet", at).unwrap();
        move_block(content, block, from, to)
    }

    /// The symbol `uuid` placed at `to` in place of `from`.
    fn move_symbol(content: &str, uuid: &str, from: &str, to: &str) -> String {
        move_block(content, symbol_block(content, uuid).unwrap(), from, to)
    }

    fn error_field(result: &CallToolResult) -> Value {
        response_json(result)["error"]["field"].clone()
    }

    #[test]
    fn a_root_call_numbers_every_sheet_instance_in_eeschemas_page_order() {
        // Every designator of the nested fixture reset to `?`, and two of the
        // root's symbols recording their instance under project "". Annotated
        // from the root, each record must get back the number eeschema's
        // annotation gave it: south (page 2) before north (page 10), although
        // north's path sorts first as a string, north comes first in the file
        // and sits above south; under south, east (page 3) before west (page
        // 4), although west is left of east.
        let (dir, root) = nested_project(|name, content| {
            reset_designators(&unnamed_project_records(name, content), |_| true)
        });
        let body = response_json(&annotate_hierarchy(&root, AnnotateOptions::default()));
        assert_eq!(body["schematic_scope"], "hierarchy", "{body}");
        assert_eq!(body["outcome"]["status"], "complete", "{body}");
        assert_eq!(body["written"], true, "{body}");
        let expected = nested_expected();
        assert_eq!(
            hierarchy_designators(&saved(dir.path(), &nested_names())),
            expected
        );
        let records = expected.keys().filter(|(_, _, p)| p != "Reference").count();
        let assigned = body["assigned"].as_array().unwrap();
        assert_eq!(assigned.len(), records, "{body}");
        assert!(
            assigned.iter().all(|a| a["schematic"]
                .as_str()
                .is_some_and(|s| s.ends_with(".kicad_sch"))),
            "{body}"
        );
        // The root's records named project "" are the project's, by path.
        assert_eq!(
            assigned.iter().filter(|a| a["project"] == "").count(),
            2,
            "{body}"
        );
        assert_eq!(body["sheets"].as_array().map(Vec::len), Some(7), "{body}");

        let again = response_json(&annotate_hierarchy(&root, AnnotateOptions::default()));
        assert_eq!(again["written"], false, "{again}");
        assert_eq!(again["outcome"]["status"], "complete", "{again}");
    }

    #[test]
    fn a_root_call_numbers_the_symbol_left_unannotated_on_a_nested_instance() {
        // The only `?` sits on a cell instance. A file-scope call on the root
        // has nothing to do and says the sheets were not annotated; the
        // hierarchy call numbers the crystal, Y3 (Y1, Y2 and Y4 are taken),
        // and nothing else.
        let (dir, root) = nested_project(unannotated_crystal);
        for dry_run in [false, true] {
            let options = AnnotateOptions {
                dry_run,
                ..AnnotateOptions::default()
            };
            let file = response_json(&annotate_file(&root, options));
            assert_eq!(file["schematic_scope"], "file", "{file}");
            assert_eq!(file["written"], false, "{file}");
            assert!(
                file["message"]
                    .as_str()
                    .unwrap()
                    .contains("schematic_scope 'hierarchy'"),
                "{file}"
            );
        }

        let before = saved(dir.path(), &nested_names());
        let body = response_json(&annotate_hierarchy(&root, AnnotateOptions::default()));
        assert_eq!(body["outcome"]["status"], "complete", "{body}");
        let assigned = body["assigned"].as_array().unwrap();
        assert_eq!(assigned.len(), 1, "{body}");
        assert_eq!(assigned[0]["from"], "Y?");
        assert_eq!(assigned[0]["to"], "Y3");
        assert_eq!(assigned[0]["path"], instance(&[NORTH, WEST]));
        assert!(assigned[0]["schematic"]
            .as_str()
            .unwrap()
            .ends_with("cell.kicad_sch"));
        let after = saved(dir.path(), &nested_names());
        assert_eq!(after[..2], before[..2], "root and bank are not written");
        assert_eq!(hierarchy_designators(&after), nested_expected());
    }

    #[test]
    fn a_duplicate_across_sheets_keeps_its_number_on_the_earliest_page() {
        // R10 on page 10 (north) renamed R6, which page 3 (east under south)
        // carries. The page-3 record keeps R6, although page 10's path sorts
        // first and north is walked first.
        let page10 = instance(&[NORTH]);
        let r10 = uuid_at(NESTED[1].1, &page10, "R10");
        let derive = |name: &str, content: &str| {
            if name == "bank.kicad_sch" {
                set_record(content, &r10, &page10, "R6")
            } else {
                content.to_string()
            }
        };
        let (dir, root) = nested_project(derive);
        let body = response_json(&annotate_hierarchy(&root, AnnotateOptions::default()));
        assert_eq!(body["outcome"]["status"], "partial", "{body}");
        assert_eq!(
            body["outcome"]["retry"]["scope"], "unresolved_duplicates",
            "{body}"
        );
        let group = &body["unresolved"][0];
        assert_eq!(group["reference"], "R6", "{body}");
        assert_eq!(group["reason"], REASON_DUPLICATE_IN_HIERARCHY);
        assert_eq!(group["members"][0]["path"], instance(&[SOUTH, EAST]));
        assert!(group["members"][0]["schematic"]
            .as_str()
            .unwrap()
            .ends_with("cell.kicad_sch"));
        assert_eq!(group["members"][1]["path"], page10);
        assert!(group["members"][1]["schematic"]
            .as_str()
            .unwrap()
            .ends_with("bank.kicad_sch"));

        let resolve = AnnotateOptions {
            resolve_duplicates: true,
            ..AnnotateOptions::default()
        };
        let body = response_json(&annotate_hierarchy(&root, resolve));
        assert_eq!(body["outcome"]["status"], "complete", "{body}");
        assert_eq!(
            hierarchy_designators(&saved(dir.path(), &nested_names())),
            nested_expected(),
            "page 10 takes R10, the first free number, back"
        );
    }

    #[test]
    fn the_reference_property_follows_the_first_instance_in_page_order() {
        // A resistor whose page-4 record copied R6 from page 3, as a reused
        // sheet does when one designator is written to every instance. With
        // the duplicate resolved page 4 gets R8 back and the property stays
        // R6, page 3's, which is the instance eeschema saves it from: not
        // north/west's (R13), the first of cell's instances in the walk, nor
        // north/east's (R15), whose path sorts first.
        let page3 = instance(&[SOUTH, EAST]);
        let page4 = instance(&[SOUTH, WEST]);
        let r6 = uuid_at(NESTED[2].1, &page3, "R6");
        let derive = |name: &str, content: &str| {
            if name == "cell.kicad_sch" {
                set_record(content, &r6, &page4, "R6")
            } else {
                content.to_string()
            }
        };
        let (dir, root) = nested_project(derive);
        let resolve = AnnotateOptions {
            resolve_duplicates: true,
            ..AnnotateOptions::default()
        };
        let body = response_json(&annotate_hierarchy(&root, resolve));
        assert_eq!(body["outcome"]["status"], "complete", "{body}");
        assert_eq!(
            hierarchy_designators(&saved(dir.path(), &nested_names())),
            nested_expected()
        );
    }

    /// [`nested_expected`] with the records of sheet instances `a` and `b`
    /// exchanged: the two swapped pages.
    fn nested_expected_swapping(a: &str, b: &str) -> BTreeMap<(String, String, String), String> {
        nested_expected()
            .into_iter()
            .map(|((name, uuid, path), reference)| {
                let path = if path == a {
                    b.to_string()
                } else if path == b {
                    a.to_string()
                } else {
                    path
                };
                ((name, uuid, path), reference)
            })
            .collect()
    }

    #[test]
    fn repeated_pages_are_ordered_as_eeschema_repairs_them() {
        // south given north's page 10. eeschema keeps 10 for the first of the
        // two in its walk of the root's sheets (by X, then Y) and gives the
        // other 2, the lowest page nobody holds. The fixture's two sheets
        // share X, so each case first moves one: south left of north, still
        // below it; then north below south, at the same X. In both, north
        // comes first in the file, by uuid and by path; a Y-first walk takes
        // it first in the first case, an X-then-uuid walk in the second. Only
        // an X-then-Y walk takes south first in both: south keeps 10 and
        // north takes 2, so the two swap numbers.
        let moves = [
            (SOUTH, "25.4 101.6", "12.7 101.6"),
            (NORTH, "25.4 38.1", "25.4 139.7"),
        ];
        for (sheet, from, to) in moves {
            let derive = |name: &str, content: &str| {
                let content = if name == "nested_pages.kicad_sch" {
                    let content = move_sheet(content, sheet, from, to);
                    set_sheet_page(&content, SOUTH, &instance(&[]), "10")
                } else {
                    content.to_string()
                };
                reset_designators(&content, |_| true)
            };
            let (dir, root) = nested_project(derive);
            let body = response_json(&annotate_hierarchy(&root, AnnotateOptions::default()));
            assert_eq!(body["outcome"]["status"], "complete", "{body}");
            let (north, south) = (instance(&[NORTH]), instance(&[SOUTH]));
            assert_eq!(
                hierarchy_designators(&saved(dir.path(), &nested_names())),
                nested_expected_swapping(&north, &south),
                "{sheet} at ({to})"
            );
            let sheets = body["sheets"].as_array().unwrap();
            let sheet_at = |path: &str| sheets.iter().find(|s| s["path"] == path).unwrap();
            let moved = sheet_at(&north);
            assert_eq!(moved["page"], "2", "{body}");
            assert_eq!(moved["page_in_file"], "10", "{body}");
            assert_eq!(moved["sheet_number"], 2, "{body}");
            let kept = sheet_at(&south);
            assert_eq!(kept["page"], "10", "{body}");
            assert_eq!(kept["page_in_file"], Value::Null, "{body}");
        }
    }

    #[test]
    fn a_repeated_page_stays_with_the_sheet_further_left() {
        // Under south (page 2), west (page 4) given page 3 as well. eeschema
        // walks a sheet's children by position, so west, left of east although
        // it comes second in the file and its uuid and path sort after, keeps 3
        // and east is moved to 4: the two swap numbers.
        let derive = |name: &str, content: &str| {
            let content = if name == "bank.kicad_sch" {
                set_sheet_page(content, WEST, &instance(&[SOUTH]), "3")
            } else {
                content.to_string()
            };
            reset_designators(&content, |_| true)
        };
        let (dir, root) = nested_project(derive);
        let body = response_json(&annotate_hierarchy(&root, AnnotateOptions::default()));
        assert_eq!(body["outcome"]["status"], "complete", "{body}");
        let (west, east) = (instance(&[SOUTH, WEST]), instance(&[SOUTH, EAST]));
        // The properties are unchanged: they follow page 3, now west, which
        // takes the numbers page 3 had.
        assert_eq!(
            hierarchy_designators(&saved(dir.path(), &nested_names())),
            nested_expected_swapping(&west, &east)
        );
        let moved = body["sheets"]
            .as_array()
            .unwrap()
            .iter()
            .find(|sheet| sheet["path"] == east)
            .unwrap();
        assert_eq!(moved["page"], "4", "{body}");
        assert_eq!(moved["page_in_file"], "3", "{body}");
    }

    #[test]
    fn a_page_that_is_not_a_number_ties_by_position_with_its_virtual_page() {
        // north's page "N" sorts after every number, so its virtual page is
        // 7, its place in page order; its own east instance is given page 7.
        // The two, in different files, tie on sheet number 7, and eeschema
        // orders a tie by X alone. bank's leftmost resistor is moved left of
        // both cell resistors, so the tie interleaves the two instances:
        // north's moved resistor, then north/east's two, then north's other
        // two. Only the resistors are reset; the rest keep their numbers.

        // bank's resistors left to right (x 165.1, 177.8, 190.5), and cell's
        // (76.2, 101.6).
        let (south, north) = (instance(&[SOUTH]), instance(&[NORTH]));
        let (bank_a, bank_b, bank_c) = (
            uuid_at(NESTED[1].1, &south, "R3"),
            uuid_at(NESTED[1].1, &south, "R4"),
            uuid_at(NESTED[1].1, &south, "R5"),
        );
        let page3 = instance(&[SOUTH, EAST]);
        let (cell_l, cell_r) = (
            uuid_at(NESTED[2].1, &page3, "R6"),
            uuid_at(NESTED[2].1, &page3, "R7"),
        );
        let derive = |name: &str, content: &str| {
            let content = match name {
                "nested_pages.kicad_sch" => set_sheet_page(content, NORTH, &instance(&[]), "N"),
                "bank.kicad_sch" => {
                    let content = set_sheet_page(content, EAST, &north, "7");
                    move_symbol(&content, &bank_a, "165.1 50.8 0", "63.5 50.8 0")
                }
                _ => content.to_string(),
            };
            reset_designators(&content, |prefix| prefix == "R")
        };
        let (dir, root) = nested_project(derive);
        let body = response_json(&annotate_hierarchy(&root, AnnotateOptions::default()));
        assert_eq!(body["outcome"]["status"], "complete", "{body}");
        let sheets = body["sheets"].as_array().unwrap();
        let tied: Vec<&Value> = sheets
            .iter()
            .filter(|sheet| sheet["sheet_number"] == 7)
            .map(|sheet| &sheet["path"])
            .collect();
        assert_eq!(tied, [&json!(instance(&[NORTH, EAST])), &json!(north)]);

        // By hand, by sheet number, then X, each taking the first free number:
        // root (1), south (2, the moved resistor still its leftmost), south/
        // east (3) and south/west (4) keep R1 to R9. Then 7: north's moved
        // resistor (x 63.5) R10, north/east's (76.2, 101.6) R11 and R12,
        // north's other two (177.8, 190.5) R13 and R14; last north/west (11),
        // R15 and R16. Each property shows its file's first instance in page
        // order, as before.
        let mut expected = nested_expected();
        let (bank, cell) = ("bank.kicad_sch", "cell.kicad_sch");
        let renumbered = [
            (bank, &bank_a, north.clone(), "R10"),
            (cell, &cell_l, instance(&[NORTH, EAST]), "R11"),
            (cell, &cell_r, instance(&[NORTH, EAST]), "R12"),
            (bank, &bank_b, north.clone(), "R13"),
            (bank, &bank_c, north.clone(), "R14"),
            (cell, &cell_l, instance(&[NORTH, WEST]), "R15"),
            (cell, &cell_r, instance(&[NORTH, WEST]), "R16"),
        ];
        for (file, uuid, path, reference) in renumbered {
            let key = (file.to_string(), uuid.clone(), path);
            assert!(expected.insert(key, reference.to_string()).is_some());
        }
        assert_eq!(
            hierarchy_designators(&saved(dir.path(), &nested_names())),
            expected
        );
    }

    #[test]
    fn another_projects_records_are_left_byte_for_byte() {
        let other = "\n\t\t\t(project \"other\"\n\t\t\t\t(path \
                     \"/00000000-0000-0000-0000-000000000001/00000000-0000-0000-0000-000000000002\"\
                     \n\t\t\t\t\t(reference \"R99\")\n\t\t\t\t\t(unit 1)\n\t\t\t\t)\n\t\t\t)";
        let r6 = uuid_at(NESTED[2].1, &instance(&[SOUTH, EAST]), "R6");
        let derive = |name: &str, content: &str| {
            let content = reset_designators(content, |_| true);
            if name == "cell.kicad_sch" {
                add_record(
                    &content,
                    &r6,
                    "other",
                    "/00000000-0000-0000-0000-000000000001/00000000-0000-0000-0000-000000000002",
                    "R99",
                )
            } else {
                content
            }
        };
        let (dir, root) = nested_project(derive);
        let body = response_json(&annotate_hierarchy(&root, AnnotateOptions::default()));
        assert_eq!(body["outcome"]["status"], "complete", "{body}");
        let cell = std::fs::read_to_string(dir.path().join("cell.kicad_sch")).unwrap();
        assert!(
            cell.contains(other),
            "the other project's record is untouched"
        );
    }

    #[test]
    fn instance_record_problems_refuse_before_anything_is_written() {
        let page3 = instance(&[SOUTH, EAST]);
        let page4 = instance(&[SOUTH, WEST]);
        let r6 = uuid_at(NESTED[2].1, &page3, "R6");
        let dead = instance(&[SOUTH, "00000000-0000-0000-0000-00000000dead"]);
        type Edit<'a> = Box<dyn Fn(&str) -> String + 'a>;
        let elsewhere =
            "/00000000-0000-0000-0000-000000000001/00000000-0000-0000-0000-000000000002";
        let cases: [(&str, Edit); 6] = [
            (
                "has no record for sheet instance",
                Box::new(|c: &str| remove_record(c, &r6, &page4)),
            ),
            (
                "more than once",
                Box::new(|c: &str| add_record(c, &r6, "nested_pages", &page3, "R6")),
            ),
            (
                // Under another root but this project's name: eeschema prunes it.
                "which does not start at the root",
                Box::new(|c: &str| add_record(c, &r6, "nested_pages", elsewhere, "R51")),
            ),
            (
                "shares its uuid with another symbol",
                Box::new(|c: &str| {
                    let (start, end) = symbol_block(c, &r6).unwrap();
                    format!("{}\n\t{}{}", &c[..end], &c[start..end], &c[end..])
                }),
            ),
            (
                "which is not a sheet instance of this file",
                Box::new(|c: &str| add_record(c, &r6, "nested_pages", &dead, "R50")),
            ),
            (
                "under project 'renamed'",
                Box::new(|c: &str| {
                    let (start, end) = symbol_block(c, &r6).unwrap();
                    let block =
                        c[start..end].replace("(project \"nested_pages\"", "(project \"renamed\"");
                    format!("{}{block}{}", &c[..start], &c[end..])
                }),
            ),
        ];
        for (diagnosis, edit) in cases {
            let (dir, root) = nested_project(|name, content| {
                let content = unannotated_crystal(name, content);
                if name == "cell.kicad_sch" {
                    edit(&content)
                } else {
                    content
                }
            });
            let before = saved(dir.path(), &nested_names());
            let result = annotate_hierarchy(&root, AnnotateOptions::default());
            assert_eq!(extract_error_kind(&result).as_deref(), Some("stale_target"));
            assert_eq!(response_json(&result)["outcome"]["status"], "failed");
            assert!(text_of(&result).contains(diagnosis), "{}", text_of(&result));
            assert_eq!(saved(dir.path(), &nested_names()), before);
        }
    }

    #[test]
    fn a_hierarchy_call_never_falls_back_to_one_file() {
        let (dir, root) = nested_project(unannotated_crystal);
        let before = saved(dir.path(), &nested_names());

        let child = annotate_hierarchy(
            &dir.path().join("cell.kicad_sch"),
            AnnotateOptions::default(),
        );
        assert_eq!(
            extract_error_kind(&child).as_deref(),
            Some("invalid_argument")
        );
        assert_eq!(error_field(&child), "schematic");
        assert!(text_of(&child).contains("nested_pages.kicad_sch"));

        let other = AnnotateOptions {
            project: Some("nope".into()),
            ..AnnotateOptions::default()
        };
        let named = annotate_hierarchy(&root, other);
        assert_eq!(error_field(&named), "project");

        std::fs::write(dir.path().join("stray.kicad_pro"), "{}").unwrap();
        let stray = annotate_hierarchy(&root, AnnotateOptions::default());
        assert_eq!(extract_error_kind(&stray).as_deref(), Some("conflict"));
        std::fs::remove_file(dir.path().join("stray.kicad_pro")).unwrap();

        std::fs::remove_file(dir.path().join("nested_pages.kicad_pro")).unwrap();
        let loose = annotate_hierarchy(&root, AnnotateOptions::default());
        assert_eq!(error_field(&loose), "schematic_scope");
        assert_eq!(saved(dir.path(), &nested_names()), before);
    }

    #[test]
    fn a_hierarchy_that_cannot_be_walked_refuses_before_anything_is_written() {
        let refusals: [(&str, &str, &str, &str); 2] = [
            // A root saved before sheet blocks carried their own pages.
            (
                "nested_pages.kicad_sch",
                "(version 20260306)",
                "(version 20221002)",
                "KiCad 7 or later",
            ),
            // Two of bank's sheet blocks with one uuid: one instance path.
            ("bank.kicad_sch", WEST, EAST, "appears twice"),
        ];
        for (file, from, to, says) in refusals {
            let (dir, root) = nested_project(|name, content| {
                let content = unannotated_crystal(name, content);
                if name == file {
                    content.replace(from, to)
                } else {
                    content
                }
            });
            let before = saved(dir.path(), &nested_names());
            let result = annotate_hierarchy(&root, AnnotateOptions::default());
            assert_eq!(
                extract_error_kind(&result).as_deref(),
                Some("handler_error")
            );
            assert!(text_of(&result).contains(says), "{}", text_of(&result));
            assert_eq!(saved(dir.path(), &nested_names()), before);
        }

        let (dir, root) = nested_project(unannotated_crystal);
        std::fs::remove_file(dir.path().join("cell.kicad_sch")).unwrap();
        let missing = annotate_hierarchy(&root, AnnotateOptions::default());
        assert_eq!(
            extract_error_kind(&missing).as_deref(),
            Some("file_not_found")
        );
        assert!(text_of(&missing).contains("cell.kicad_sch"));

        // cell placing bank, which places cell.
        let at = NESTED[1].1.find(&format!("(uuid \"{EAST}\")")).unwrap();
        let (start, end) = find_enclosing_block(NESTED[1].1, "sheet", at).unwrap();
        let sheet = NESTED[1].1[start..end]
            .replace("cell.kicad_sch", "bank.kicad_sch")
            .replace(EAST, "00000000-0000-0000-0000-0000000000c1");
        let (dir, root) = nested_project(|name, content| {
            let content = unannotated_crystal(name, content);
            if name == "cell.kicad_sch" {
                let close = content.rfind(')').unwrap();
                format!("{}\t{sheet}\n{}", &content[..close], &content[close..])
            } else {
                content
            }
        });
        let before = saved(dir.path(), &nested_names());
        let cycle = annotate_hierarchy(&root, AnnotateOptions::default());
        assert_eq!(extract_error_kind(&cycle).as_deref(), Some("handler_error"));
        assert!(
            text_of(&cycle).contains("which contains this sheet"),
            "{}",
            text_of(&cycle)
        );
        assert_eq!(saved(dir.path(), &nested_names()), before);
    }

    #[test]
    fn a_kicad_lock_beside_any_sheet_refuses_the_whole_call() {
        for locked in ["~nested_pages.kicad_sch.lck", "~cell.kicad_sch.lck"] {
            let (dir, root) = nested_project(unannotated_crystal);
            std::fs::write(dir.path().join(locked), "").unwrap();
            let before = saved(dir.path(), &nested_names());
            let result = annotate_hierarchy(&root, AnnotateOptions::default());
            assert_eq!(extract_error_kind(&result).as_deref(), Some("conflict"));
            assert!(text_of(&result).contains(locked), "{}", text_of(&result));
            assert_eq!(saved(dir.path(), &nested_names()), before);
        }
    }

    /// Anything at the lock path counts, as the writer counts it: a dangling
    /// link beside the root, which this call would not write, refuses it too.
    #[cfg(unix)]
    #[test]
    fn a_lock_that_cannot_be_followed_still_refuses_the_call() {
        let (dir, root) = nested_project(unannotated_crystal);
        let lock = dir.path().join("~nested_pages.kicad_sch.lck");
        std::os::unix::fs::symlink(dir.path().join("nowhere"), &lock).unwrap();
        let before = saved(dir.path(), &nested_names());
        let result = annotate_hierarchy(&root, AnnotateOptions::default());
        assert_eq!(extract_error_kind(&result).as_deref(), Some("conflict"));
        assert!(text_of(&result).contains("~nested_pages.kicad_sch.lck"));
        assert_eq!(saved(dir.path(), &nested_names()), before);
    }

    #[test]
    fn a_project_with_another_top_level_sheet_is_refused() {
        // KiCad 10 annotates every top-level sheet a project lists as one
        // hierarchy. A second one, or a uuid the project gives the root in
        // place of its own, is refused; the root alone is annotated.
        let with_sheets = |sheets: Value| {
            let mut settings: Value = serde_json::from_str(NESTED_PRO).unwrap();
            settings["schematic"]["top_level_sheets"] = sheets;
            settings.to_string()
        };
        let root_only = json!([{"uuid": "00000000-0000-0000-0000-000000000000",
            "name": "nested_pages", "filename": "nested_pages.kicad_sch"}]);
        let two = json!([root_only[0].clone(), {"uuid": "11111111-1111-1111-1111-111111111111",
            "name": "other", "filename": "other.kicad_sch"}]);
        let renamed = json!([{"uuid": "11111111-1111-1111-1111-111111111111",
            "name": "nested_pages", "filename": "nested_pages.kicad_sch"}]);
        for (sheets, refused) in [(two, true), (renamed, true), (root_only, false)] {
            let (dir, root) = nested_project(unannotated_crystal);
            std::fs::write(
                dir.path().join("nested_pages.kicad_pro"),
                with_sheets(sheets),
            )
            .unwrap();
            let before = saved(dir.path(), &nested_names());
            for dry_run in [true, false] {
                let options = AnnotateOptions {
                    dry_run,
                    ..AnnotateOptions::default()
                };
                let result = annotate_hierarchy(&root, options);
                if refused {
                    assert_eq!(
                        error_field(&result),
                        "schematic_scope",
                        "{}",
                        text_of(&result)
                    );
                    assert_eq!(saved(dir.path(), &nested_names()), before);
                } else {
                    assert_eq!(response_json(&result)["outcome"]["status"], "complete");
                }
            }
        }
    }

    #[test]
    fn a_failed_transaction_is_uncertain_and_names_every_file() {
        for committed in [true, false] {
            let (dir, root) = nested_project(|_, content| reset_designators(content, |_| true));
            let before = saved(dir.path(), &nested_names());
            let result = annotate_hierarchy_with_persistence(
                &root,
                AnnotateOptions::default(),
                |directory, transitions| {
                    if committed {
                        commit_file_transaction(directory, transitions).unwrap();
                    }
                    Err(SexpError::Conflict {
                        path: directory.to_path_buf(),
                    })
                },
            );
            let body = response_json(&result);
            assert_eq!(body["outcome"]["status"], "uncertain", "{body}");
            assert_eq!(body["outcome"]["retry"], outcome::inspect_before_retry());
            assert_eq!(
                extract_error_kind(&result).as_deref(),
                Some("mutation_outcome_uncertain")
            );
            let text = text_of(&result);
            for name in nested_names() {
                assert!(text.contains(name), "{text}");
            }
            assert!(text.contains("copperpour transaction status"), "{text}");
            let after = saved(dir.path(), &nested_names());
            assert_eq!(after == before, !committed);
        }
    }

    #[test]
    fn a_dry_run_from_the_root_writes_nothing() {
        let (dir, root) = nested_project(|_, content| reset_designators(content, |_| true));
        let before = saved(dir.path(), &nested_names());
        let dry = AnnotateOptions {
            dry_run: true,
            ..AnnotateOptions::default()
        };
        let body = response_json(&annotate_hierarchy(&root, dry));
        assert_eq!(body["outcome"]["source"], "dry_run_plan", "{body}");
        assert_eq!(body["outcome"]["status"], "complete", "{body}");
        assert_eq!(body["written"], false);
        assert_eq!(saved(dir.path(), &nested_names()), before);
        let real = response_json(&annotate_hierarchy(&root, AnnotateOptions::default()));
        assert_eq!(
            real["assigned"], body["assigned"],
            "the dry run reports the same plan"
        );
    }

    #[test]
    fn a_sheet_to_write_outside_the_project_directory_is_refused() {
        // The journal lives in the root's directory; a sheet reached through
        // `..` cannot be covered by it.
        let dir = tempfile::tempdir().unwrap();
        let project = dir.path().join("project");
        std::fs::create_dir(&project).unwrap();
        std::fs::write(project.join("nested_pages.kicad_pro"), NESTED_PRO).unwrap();
        let root = project.join("nested_pages.kicad_sch");
        std::fs::write(
            &root,
            NESTED[0]
                .1
                .replace("\"bank.kicad_sch\"", "\"../bank.kicad_sch\""),
        )
        .unwrap();
        std::fs::write(dir.path().join("bank.kicad_sch"), NESTED[1].1).unwrap();
        let cell = unannotated_crystal(NESTED[2].0, NESTED[2].1);
        std::fs::write(dir.path().join("cell.kicad_sch"), &cell).unwrap();
        for dry_run in [true, false] {
            let options = AnnotateOptions {
                dry_run,
                ..AnnotateOptions::default()
            };
            let result = annotate_hierarchy(&root, options);
            assert_eq!(extract_error_kind(&result).as_deref(), Some("stale_target"));
            assert!(
                text_of(&result).contains("lies outside"),
                "{}",
                text_of(&result)
            );
            assert_eq!(response_json(&result)["outcome"]["status"], "failed");
        }
        assert_eq!(
            std::fs::read_to_string(dir.path().join("cell.kicad_sch")).unwrap(),
            cell
        );
    }

    #[test]
    fn an_unsettled_transaction_in_the_project_directory_refuses_first() {
        // A journal the transaction code cannot recover: the plan would be
        // read from files it might still change, so neither a dry run nor a
        // real run goes on.
        let (dir, root) = nested_project(unannotated_crystal);
        std::fs::write(
            dir.path().join(".copperpour-transaction-unreadable.json"),
            "{}",
        )
        .unwrap();
        let before = saved(dir.path(), &nested_names());
        for dry_run in [true, false] {
            let options = AnnotateOptions {
                dry_run,
                ..AnnotateOptions::default()
            };
            let result = annotate_hierarchy(&root, options);
            assert_eq!(extract_error_kind(&result).as_deref(), Some("conflict"));
            assert!(
                text_of(&result).contains("copperpour transaction status"),
                "{}",
                text_of(&result)
            );
        }
        assert_eq!(saved(dir.path(), &nested_names()), before);
    }

    #[test]
    fn the_reply_reports_the_hierarchy_as_read_back() {
        // Another writer gives a bank resistor page 3's R6 between this
        // call's commit and its readback. The reply is partial and names the
        // duplicate the saved hierarchy now holds, though the plan had none.
        let (dir, root) = nested_project(unannotated_crystal);
        let page2 = instance(&[SOUTH]);
        let r3 = uuid_at(NESTED[1].1, &page2, "R3");
        let bank = dir.path().join("bank.kicad_sch");
        let result = annotate_hierarchy_with_persistence(
            &root,
            AnnotateOptions::default(),
            |directory, transitions| {
                commit_file_transaction(directory, transitions)?;
                let content = std::fs::read_to_string(&bank).unwrap();
                std::fs::write(&bank, set_record(&content, &r3, &page2, "R6")).unwrap();
                Ok(())
            },
        );
        let body = response_json(&result);
        assert_eq!(body["written"], true, "{body}");
        assert_eq!(body["outcome"]["status"], "partial", "{body}");
        assert_eq!(
            body["unresolved"].as_array().map(Vec::len),
            Some(0),
            "{body}"
        );
        assert_eq!(body["remaining_unresolved"][0]["reference"], "R6", "{body}");
    }

    #[test]
    fn copies_of_one_sheet_keep_their_own_records() {
        // west points at a copy of cell: the two files share every symbol
        // uuid, each keeping only its own instances' records.
        let (kept, copied) = (EAST, WEST);
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("nested_pages.kicad_pro"), NESTED_PRO).unwrap();
        let root = dir.path().join("nested_pages.kicad_sch");
        std::fs::write(&root, reset_designators(NESTED[0].1, |_| true)).unwrap();
        let bank = {
            let at = NESTED[1].1.find(&format!("(uuid \"{copied}\")")).unwrap();
            let (start, end) = find_enclosing_block(NESTED[1].1, "sheet", at).unwrap();
            let sheet = NESTED[1].1[start..end].replace("cell.kicad_sch", "cell_copy.kicad_sch");
            assert_ne!(sheet, NESTED[1].1[start..end]);
            format!("{}{sheet}{}", &NESTED[1].1[..start], &NESTED[1].1[end..])
        };
        std::fs::write(
            dir.path().join("bank.kicad_sch"),
            reset_designators(&bank, |_| true),
        )
        .unwrap();
        let keep_only = |keep: &str| {
            let mut content = NESTED[2].1.to_string();
            for symbol in symbols_of(NESTED[2].1) {
                for record in &symbol.paths {
                    if !record.path.ends_with(keep) {
                        content = remove_record(&content, &symbol.uuid, &record.path);
                    }
                }
            }
            reset_designators(&content, |_| true)
        };
        std::fs::write(dir.path().join("cell.kicad_sch"), keep_only(kept)).unwrap();
        std::fs::write(dir.path().join("cell_copy.kicad_sch"), keep_only(copied)).unwrap();

        let body = response_json(&annotate_hierarchy(&root, AnnotateOptions::default()));
        assert_eq!(body["outcome"]["status"], "complete", "{body}");
        let expected = nested_expected();
        for (name, keep) in [("cell.kicad_sch", kept), ("cell_copy.kicad_sch", copied)] {
            let now = hierarchy_designators(&saved(dir.path(), &[name]));
            let mut records = 0;
            for ((_, uuid, path), reference) in &now {
                if path == "Reference" {
                    continue;
                }
                records += 1;
                assert!(path.ends_with(keep), "{name} keeps only its records");
                let original =
                    &expected[&("cell.kicad_sch".to_string(), uuid.clone(), path.clone())];
                assert_eq!(reference, original, "{name} {path}");
            }
            assert_eq!(records, 16, "{name}: eight symbols on two instances");
        }
    }

    #[test]
    fn a_root_with_no_symbols_of_its_own_still_annotates_its_sheets() {
        let derive = |name: &str, content: &str| {
            if name == "nested_pages.kicad_sch" {
                let mut content = content.to_string();
                for symbol in symbols_of(NESTED[0].1).iter().rev() {
                    content = remove_symbol(&content, &symbol.uuid);
                }
                content
            } else {
                unannotated_crystal(name, content)
            }
        };
        let (_dir, root) = nested_project(derive);
        let body = response_json(&annotate_hierarchy(&root, AnnotateOptions::default()));
        assert_eq!(body["project"], "nested_pages", "{body}");
        assert_eq!(body["assigned"][0]["to"], "Y3", "{body}");
        assert_eq!(body["outcome"]["status"], "complete", "{body}");
    }

    /// The multichannel demo as a project: the root and its channel strip,
    /// placed four times (CH1 is page 2, though its path sorts after CH2's).
    fn multichannel_project(
        root: impl Fn(&str) -> String,
        strip: impl Fn(&str) -> String,
    ) -> (tempfile::TempDir, std::path::PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("multichannel_mixer.kicad_pro"),
            include_str!("../../tests/fixtures/multichannel_mixer.kicad_pro"),
        )
        .unwrap();
        let path = dir.path().join("multichannel_mixer.kicad_sch");
        std::fs::write(
            &path,
            root(include_str!(
                "../../tests/fixtures/multichannel_mixer.kicad_sch"
            )),
        )
        .unwrap();
        std::fs::write(
            dir.path().join("channel_strip.kicad_sch"),
            strip(REUSED_SHEET),
        )
        .unwrap();
        (dir, path)
    }

    #[test]
    fn a_reused_sheet_is_numbered_in_page_order_with_its_packages_whole() {
        // The demo's R and IC designators reset (each IC a two-unit TL072):
        // annotated from the root they come back as eeschema left them, CH1
        // first and every IC's two units sharing one number per channel.
        let root_source = include_str!("../../tests/fixtures/multichannel_mixer.kicad_sch");
        let (dir, root) = multichannel_project(
            |content| reset_designators(content, |prefix| prefix == "IC"),
            |content| reset_designators(content, |prefix| prefix == "R" || prefix == "IC"),
        );
        let body = response_json(&annotate_hierarchy(&root, AnnotateOptions::default()));
        assert_eq!(body["outcome"]["status"], "complete", "{body}");
        let names = ["multichannel_mixer.kicad_sch", "channel_strip.kicad_sch"];
        let expected = hierarchy_designators(&[
            (names[0].to_string(), root_source.to_string()),
            (names[1].to_string(), REUSED_SHEET.to_string()),
        ]);
        assert_eq!(hierarchy_designators(&saved(dir.path(), &names)), expected);
    }

    #[test]
    fn units_of_one_part_split_across_sheets_are_not_guessed() {
        // The root keeps only its TL072's unit 1, unannotated; the strip
        // only its unit 2, unannotated on all four channels. eeschema would
        // pack a channel's unit 2 into the root's package; which one is not
        // reproduced, so the channels stay unannotated and say why.
        let root_source = include_str!("../../tests/fixtures/multichannel_mixer.kicad_sch");
        let unit = |content: &str, unit: u32| {
            symbols_of(content)
                .into_iter()
                .find(|s| s.lib_symbol.ends_with("TL072CD") && s.unit == unit)
                .unwrap()
                .uuid
        };
        let root_unit2 = unit(root_source, 2);
        let strip_unit1 = unit(REUSED_SHEET, 1);
        let (_dir, root) = multichannel_project(
            |content| reset_designators(&remove_symbol(content, &root_unit2), |p| p == "IC"),
            |content| reset_designators(&remove_symbol(content, &strip_unit1), |p| p == "IC"),
        );
        let body = response_json(&annotate_hierarchy(&root, AnnotateOptions::default()));
        assert_eq!(body["outcome"]["status"], "partial", "{body}");
        let held = |list: &Value, reason: &str| {
            list.as_array()
                .unwrap()
                .iter()
                .filter(|u| u["reason"] == reason)
                .count()
        };
        assert_eq!(
            held(&body["unresolved"], REASON_SPLIT_PACKAGE),
            4,
            "one per channel: {body}"
        );
        let assigned = body["assigned"].as_array().unwrap();
        assert_eq!(assigned.len(), 1, "{body}");
        assert_eq!(assigned[0]["to"], "IC1", "{body}");
        // Read back, IC1 is a numbered package lacking unit 2: the channels
        // stay held, and a second call numbers none of them.
        assert_eq!(
            held(&body["remaining_unresolved"], REASON_JOINS_PACKAGE),
            4,
            "{body}"
        );
        let again = response_json(&annotate_hierarchy(&root, AnnotateOptions::default()));
        assert_eq!(again["outcome"]["status"], "partial", "{again}");
        assert_eq!(again["assigned"], json!([]), "{again}");
        assert_eq!(
            held(&again["unresolved"], REASON_JOINS_PACKAGE),
            4,
            "{again}"
        );
    }
}
