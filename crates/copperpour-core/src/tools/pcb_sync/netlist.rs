//! Read the exported netlist and saved schematic hierarchy used by the sync.

use super::{
    DesignComponent, ExportedDesign, SkippedComponent, SymbolField, SymbolSheet, SymbolUnit,
    UnassignedComponent,
};
use anyhow::{bail, Context, Result};
use copperpour_sexp::paths::display_path;
use copperpour_sexp::SexpNode;
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::path::{Path, PathBuf};

/// Normalize a schematic symbol path for identity comparison by stripping at
/// most one leading `/<root_sheet_uuid>` segment when it matches the requested
/// root schematic's UUID and is followed by at least one non-empty segment
/// (#82).
///
/// KiCad's netlist exporter omits the root sheet UUID
/// (`SCH_SHEET_PATH::PathAsString`), while saved schematic instance paths and
/// some PCB footprints carry it. Foreign root UUIDs, other sheet prefixes, and
/// bare `/<root>` paths are returned unchanged so they remain distinct
/// identities.
pub(super) fn normalize_symbol_path<'a>(path: &'a str, root_sheet_uuid: Option<&str>) -> &'a str {
    let Some(root_uuid) = root_sheet_uuid.filter(|uuid| !uuid.is_empty() && !uuid.contains('/'))
    else {
        return path;
    };
    let Some(without_leading_slash) = path.strip_prefix('/') else {
        return path;
    };
    let Some(rest) = without_leading_slash.strip_prefix(root_uuid) else {
        return path;
    };
    let Some(tail) = rest.strip_prefix('/') else {
        return path;
    };
    if tail.is_empty() || tail.starts_with('/') {
        return path;
    }
    rest
}

/// A stable identity for the design-bearing netlist sections.
///
/// `kicad-cli sch export netlist` stamps `(date "…T14:48:16")` and the
/// exporting tool's version into every export, so hashing the raw source
/// yields a different revision **every second** for a design nobody touched —
/// and since apply requires the revision a dry run returned, apply could only
/// ever succeed if both calls landed inside the same wall-clock second. That
/// is a race, not a guarantee: it passes on a fast machine and fails on a
/// human reviewing the plan first, which is the whole point of the plan.
///
/// The revision must cover what the plan *read*: the complete top-level
/// `components` and `nets` trees. Hashing those trees structurally ignores the
/// volatile header without confusing nested nodes or quoted text for header
/// metadata.
pub(super) fn netlist_identity(netlist_source: &str) -> Vec<u8> {
    let Ok(root) = copperpour_sexp::parse_sexp(netlist_source) else {
        // Production reaches this function only after successful netlist
        // parsing. Keeping invalid synthetic planner inputs distinct makes the
        // pure planner tests useful without creating a second error path here.
        return netlist_source.as_bytes().to_vec();
    };

    let mut identity = Vec::new();
    for tag in ["components", "nets"] {
        match root.find(tag) {
            Some(node) => {
                identity.push(1);
                append_sexp_identity(node, &mut identity);
            }
            None => identity.push(0),
        }
    }
    identity
}

fn append_sexp_identity(node: &SexpNode, identity: &mut Vec<u8>) {
    match node {
        SexpNode::Atom(value) => {
            identity.push(0);
            append_identity_bytes(value.as_bytes(), identity);
        }
        SexpNode::Str(value) => {
            identity.push(1);
            append_identity_bytes(value.as_bytes(), identity);
        }
        SexpNode::List(children) => {
            identity.push(2);
            identity.extend_from_slice(&(children.len() as u64).to_le_bytes());
            for child in children {
                append_sexp_identity(child, identity);
            }
        }
    }
}

fn append_identity_bytes(value: &[u8], identity: &mut Vec<u8>) {
    identity.extend_from_slice(&(value.len() as u64).to_le_bytes());
    identity.extend_from_slice(value);
}

pub(super) fn parse_exported_netlist(source: &str) -> Result<ExportedDesign> {
    let root = copperpour_sexp::parse_sexp(source).context("invalid KiCad netlist S-expression")?;
    let components_node = root
        .find("components")
        .context("KiCad netlist has no components section")?;

    let mut components = Vec::new();
    let mut by_reference = HashMap::new();
    let mut unassigned = Vec::new();
    let mut unassigned_references = HashSet::new();
    let mut seen_references = HashSet::new();
    for component_node in components_node.find_all("comp") {
        let reference = required_value(component_node, "ref")?;
        // One invariant for every exported component, checked before the
        // footprint branch: an unassigned component never enters
        // `by_reference`, so a check there alone let a reference repeat
        // across the two roles and reach the plan (#507 review).
        if !seen_references.insert(reference.clone()) {
            bail!("KiCad netlist contains duplicate component reference {reference}");
        }

        let sheet_stamp = component_node
            .find("sheetpath")
            .and_then(|sheet| sheet.find_str("tstamps"))
            .context("KiCad netlist component has no sheet timestamp")?;
        // One UUID per unit of a multi-unit symbol, every one of which KiCad
        // matches a footprint by (`KICAD_NETLIST_PARSER::parseComponent`).
        let symbol_stamps = component_node
            .find("tstamps")
            .and_then(SexpNode::children)
            .map(|children| {
                children
                    .iter()
                    .skip(1)
                    .filter_map(SexpNode::as_str)
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        let (symbol_stamp, other_stamps) = symbol_stamps
            .split_first()
            .context("KiCad netlist component has no symbol timestamp")?;
        let unit_path = |stamp: &str| {
            format!(
                "/{}/{}",
                sheet_stamp.trim_matches('/'),
                stamp.trim_matches('/')
            )
            .replace("//", "/")
        };
        let symbol_path = unit_path(symbol_stamp);
        let other_unit_paths = other_stamps
            .iter()
            .map(|stamp| unit_path(stamp))
            .collect::<Vec<_>>();
        let dnp = component_node.find_all("property").iter().any(|property| {
            property.find_str("name") == Some("dnp")
                || property.get(1).and_then(SexpNode::as_str) == Some("dnp")
        });
        let exclude_from_bom = component_node.find_all("property").iter().any(|property| {
            property.find_str("name") == Some("exclude_from_bom")
                || property.get(1).and_then(SexpNode::as_str) == Some("exclude_from_bom")
        });

        let value = required_value(component_node, "value")?;
        // kicad-cli writes no `(footprint …)` node at all for a symbol whose
        // Footprint property is empty — only a bare `(field (name
        // "Footprint"))`. That is a legitimate state (a generic `Device:R`
        // whose package has not been chosen yet), and it used to fail the
        // whole sync for every other component with it (#507).
        let footprint_id = component_node
            .find_str("footprint")
            .map(str::trim)
            .filter(|footprint| !footprint.is_empty())
            .map(str::to_owned);
        let Some(footprint_id) = footprint_id else {
            let lib_id = component_node.find("libsource").and_then(|source| {
                let lib = source.find_str("lib")?;
                let part = source.find_str("part")?;
                (!lib.is_empty() || !part.is_empty()).then(|| format!("{lib}:{part}"))
            });
            unassigned_references.insert(reference.clone());
            unassigned.push(UnassignedComponent {
                reference,
                value,
                lib_id,
                symbol_path,
            });
            continue;
        };

        let index = components.len();
        by_reference.insert(reference.clone(), index);
        components.push(DesignComponent {
            reference,
            value,
            footprint_id,
            symbol_path,
            other_unit_paths,
            dnp,
            exclude_from_bom,
            pad_nets: BTreeMap::new(),
            unconnected_pins: BTreeSet::new(),
            fields: symbol_fields(component_node),
            sheet: symbol_sheet(component_node),
        });
    }

    if components.is_empty() && unassigned.is_empty() {
        bail!("KiCad netlist contains zero components");
    }

    let mut net_names = BTreeSet::new();
    if let Some(nets_node) = root.find("nets") {
        for net_node in nets_node.find_all("net") {
            let net_name = required_value(net_node, "name")?;
            let nodes = net_node.find_all("node");
            // KiCad names the net of a pin wired to nothing
            // `unconnected-(<ref>-<pin>-Pad<n>)`, and a pin with a no-connect
            // flag, or of the no-connect type, has `no_connect` in its pin
            // type. Alone on its net, such a pin connects nothing. A pin with
            // a no-connect flag that is wired to other pins is still
            // connected, and so is a named net of one pin.
            if let [node] = nodes.as_slice() {
                let no_connect = node
                    .find_str("pintype")
                    .is_some_and(|pin_type| pin_type.contains("no_connect"));
                if no_connect || net_name.starts_with("unconnected-") {
                    let reference = required_value(node, "ref")?;
                    let pin = required_value(node, "pin")?;
                    if let Some(&index) = by_reference.get(&reference) {
                        components[index].unconnected_pins.insert(pin);
                    }
                }
            }
            for node in nodes {
                let reference = required_value(node, "ref")?;
                let pin = required_value(node, "pin")?;
                // KiCad's netlist holds the net for any component's pin,
                // whether or not the component has a footprint.
                if !net_name.is_empty() {
                    net_names.insert(net_name.clone());
                }
                let Some(&index) = by_reference.get(&reference) else {
                    // A wired pin of a component with no footprint: there is
                    // no pad to carry the net, so the node is dropped, not
                    // fatal.
                    if unassigned_references.contains(&reference) {
                        continue;
                    }
                    bail!("net {net_name} refers to unknown component {reference}");
                };
                if components[index]
                    .pad_nets
                    .insert(pin.clone(), net_name.clone())
                    .is_some()
                {
                    bail!("component {reference} pad {pin} appears in more than one net");
                }
            }
        }
    }

    Ok(ExportedDesign {
        root_sheet_uuid: None,
        components,
        skipped: Vec::new(),
        unassigned,
        net_names,
    })
}

/// The fields KiCad's Update PCB copies from a netlist `(comp …)`: every
/// `(fields (field (name …) "value"))` but Reference, Value, Footprint and
/// Component Class, an empty one included. A repeated name keeps its first
/// position and its last value, as KiCad's reader does (#788).
fn symbol_fields(component_node: &SexpNode) -> Vec<SymbolField> {
    let mut fields = Vec::new();
    let Some(listed) = component_node.find("fields") else {
        return fields;
    };
    for field in listed.find_all("field") {
        let Some(name) = field.find_str("name").filter(|name| !name.is_empty()) else {
            continue;
        };
        if matches!(
            name,
            "Reference" | "Value" | "Footprint" | "Component Class"
        ) {
            continue;
        }
        let value = field
            .children()
            .unwrap_or_default()
            .iter()
            .skip(1)
            .find_map(SexpNode::as_str)
            .unwrap_or_default()
            .to_string();
        match fields.iter_mut().find(|known| known.name == name) {
            Some(known) => known.value = value,
            None => fields.push(SymbolField {
                name: name.to_string(),
                value,
            }),
        }
    }
    fields
}

/// The sheet name, sheet file, footprint filters and units KiCad's Update PCB
/// gives a `(comp …)`'s footprint: the sheet path's `names`, or the
/// `Sheetname` property when they are empty, the `Sheetfile` property, the
/// `ki_fp_filters` property and the `(units …)` node. A property named twice
/// keeps its last value, as KiCad's reader does.
fn symbol_sheet(component_node: &SexpNode) -> SymbolSheet {
    let property = |name: &str| {
        component_node
            .find_all("property")
            .into_iter()
            .rev()
            .find(|property| property.find_str("name") == Some(name))
            .and_then(|property| property.find_str("value"))
            .unwrap_or_default()
            .to_string()
    };
    let names = component_node
        .find("sheetpath")
        .and_then(|sheet| sheet.find_str("names"))
        .filter(|names| !names.is_empty());
    SymbolSheet {
        sheet_name: names.map_or_else(|| property("Sheetname"), str::to_owned),
        sheet_file: property("Sheetfile"),
        footprint_filters: property("ki_fp_filters"),
        symbol_units: symbol_units(component_node),
    }
}

/// Each `(unit (name …) (pins (pin (num …)) …))` of a `(comp …)`'s
/// `(units …)`, in order; a component without the node has no units.
fn symbol_units(component_node: &SexpNode) -> Vec<SymbolUnit> {
    let Some(units) = component_node.find("units") else {
        return Vec::new();
    };
    units
        .find_all("unit")
        .into_iter()
        .map(|unit| SymbolUnit {
            name: unit.find_str("name").unwrap_or_default().to_string(),
            pin_numbers: unit
                .find_all("pins")
                .into_iter()
                .flat_map(|pins| pins.find_all("pin"))
                .filter_map(|pin| pin.find_str("num"))
                .map(str::to_string)
                .collect(),
        })
        .collect()
}

fn required_value(node: &SexpNode, tag: &str) -> Result<String> {
    node.find_str(tag)
        .map(str::to_owned)
        .with_context(|| format!("KiCad netlist node is missing {tag}"))
}

pub(super) fn saved_hierarchy_files(root: &Path) -> Result<Vec<PathBuf>> {
    fn visit(
        path: &Path,
        seen: &mut HashSet<PathBuf>,
        active: &mut HashSet<PathBuf>,
        files: &mut Vec<PathBuf>,
    ) -> Result<()> {
        let canonical = path
            .canonicalize()
            .with_context(|| format!("cannot resolve schematic {}", path.display()))?;
        if active.contains(&canonical) {
            bail!(
                "schematic hierarchy contains a cycle at {}",
                display_path(&canonical)
            );
        }
        if !seen.insert(canonical.clone()) {
            return Ok(());
        }
        active.insert(canonical.clone());
        let name = canonical
            .file_name()
            .and_then(|name| name.to_str())
            .context("schematic path has no file name")?;
        let lock = canonical.with_file_name(format!("~{name}.lck"));
        if lock.exists() {
            bail!(
                "{} is open in the schematic editor; save and close the hierarchy before syncing",
                display_path(&canonical)
            );
        }
        let schematic = copperpour_schematic_editor::Schematic::load(&canonical)
            .with_context(|| format!("cannot load schematic {}", display_path(&canonical)))?;
        files.push(canonical.clone());
        let parent = canonical.parent().unwrap_or_else(|| Path::new("."));
        for sheet in schematic.sheets.iter() {
            let child = parent.join(sheet.file());
            if !child.exists() {
                bail!(
                    "hierarchical sheet {} referenced by {} does not exist",
                    display_path(&child),
                    display_path(&canonical)
                );
            }
            visit(&child, seen, active, files)?;
        }
        active.remove(&canonical);
        Ok(())
    }

    let mut files = Vec::new();
    visit(root, &mut HashSet::new(), &mut HashSet::new(), &mut files)?;
    Ok(files)
}

pub(super) fn apply_saved_symbol_flags(
    files: &[PathBuf],
    design: &mut ExportedDesign,
) -> Result<()> {
    #[derive(Debug)]
    struct Flags {
        reference: String,
        symbol_path: String,
        in_bom: bool,
        on_board: bool,
        dnp: bool,
    }

    let mut flags = Vec::new();
    for (index, file) in files.iter().enumerate() {
        let source = std::fs::read_to_string(file)?;
        let tree = copperpour_sexp::parse_sexp(&source)?;
        let file_uuid = tree.find_str("uuid").unwrap_or("");
        if index == 0 && !file_uuid.is_empty() {
            design.root_sheet_uuid = Some(file_uuid.to_string());
        }
        for symbol in tree.find_all("symbol") {
            let Some(uuid) = symbol.find_str("uuid") else {
                continue;
            };
            let in_bom = symbol.find_str("in_bom") != Some("no");
            let on_board = symbol.find_str("on_board") != Some("no");
            let dnp = symbol.find_str("dnp") == Some("yes");
            let projects = symbol
                .find("instances")
                .map(|instances| instances.find_all("project"))
                .unwrap_or_default();
            for project in projects {
                for path in project.find_all("path") {
                    let Some(reference) = path.find_str("reference") else {
                        continue;
                    };
                    let instance = path.get(1).and_then(SexpNode::as_str).unwrap_or("/");
                    let base = if instance == "/" && !file_uuid.is_empty() {
                        format!("/{file_uuid}")
                    } else {
                        instance.trim_end_matches('/').to_string()
                    };
                    flags.push(Flags {
                        reference: reference.to_string(),
                        symbol_path: format!("{base}/{uuid}").replace("//", "/"),
                        in_bom,
                        on_board,
                        dnp,
                    });
                }
            }
        }
    }

    for reference in flags
        .iter()
        .map(|entry| entry.reference.as_str())
        .collect::<HashSet<_>>()
    {
        let entries = flags
            .iter()
            .filter(|entry| entry.reference == reference)
            .collect::<Vec<_>>();
        if entries.iter().any(|entry| {
            entry.in_bom != entries[0].in_bom
                || entry.on_board != entries[0].on_board
                || entry.dnp != entries[0].dnp
        }) {
            bail!("multi-unit reference {reference} has inconsistent board/BOM/DNP flags");
        }
    }

    let root_uuid = design.root_sheet_uuid.as_deref();
    design.components.retain_mut(|component| {
        let component_path = normalize_symbol_path(&component.symbol_path, root_uuid);
        let path_match = flags
            .iter()
            .find(|entry| normalize_symbol_path(&entry.symbol_path, root_uuid) == component_path);
        let reference_matches = flags
            .iter()
            .filter(|entry| entry.reference == component.reference)
            .collect::<Vec<_>>();
        let entry = path_match.or_else(|| reference_matches.first().copied());
        let Some(entry) = entry else {
            return true;
        };
        if !entry.on_board {
            design.skipped.push(SkippedComponent {
                reference: entry.reference.clone(),
                symbol_path: entry.symbol_path.clone(),
            });
            return false;
        }
        component.dnp = entry.dnp;
        component.exclude_from_bom = !entry.in_bom;
        true
    });
    // The same flag governs a component with no footprint: excluded from the
    // board it is skipped by flag rather than reported as unassigned. Only
    // Exclude from board keeps a part off the PCB, as in KiCad (#875).
    design.unassigned.retain(|component| {
        let component_path = normalize_symbol_path(&component.symbol_path, root_uuid);
        let path_match = flags
            .iter()
            .find(|entry| normalize_symbol_path(&entry.symbol_path, root_uuid) == component_path);
        let entry = path_match.or_else(|| {
            flags
                .iter()
                .find(|entry| entry.reference == component.reference)
        });
        let Some(entry) = entry else {
            return true;
        };
        if !entry.on_board {
            design.skipped.push(SkippedComponent {
                reference: entry.reference.clone(),
                symbol_path: entry.symbol_path.clone(),
            });
            return false;
        }
        true
    });
    let mut skipped_references = HashSet::new();
    for entry in flags.iter().filter(|entry| !entry.on_board) {
        if !skipped_references.insert(entry.reference.as_str()) {
            continue;
        }
        let entry_path = normalize_symbol_path(&entry.symbol_path, root_uuid);
        if !design
            .skipped
            .iter()
            .any(|skipped| normalize_symbol_path(&skipped.symbol_path, root_uuid) == entry_path)
        {
            design.skipped.push(SkippedComponent {
                reference: entry.reference.clone(),
                symbol_path: entry.symbol_path.clone(),
            });
        }
    }
    Ok(())
}
