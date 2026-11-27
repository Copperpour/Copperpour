//! Compare saved embedded definitions without reconstructing KiCad's export order.

use super::properties;
use copperpour_sexp::SexpNode;
use std::collections::BTreeMap;

type Symbols<'a> = BTreeMap<&'a str, &'a SexpNode>;
type Records<'a> = Vec<&'a SexpNode>;
type Field<'a> = (bool, Option<&'a SexpNode>, Option<&'a SexpNode>);

#[derive(PartialEq)]
struct Pin<'a> {
    electrical_type: Option<&'a SexpNode>,
    number: Records<'a>,
    name: Records<'a>,
    position: Vec<[Option<&'a SexpNode>; 2]>,
}

impl<'a> Pin<'a> {
    fn from_node(node: &'a SexpNode) -> Self {
        let text = |key| {
            node.find_all(key)
                .into_iter()
                .filter_map(|n| n.get(1))
                .collect()
        };
        Self {
            electrical_type: node.get(1),
            number: text("number"),
            name: text("name"),
            position: node
                .find_all("at")
                .into_iter()
                .map(|at| [at.get(1), at.get(2)])
                .collect(),
        }
    }
}

struct Unit<'a> {
    key: Option<&'a SexpNode>,
    names: Records<'a>,
    pins: Vec<Pin<'a>>,
}

impl<'a> Unit<'a> {
    fn from_node(node: &'a SexpNode) -> Self {
        Self {
            key: node.get(1),
            names: node.find_all("unit_name"),
            pins: node
                .find_all("pin")
                .into_iter()
                .map(Pin::from_node)
                .collect(),
        }
    }
}

struct Metadata<'a> {
    duplicate_jumpers: Records<'a>,
    jumper_groups: Records<'a>,
    fields: Vec<Field<'a>>,
    units: Vec<Unit<'a>>,
}

impl<'a> Metadata<'a> {
    fn from_node(node: &'a SexpNode) -> Self {
        let fields = node
            .find_all("property")
            .into_iter()
            .filter_map(|field| {
                let private = matches!(field.get(1), Some(SexpNode::Atom(tag)) if tag == "private");
                let start = 1 + usize::from(private);
                let name = field.get(start);
                match name.and_then(SexpNode::as_str) {
                    // KiCad reads ki_locked as a library attribute, not an exported field.
                    Some("ki_locked") => None,
                    // Embedded description, keywords and footprint filters are tracked in #938.
                    Some("Description" | "ki_description" | "ki_keywords" | "ki_fp_filters") => {
                        None
                    }
                    _ => Some((private, name, field.get(start + 1))),
                }
            })
            .collect();
        Self {
            duplicate_jumpers: node.find_all("duplicate_pin_numbers_are_jumpers"),
            jumper_groups: node.find_all("jumper_pin_groups"),
            fields,
            units: std::iter::once(node)
                .chain(node.find_all("symbol"))
                .map(Unit::from_node)
                .collect(),
        }
    }

    fn append_changes(&self, other: &Self, subject: &str, reasons: &mut Vec<String>) {
        if self.duplicate_jumpers != other.duplicate_jumpers {
            reasons.push(format!(
                "{subject} changed duplicate_pin_numbers_are_jumpers"
            ));
        }
        if self.jumper_groups != other.jumper_groups {
            reasons.push(format!("{subject} changed jumper pin groups"));
        }
        if self.fields != other.fields {
            reasons.push(format!("{subject} changed embedded library fields"));
        }
        if self
            .units
            .iter()
            .map(|u| u.key)
            .ne(other.units.iter().map(|u| u.key))
        {
            reasons.push(format!(
                "{subject} changed embedded unit or body style records"
            ));
        } else {
            for (before, after) in self.units.iter().zip(&other.units) {
                let key = before.key.and_then(SexpNode::as_str).unwrap_or("?");
                if before.names != after.names {
                    reasons.push(format!("{subject} changed embedded unit name in {key:?}"));
                }
                if before.pins != after.pins {
                    reasons.push(format!("{subject} changed embedded pin records in {key:?}"));
                }
            }
        }
    }
}

fn definitions(sheet: &SexpNode) -> Symbols<'_> {
    // Duplicate keys keep the last definition, as KiCad does.
    sheet
        .find("lib_symbols")
        .into_iter()
        .flat_map(|libs| libs.find_all("symbol"))
        .filter_map(|node| Some((node.get(1)?.as_str()?, node)))
        .collect()
}

fn library_key(symbol: &SexpNode) -> Option<&str> {
    symbol
        .find_str("lib_name")
        .or_else(|| symbol.find_str("lib_id"))
}

pub(super) fn append_changes(
    before: (&SexpNode, &Symbols<'_>),
    after: (&SexpNode, &Symbols<'_>),
    reasons: &mut Vec<String>,
) {
    let (old_defs, new_defs) = (definitions(before.0), definitions(after.0));
    // Identical saved records can have different meanings across file formats.
    let version_changed = before.0.find_all("version") != after.0.find_all("version");
    let mut old_metadata = BTreeMap::new();
    let mut new_metadata = BTreeMap::new();
    for (&uuid, &was) in before.1 {
        let Some(&now) = after.1.get(uuid) else {
            continue;
        };
        if version_changed {
            let baseline_version = before.0.find_str("version").unwrap_or("unknown");
            let candidate_version = after.0.find_str("version").unwrap_or("unknown");
            reasons.push(format!(
                "cannot compare embedded symbol metadata across schematic format versions \
                 (baseline {baseline_version}, candidate {candidate_version})"
            ));
            return;
        }
        let reference = properties::reference(was).unwrap_or("?");
        let subject = format!("symbol {uuid} ({reference})");
        if was.find_all("lib_name") != now.find_all("lib_name") {
            reasons.push(format!(
                "{subject} changed embedded library identity (lib_name)"
            ));
            continue;
        }
        if let Some(key) = library_key(was) {
            // The earlier identity and connectivity checks have already run.
            if let (Some(old), Some(new)) = (old_defs.get(key), new_defs.get(key)) {
                let old = old_metadata
                    .entry(key)
                    .or_insert_with(|| Metadata::from_node(old));
                let new = new_metadata
                    .entry(key)
                    .or_insert_with(|| Metadata::from_node(new));
                old.append_changes(new, &subject, reasons);
            }
        }
    }
}
