//! Direct saved export properties for layout comparison. No other files are read.

use copperpour_sexp::SexpNode;
use std::collections::{BTreeMap, BTreeSet};

#[path = "sch_layout_areas.rs"]
mod areas;
#[path = "sch_layout_expressions.rs"]
mod expressions;

type Instances<'a> = BTreeMap<(&'a str, &'a str), &'a SexpNode>;
type Fields<'a> = BTreeMap<&'a str, &'a str>;

pub(super) struct Source<'a> {
    tree: &'a SexpNode,
    version: u32,
}

impl<'a> Source<'a> {
    pub(super) fn comparison(before: &'a SexpNode, after: &'a SexpNode) -> (Self, Self) {
        let source = |tree: &'a SexpNode| Self {
            tree,
            version: tree
                .find_str("version")
                .and_then(|version| version.parse().ok())
                .unwrap_or(u32::MAX),
        };
        (source(before), source(after))
    }
}

fn projects(item: &SexpNode) -> impl Iterator<Item = (&str, &SexpNode)> {
    item.find_all("instances")
        .into_iter()
        .flat_map(|instances| instances.find_all("project"))
        .filter_map(|node| Some((node.get(1)?.as_str()?, node)))
}

fn instance_records(item: &SexpNode) -> impl Iterator<Item = ((&str, &str), &SexpNode)> {
    projects(item).flat_map(|(project, node)| {
        node.find_all("path")
            .into_iter()
            .filter_map(move |path| Some(((project, path.get(1)?.as_str()?), path)))
    })
}

fn instances(item: &SexpNode) -> Instances<'_> {
    instance_records(item).collect()
}

// KiCad can look up instances by path alone. Keep competing records in order.
fn repeated_instance_paths(item: &SexpNode) -> BTreeMap<&str, Vec<(&str, &SexpNode)>> {
    let mut by_path = BTreeMap::<_, Vec<_>>::new();
    for ((project, path), node) in instance_records(item) {
        by_path.entry(path).or_default().push((project, node));
    }
    by_path.retain(|_, records| records.len() > 1);
    by_path
}

type InstanceIdentity<'a> = BTreeMap<(&'a str, &'a str), [Option<&'a str>; 4]>;

pub(super) fn instance_identity(item: &SexpNode) -> (BTreeSet<&str>, InstanceIdentity<'_>) {
    let projects = projects(item).map(|(name, _)| name).collect();
    let paths = instances(item)
        .into_iter()
        .map(|(key, path)| {
            let values = ["reference", "unit", "value", "footprint"].map(|tag| path.find_str(tag));
            (key, values)
        })
        .collect();
    (projects, paths)
}

pub(super) fn saved_root_reference<'a>(item: &'a SexpNode, sheet: &SexpNode) -> Option<&'a str> {
    if sheet.find_all("uuid").len() != 1 {
        return None;
    }
    let root = sheet.find_str("uuid")?;
    // The root needs one unambiguous reference. No other saved placement
    // may need the placed Reference as a fallback.
    let mut saved = None;
    for ((_, path), record) in instance_records(item) {
        if record.find_all("reference").len() != 1 {
            return None;
        }
        let reference = record
            .find_str("reference")
            .filter(|value| !value.is_empty())?;
        if path.strip_prefix('/') != Some(root) {
            continue;
        }
        if saved.is_some() {
            return None;
        }
        saved = Some(reference);
    }
    saved
}

fn property(node: &SexpNode) -> Option<(&str, &str, bool)> {
    let private = matches!(node.get(1), Some(SexpNode::Atom(key)) if key == "private");
    let offset = usize::from(private);
    Some((
        node.get(1 + offset)?.as_str()?,
        node.get(2 + offset)?.as_str()?,
        private,
    ))
}

pub(super) fn reference(item: &SexpNode) -> Option<&str> {
    item.find_all("property")
        .into_iter()
        .filter_map(property)
        .rfind(|(name, _, _)| name.eq_ignore_ascii_case("Reference"))
        .map(|(_, value, _)| value)
}

type RepeatedFields<'a> = BTreeMap<String, Vec<(&'a str, &'a str, bool)>>;

fn repeated_fields(item: &SexpNode) -> RepeatedFields<'_> {
    let mut by_name = BTreeMap::<_, Vec<_>>::new();
    for field @ (name, _, _) in item.find_all("property").into_iter().filter_map(property) {
        let key = match name.to_ascii_lowercase().as_str() {
            "sheet name" => "sheetname".to_owned(),
            "sheet file" => "sheetfile".to_owned(),
            name => name.to_owned(),
        };
        by_name.entry(key).or_default().push(field);
    }
    by_name.retain(|_, records| records.len() > 1);
    by_name
}

fn ordered_fields(item: &SexpNode) -> impl Iterator<Item = (&str, &str, bool)> {
    item.find_all("property").into_iter().filter_map(property)
}

fn ambiguous_instance_records(item: &SexpNode) -> bool {
    item.find_all("instances").len() > 1
        || instance_records(item).any(|(_, path)| {
            ["reference", "unit", "value", "footprint"]
                .into_iter()
                .any(|tag| path.find_all(tag).len() > 1)
        })
}

fn instance_order(item: &SexpNode) -> impl Iterator<Item = (&str, &str)> {
    instance_records(item).map(|(key, _)| key)
}

fn loaded_field_text(value: &str, version: u32) -> &str {
    if version < 20250318 && value == "~" {
        ""
    } else {
        value
    }
}

fn fields(item: &SexpNode, version: u32) -> Fields<'_> {
    let is_sheet = item.head() == Some("sheet");
    let mandatory: &[&str] = if is_sheet {
        &["Sheetname", "Sheetfile"]
    } else {
        &["Value", "Footprint", "Datasheet", "Description"]
    };
    let mut fields: Fields<'_> = mandatory.iter().map(|&name| (name, "")).collect();
    let mut seen = BTreeSet::new();
    for (name, value, private) in item.find_all("property").into_iter().filter_map(property) {
        let mandatory_name = mandatory
            .iter()
            .copied()
            .find(|key| key.eq_ignore_ascii_case(name));
        if !is_sheet
            && ((private && mandatory_name.is_none()) || name.eq_ignore_ascii_case("Reference"))
        {
            continue;
        }
        let value = loaded_field_text(value, version);
        let name = if is_sheet && name.eq_ignore_ascii_case("Sheet name") {
            "Sheetname"
        } else if is_sheet && name.eq_ignore_ascii_case("Sheet file") {
            "Sheetfile"
        } else {
            mandatory_name.unwrap_or(name)
        };
        if !is_sheet || seen.insert(name) {
            fields.insert(name, value);
        }
    }
    fields
}

const FLAGS: [(&str, &str); 5] = [
    ("dnp", "no"),
    ("in_bom", "yes"),
    ("on_board", "yes"),
    ("in_pos_files", "yes"),
    ("exclude_from_sim", "no"),
];

fn flag(node: &SexpNode, name: &str, version: u32) -> Option<bool> {
    Some(loaded_flag_value(
        node.head()?,
        name,
        version,
        node.find_str(name)?,
    ))
}

fn loaded_flag_value(kind: &str, name: &str, version: u32, value: &str) -> bool {
    let yes = value == "yes";
    // KiCad fixed inverted variant in_bom read/write in 20260306.
    if name == "in_bom" && kind == "variant" && version < 20260306 {
        !yes
    } else {
        yes
    }
}

fn inherited_flag(item: &SexpNode, name: &str, default: bool) -> bool {
    // A variant inherits the parent's flags seen before its instance records.
    item.children()
        .unwrap_or_default()
        .iter()
        .take_while(|node| node.head() != Some("instances"))
        .filter(|node| node.head() == Some(name))
        .filter_map(|node| node.get(1)?.as_str())
        .last()
        .map_or(default, |value| value == "yes")
}

fn export_flags<'a>(
    item: &'a SexpNode,
    version: u32,
    variant: Option<&'a SexpNode>,
) -> impl Iterator<Item = (&'static str, bool)> + 'a {
    FLAGS
        .into_iter()
        .filter(move |(name, _)| item.head() != Some("sheet") || *name != "in_pos_files")
        .map(move |(name, default)| {
            let default = default == "yes";
            let value = match variant {
                Some(variant) => flag(variant, name, version)
                    .unwrap_or_else(|| inherited_flag(item, name, default)),
                None => flag(item, name, version).unwrap_or(default),
            };
            (name, value)
        })
}

fn variants(path: Option<&SexpNode>) -> BTreeMap<&str, &SexpNode> {
    path.into_iter()
        .flat_map(|path| path.find_all("variant"))
        .filter_map(|variant| Some((variant.find_str("name")?, variant)))
        .collect()
}

fn variant_fields(variant: Option<&SexpNode>) -> Fields<'_> {
    variant
        .into_iter()
        .flat_map(|variant| variant.find_all("field"))
        .filter_map(|field| Some((field.find_str("name")?, field.find_str("value")?)))
        .collect()
}

fn has_ambiguous_variant_entries(variant: &SexpNode) -> bool {
    let mut tags = BTreeSet::new();
    let mut fields = BTreeSet::new();
    variant
        .children()
        .unwrap_or_default()
        .iter()
        .any(|node| match node.head() {
            Some("field") => node.find_str("name").is_none_or(|name| {
                !fields.insert(name.to_ascii_lowercase())
                    || node.find_all("name").len() != 1
                    || node.find_all("value").len() != 1
            }),
            Some(tag) => !tags.insert(tag),
            None => false,
        })
}

fn ambiguous_variants(path: Option<&SexpNode>) -> bool {
    let mut names = BTreeSet::new();
    path.into_iter()
        .flat_map(|path| path.find_all("variant"))
        .any(|variant| {
            variant
                .find_str("name")
                .is_none_or(|name| !names.insert(name))
        })
}

fn keys<'a, K: Ord, A, B>(
    before: &'a BTreeMap<K, A>,
    after: &'a BTreeMap<K, B>,
) -> BTreeSet<&'a K> {
    before.keys().chain(after.keys()).collect()
}

fn field_changes(subject: &str, was: &Fields<'_>, now: &Fields<'_>, reasons: &mut Vec<String>) {
    for name in keys(was, now) {
        if was.get(name) != now.get(name) {
            reasons.push(format!("{subject} changed field {name:?}"));
        }
    }
}

pub(super) fn changes(
    subject: &str,
    (before, old): (&SexpNode, &Source<'_>),
    (after, new): (&SexpNode, &Source<'_>),
    reasons: &mut Vec<String>,
) {
    let is_sheet = before.head() == Some("sheet");
    if old.tree.find_all("uuid") != new.tree.find_all("uuid")
        && [before, after]
            .into_iter()
            .any(|item| item.find("instances").is_some())
    {
        reasons.push(format!(
            "{subject} changed the root UUID used to select saved instances"
        ));
        return;
    }
    if repeated_instance_paths(before) != repeated_instance_paths(after)
        || !instance_order(before).eq(instance_order(after))
            && instance_identity(before) == instance_identity(after)
        || before.find_all("instances") != after.find_all("instances")
            && (ambiguous_instance_records(before) || ambiguous_instance_records(after))
    {
        reasons.push(format!(
            "{subject} changed ambiguous saved instance-path records"
        ));
        return;
    }
    if is_sheet && instance_identity(before) != instance_identity(after) {
        reasons.push(format!("{subject} changed its saved instance paths"));
    }
    if (!repeated_fields(before).is_empty() || !repeated_fields(after).is_empty())
        && (old.version != new.version || !ordered_fields(before).eq(ordered_fields(after)))
    {
        reasons.push(format!("{subject} changed ambiguous saved field records"));
        return;
    }
    // KiCad can take legacy defaults instead of the placed property.
    // Do not normalize variant overrides against the placed value in that case.
    if [before, after]
        .into_iter()
        .any(has_legacy_instance_defaults)
        && (old.version != new.version || !default_inputs(before).eq(default_inputs(after)))
    {
        reasons.push(format!("{subject} changed its saved default instance"));
    }
    if let Some(name) = [before, after]
        .into_iter()
        .find_map(|item| ordered_fields(item).find_map(|(name, _, _)| legacy_sim_property(name)))
    {
        if old.version != new.version || !legacy_sim_inputs(before).eq(legacy_sim_inputs(after)) {
            reasons.push(format!(
                "{subject} changed legacy simulation attribute {name}"
            ));
        }
    }
    // Conflicting repeated flags must not disappear behind the normalized view.
    if FLAGS.into_iter().any(|(name, _)| {
        let was = before.find_all(name);
        let now = after.find_all(name);
        (was.len() > 1 || now.len() > 1) && was != now
    }) {
        reasons.push(format!("{subject} changed ambiguous saved export flags"));
        return;
    }

    let (old_fields, new_fields) = (fields(before, old.version), fields(after, new.version));
    field_changes(subject, &old_fields, &new_fields, reasons);
    for ((name, was), (_, now)) in
        export_flags(before, old.version, None).zip(export_flags(after, new.version, None))
    {
        if was != now && (is_sheet || name != "on_board") {
            reasons.push(format!("{subject} changed attribute {name}"));
        }
    }

    let (old_instances, new_instances) = (instances(before), instances(after));
    for key @ (project, path) in keys(&old_instances, &new_instances) {
        let (was_path, now_path) = (
            old_instances.get(key).copied(),
            new_instances.get(key).copied(),
        );
        if was_path != now_path && (ambiguous_variants(was_path) || ambiguous_variants(now_path)) {
            reasons.push(format!("{subject} in project {project:?}, path {path:?} changed ambiguous saved variant records"));
            continue;
        }
        let (old_variants, new_variants) = (variants(was_path), variants(now_path));
        for name in keys(&old_variants, &new_variants) {
            let (was, now) = (
                old_variants.get(name).copied(),
                new_variants.get(name).copied(),
            );
            let context =
                format!("{subject} in project {project:?}, path {path:?}, variant {name:?}");
            if was != now
                && [was, now]
                    .into_iter()
                    .flatten()
                    .any(has_ambiguous_variant_entries)
            {
                reasons.push(format!("{context} changed ambiguous saved variant records"));
                continue;
            }
            let mut was_effective = old_fields.clone();
            was_effective.extend(variant_fields(was));
            let mut now_effective = new_fields.clone();
            now_effective.extend(variant_fields(now));
            field_changes(&context, &was_effective, &now_effective, reasons);
            for ((attribute, was), (_, now)) in
                export_flags(before, old.version, was).zip(export_flags(after, new.version, now))
            {
                if was != now {
                    reasons.push(format!("{context} changed attribute {attribute}"));
                }
            }
        }
    }
}

#[derive(PartialEq)]
enum DefaultInput<'a> {
    Record(&'a SexpNode),
    Field(&'a str, &'a str, bool),
}

fn has_legacy_instance_defaults(item: &SexpNode) -> bool {
    item.find("default_instance").is_some()
        || instance_records(item)
            .any(|(_, path)| path.find("value").is_some() || path.find("footprint").is_some())
}

fn default_inputs(item: &SexpNode) -> impl Iterator<Item = DefaultInput<'_>> {
    item.children()
        .unwrap_or_default()
        .iter()
        .filter_map(|node| match node.head() {
            Some("default_instance" | "instances") => Some(DefaultInput::Record(node)),
            Some("property") => {
                let (name, value, private) = property(node)?;
                Some(DefaultInput::Field(name, value, private))
            }
            _ => None,
        })
}

fn legacy_sim_property(name: &str) -> Option<&'static str> {
    ["Sim.Enable", "Spice_Netlist_Enabled"]
        .into_iter()
        .find(|legacy| name.eq_ignore_ascii_case(legacy))
}

fn legacy_sim_inputs(item: &SexpNode) -> impl Iterator<Item = DefaultInput<'_>> {
    item.children()
        .unwrap_or_default()
        .iter()
        .filter_map(|node| match node.head()? {
            "exclude_from_sim" | "instances" => Some(DefaultInput::Record(node)),
            "property" => {
                let (name, value, private) = property(node)?;
                Some(DefaultInput::Field(
                    legacy_sim_property(name)?,
                    value,
                    private,
                ))
            }
            _ => None,
        })
}

fn sheets(tree: &SexpNode) -> Option<BTreeMap<&str, &SexpNode>> {
    let mut sheets = BTreeMap::new();
    for sheet in tree.find_all("sheet") {
        if sheets.insert(sheet.find_str("uuid")?, sheet).is_some() {
            return None;
        }
    }
    Some(sheets)
}

fn subject(item: &SexpNode, source: &Source<'_>, uuid: &str) -> String {
    if item.head() == Some("sheet") {
        let name = fields(item, source.version)["Sheetname"];
        format!("sheet instance {uuid} ({name:?}; affects its children)")
    } else {
        format!("symbol {uuid} ({})", reference(item).unwrap_or("?"))
    }
}

pub(super) fn sheet_changes(
    (before, old): (&SexpNode, &Source<'_>),
    (after, new): (&SexpNode, &Source<'_>),
    reasons: &mut Vec<String>,
) {
    let (Some(was), Some(now)) = (sheets(before), sheets(after)) else {
        reasons.push("a child-sheet instance has missing or duplicate identity".into());
        return;
    };
    for uuid in keys(&was, &now) {
        match (was.get(uuid), now.get(uuid)) {
            (Some(before), Some(after)) => {
                changes(
                    &subject(before, old, uuid),
                    (before, old),
                    (after, new),
                    reasons,
                );
            }
            (None, Some(after)) => reasons.push(format!("{} was added", subject(after, new, uuid))),
            (Some(before), None) => {
                reasons.push(format!("{} was removed", subject(before, old, uuid)))
            }
            (None, None) => unreachable!(),
        }
    }
}

fn placements(tree: &SexpNode) -> impl Iterator<Item = &SexpNode> {
    tree.find_all("symbol")
        .into_iter()
        .chain(tree.find_all("sheet"))
}

fn opaque_expression(value: &str) -> bool {
    value.contains("${") || value.contains("@{") || value.contains("{dollar}")
}

fn record_values<'a>(record: &'a SexpNode, name: &str) -> impl Iterator<Item = &'a str> {
    record
        .find_all(name)
        .into_iter()
        .filter_map(|node| node.get(1)?.as_str())
}

pub(super) fn uncertain_changes((old, new): (&Source<'_>, &Source<'_>), reasons: &mut Vec<String>) {
    if old.tree == new.tree {
        return;
    }
    let comparison = expressions::Comparison::new(old, new);
    areas::changes(old, new, &comparison, reasons);
    expressions::changes(&comparison, reasons);
}
