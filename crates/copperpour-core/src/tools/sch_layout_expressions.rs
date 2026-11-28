//! Prove equality of saved text inputs without expanding project variables.

use super::*;
use copperpour_sexp::schematic_rule_area::area_library;
use copperpour_sexp::title_block::{self, TitleBlock};
use std::cell::{Cell, OnceCell};

#[derive(Clone, Copy)]
struct View<'a> {
    item: &'a SexpNode,
    variant: Option<&'a SexpNode>,
}

type Pair<'a> = [View<'a>; 2];
type InstanceKey<'a> = (&'a str, &'a str);

#[derive(Clone, Copy, PartialEq, Eq)]
enum Difference {
    Same,
    Changed,
    Unproved,
}

struct Inputs<'a> {
    tree: &'a SexpNode,
    version: u32,
    title: Option<TitleBlock>,
    library: Vec<&'a SexpNode>,
    items: Vec<&'a SexpNode>,
}

impl<'a> Inputs<'a> {
    fn read(source: &Source<'a>) -> Self {
        Self {
            tree: source.tree,
            version: source.version,
            title: title_block::read(source.tree).ok(),
            library: area_library(source.tree),
            items: placements(source.tree).collect(),
        }
    }

    fn title_value(&self, token: &str) -> Option<&str> {
        let title = self.title.as_ref()?;
        Some(match token {
            "TITLE" => &title.title,
            "ISSUE_DATE" => &title.date,
            "REVISION" => &title.revision,
            "COMPANY" => &title.company,
            _ => {
                let [digit @ b'1'..=b'9'] = *token.strip_prefix("COMMENT")?.as_bytes() else {
                    return None;
                };
                &title.comments[usize::from(digit - b'1')]
            }
        })
    }

    fn has_dynamic_net_labels(&self) -> bool {
        ["label", "global_label", "hierarchical_label"]
            .into_iter()
            .flat_map(|tag| self.tree.find_all(tag))
            .any(|node| {
                node.get(1)
                    .and_then(SexpNode::as_str)
                    .is_some_and(opaque_expression)
            })
            || self.items.iter().any(|item| {
                self.library_candidates(item)
                    .any(|lib| lib.find("power").is_some())
                    && fields(item, self.version)
                        .get("Value")
                        .is_some_and(|v| opaque_expression(v))
            })
    }

    fn library_candidates<'b>(
        &'b self,
        item: &'b SexpNode,
    ) -> impl Iterator<Item = &'a SexpNode> + 'b {
        let ids: BTreeSet<_> = ["lib_id", "lib_name"]
            .into_iter()
            .flat_map(|tag| record_values(item, tag))
            .map(normalize_lib_key)
            .collect();
        self.library.iter().copied().filter(move |lib| {
            lib.get(1)
                .and_then(SexpNode::as_str)
                .is_some_and(|key| ids.contains(&normalize_lib_key(key)))
        })
    }

    fn targets(&self, key: &str) -> Vec<(&'a str, &'a SexpNode)> {
        self.items
            .iter()
            .filter_map(|&item| {
                let uuid = item.find_str("uuid")?;
                let matches_reference = |reference: &str| {
                    reference == key
                        || reference
                            .strip_prefix(key)
                            .is_some_and(|tail| matches!(tail.as_bytes(), [b'A'..=b'Z']))
                };
                (uuid.eq_ignore_ascii_case(key) || references(item).any(matches_reference))
                    .then_some((uuid, item))
            })
            .collect()
    }
}

fn normalize_lib_key(key: &str) -> std::borrow::Cow<'_, str> {
    if key.contains("{slash}") {
        key.replace("{slash}", "/").into()
    } else {
        key.into()
    }
}

fn references(item: &SexpNode) -> impl Iterator<Item = &str> {
    reference(item)
        .into_iter()
        .chain(instance_records(item).flat_map(|(_, path)| record_values(path, "reference")))
}

fn drawing_only(node: &SexpNode) -> bool {
    matches!(
        node.head(),
        Some("text" | "text_box" | "image" | "polyline" | "rectangle" | "circle" | "arc")
    )
}

fn possible_inputs(tree: &SexpNode) -> impl Iterator<Item = &SexpNode> {
    tree.children()
        .unwrap_or_default()
        .iter()
        .filter(|node| !drawing_only(node))
}

fn connection_inputs(tree: &SexpNode) -> impl Iterator<Item = Vec<&SexpNode>> {
    let has_areas = tree.find("rule_area").is_some();
    possible_inputs(tree).filter_map(move |node| match node.head() {
        Some("uuid" | "version" | "title_block" | "sheet_instances" | "lib_symbols") => {
            Some(vec![node])
        }
        Some("symbol" | "sheet" | "rule_area") if has_areas => Some(vec![node]),
        Some(
            "wire" | "bus" | "bus_entry" | "junction" | "no_connect" | "label" | "global_label"
            | "hierarchical_label" | "symbol" | "sheet" | "rule_area" | "directive_label"
            | "netclass_flag",
        ) => Some(
            node.children()
                .unwrap_or_default()
                .iter()
                .filter(|child| {
                    !matches!(
                        child.head(),
                        Some("effects" | "stroke" | "fields_autoplaced")
                    )
                })
                .collect(),
        ),
        _ => None,
    })
}

fn pin_records(lib: &SexpNode) -> impl Iterator<Item = &SexpNode> {
    fn unit_inputs(node: &SexpNode) -> impl Iterator<Item = &SexpNode> {
        node.children().unwrap_or_default().iter().filter(|child| {
            matches!(
                child.head(),
                Some("pin" | "unit_name" | "body_style_name" | "power" | "extends")
            )
        })
    }
    // Keep unit identifiers in order, including units with no pin records.
    unit_inputs(lib).chain(
        lib.find_all("symbol")
            .into_iter()
            .flat_map(|unit| unit.get(1).into_iter().chain(unit_inputs(unit))),
    )
}

fn contains_expression(node: &SexpNode) -> bool {
    node.as_str().is_some_and(opaque_expression)
        || node
            .children()
            .is_some_and(|items| items.iter().any(contains_expression))
}

fn placed_pin_records(item: &SexpNode) -> impl Iterator<Item = &SexpNode> {
    item.children()
        .unwrap_or_default()
        .iter()
        .filter(|node| {
            matches!(
                node.head(),
                Some("lib_id" | "lib_name" | "unit" | "body_style" | "convert" | "pin")
            )
        })
        .chain(instance_records(item).flat_map(|(_, path)| path.find_all("unit")))
}

fn pin_inputs_changed<'a>(inputs: &[Inputs<'a>; 2], items: [&'a SexpNode; 2]) -> bool {
    if items.iter().any(|item| item.head() != Some("symbol")) {
        return true;
    }
    let refs: BTreeSet<_> = items.into_iter().flat_map(references).collect();
    let related = |item: &SexpNode| {
        (item.head() == Some("symbol") && references(item).any(|name| refs.contains(name)))
            || items
                .iter()
                .any(|owner| owner.find_str("uuid") == item.find_str("uuid"))
    };
    let signatures = |source: &Inputs<'a>| {
        source
            .items
            .iter()
            .copied()
            .filter(|item| related(item))
            .map(|item| {
                (
                    item.find_str("uuid"),
                    placed_pin_records(item).collect::<Vec<_>>(),
                    source
                        .library_candidates(item)
                        .map(|lib| (lib.get(1), pin_records(lib).collect::<Vec<_>>()))
                        .collect::<Vec<_>>(),
                )
            })
            .collect::<Vec<_>>()
    };
    let [was, now] = inputs.each_ref().map(signatures);
    was != now
        || was
            .iter()
            .any(|(_, placed, _)| placed.iter().any(|node| contains_expression(node)))
        || was.iter().any(|(_, _, libraries)| {
            libraries
                .iter()
                .any(|(_, pins)| pins.iter().any(|node| contains_expression(node)))
        })
}

fn local_fields<'a>(view: View<'a>, version: u32) -> Fields<'a> {
    let mut result = Fields::new();
    for (name, value, _) in ordered_fields(view.item) {
        let value = loaded_field_text(value, version);
        if view.item.head() == Some("symbol") {
            result.insert(name, value);
        } else {
            result.entry(name).or_insert(value);
        }
    }
    result.extend(fields(view.item, version));
    result.extend(variant_fields(view.variant));
    result
}

fn lookup<'a>(item: &SexpNode, values: &Fields<'a>, token: &str) -> Option<&'a str> {
    match item.head() {
        Some("symbol") => values
            .iter()
            .find_map(|(name, &value)| name.eq_ignore_ascii_case(token).then_some(value))
            .or_else(|| match token {
                "FOOTPRINT_LIBRARY" | "FOOTPRINT_NAME" => values.get("Footprint").copied(),
                _ => None,
            }),
        Some("sheet") => values
            .get(match token {
                "SHEETNAME" => "Sheetname",
                "SHEETFILE" => "Sheetfile",
                _ => token,
            })
            .copied(),
        _ => values.get(token).copied(),
    }
}

fn variant_views<'a>(
    item: &'a SexpNode,
    name: &str,
) -> BTreeMap<InstanceKey<'a>, Option<&'a SexpNode>> {
    instances(item)
        .into_iter()
        .map(|(key, path)| (key, variants(Some(path)).get(name).copied()))
        .collect()
}

fn page_inputs<'a>(item: &'a SexpNode, tree: &'a SexpNode) -> Vec<&'a SexpNode> {
    if item.head() == Some("sheet") {
        instance_records(item)
            .flat_map(|(_, path)| path.find_all("page"))
            .collect()
    } else {
        tree.find_all("sheet_instances")
    }
}

fn has_lookup_hazard(view: View<'_>) -> bool {
    // Repeated setters or a self-reference can stop lookup before later
    // fields and built-ins. Do not assume the normalized field map wins.
    let mut names = BTreeSet::new();
    ordered_fields(view.item).any(|(name, value, _)| {
        !name.is_ascii()
            || !names.insert(name.to_ascii_lowercase())
            || (opaque_expression(value)
                && value
                    .replace(' ', "")
                    .eq_ignore_ascii_case(&format!("${{{name}}}")))
    }) || view.variant.is_some_and(has_ambiguous_variant_entries)
}

pub(super) struct Comparison<'a> {
    inputs: [Inputs<'a>; 2],
    changed: bool,
    connection_changed: OnceCell<bool>,
    net_name_changed: OnceCell<bool>,
}

impl<'a> Comparison<'a> {
    pub(super) fn new(old: &Source<'a>, new: &Source<'a>) -> Self {
        Self {
            inputs: [Inputs::read(old), Inputs::read(new)],
            changed: !possible_inputs(old.tree).eq(possible_inputs(new.tree)),
            connection_changed: OnceCell::new(),
            net_name_changed: OnceCell::new(),
        }
    }

    fn walk(&self, pair: Pair<'a>, value: &str, depth: u8, budget: &Cell<u16>) -> Difference {
        if !self.changed {
            return Difference::Same;
        }
        if value.contains("{dollar}") || value.contains("@{") {
            return Difference::Unproved;
        }
        let Some((prefix, mut tail)) = value.split_once("${") else {
            return Difference::Same;
        };
        // Nested, escaped, malformed and composable fragments require the
        // fallback, rather than approximating the next expansion pass.
        if depth == 0 || prefix.ends_with(['\\', '$']) {
            return Difference::Unproved;
        }
        loop {
            let Some((token, rest)) = tail.split_once('}') else {
                return Difference::Unproved;
            };
            if token.is_empty()
                || token.contains(['{', '$'])
                || rest.starts_with(['{', '}'])
                || rest.starts_with("${")
            {
                return Difference::Unproved;
            }
            let Some(remaining) = budget.get().checked_sub(1) else {
                return Difference::Unproved;
            };
            budget.set(remaining);
            let result = self.token(pair, token, depth, budget);
            if result != Difference::Same {
                return result;
            }
            let Some((between, next)) = rest.split_once("${") else {
                return Difference::Same;
            };
            if between.ends_with(['\\', '$']) {
                return Difference::Unproved;
            }
            tail = next;
        }
    }

    fn token(&self, caller: Pair<'a>, token: &str, depth: u8, budget: &Cell<u16>) -> Difference {
        let (head, arguments) = token.split_once('(').unwrap_or((token, ""));
        let Some((target, field)) = head.split_once(':') else {
            return self.in_context(caller, token, depth, budget);
        };
        if target.contains('/') || arguments.contains(':') {
            return Difference::Unproved;
        }
        let (field, variant) = field
            .split_once(':')
            .map_or((field, None), |(field, variant)| (field, Some(variant)));
        if variant.is_some() && !arguments.is_empty() {
            return Difference::Unproved;
        }
        let field = if arguments.is_empty() {
            std::borrow::Cow::Borrowed(field)
        } else {
            format!("{field}({arguments}").into()
        };
        let [old, new] = self.inputs.each_ref().map(|input| input.targets(target));
        if old.len() != 1 || new.len() != 1 {
            return Difference::Unproved;
        }
        if old[0].0 != new[0].0 || old[0].1.head() != new[0].1.head() {
            return Difference::Changed;
        }
        {
            let [before, after] = [old[0].1, new[0].1];
            let pair = [
                View {
                    item: before,
                    variant: None,
                },
                View {
                    item: after,
                    variant: None,
                },
            ];
            let Some(remaining) = budget.get().checked_sub(1) else {
                return Difference::Unproved;
            };
            budget.set(remaining);
            let result = self.in_context(pair, &field, depth, budget);
            if result != Difference::Same {
                return result;
            }
            if let Some(variant) = variant {
                if pair.into_iter().any(|view| {
                    ambiguous_instance_records(view.item)
                        || instance_records(view.item).any(|(_, path)| {
                            ambiguous_variants(Some(path))
                                || variants(Some(path))
                                    .get(variant)
                                    .is_some_and(|record| has_ambiguous_variant_entries(record))
                        })
                }) {
                    return Difference::Unproved;
                }
                let [was, now] = pair.map(|view| variant_views(view.item, variant));
                if !was.keys().eq(now.keys()) {
                    return Difference::Changed;
                }
                for (key, was) in was {
                    let old_value = lookup(before, &variant_fields(was), &field);
                    let new_value = lookup(after, &variant_fields(now[&key]), &field);
                    if old_value != new_value {
                        return Difference::Changed;
                    }
                    if let Some(value) = old_value {
                        // The explicit override comes back as raw text and
                        // can expand in the caller rather than the target.
                        let result = self.alias(caller, value, depth, budget);
                        if result != Difference::Same {
                            return result;
                        }
                    }
                }
            }
        }
        Difference::Same
    }

    fn alias(&self, pair: Pair<'a>, value: &str, depth: u8, budget: &Cell<u16>) -> Difference {
        if (value.contains('$') && !opaque_expression(value)) || value.ends_with('{') {
            // An expansion can leave half of a token for a later pass.
            Difference::Unproved
        } else {
            self.walk(pair, value, depth - 1, budget)
        }
    }

    fn in_context(&self, pair: Pair<'a>, token: &str, depth: u8, budget: &Cell<u16>) -> Difference {
        let items = pair.map(|view| view.item);
        if pair.into_iter().any(has_lookup_hazard) {
            return Difference::Unproved;
        }
        let [old, new] = &self.inputs;
        let [was, now] = [
            local_fields(pair[0], old.version),
            local_fields(pair[1], new.version),
        ];
        let before = lookup(items[0], &was, token);
        let after = lookup(items[1], &now, token);
        if before != after {
            return Difference::Changed;
        }
        if let Some(value) = before {
            let result = self.alias(pair, value, depth, budget);
            if result != Difference::Same {
                return result;
            }
        }
        let [was_title, now_title] = self.inputs.each_ref().map(|input| input.title_value(token));
        if was_title != now_title {
            return Difference::Changed;
        }
        if let Some(value) = was_title {
            return self.alias(pair, value, depth, budget);
        }
        let same = match token {
            "#" => {
                let was = page_inputs(items[0], old.tree);
                let now = page_inputs(items[1], new.tree);
                // Page labels can introduce another substitution on a later
                // expansion pass. Equal saved labels do not prove equality.
                if was.iter().chain(&now).any(|page| contains_expression(page)) {
                    return Difference::Unproved;
                }
                was == now
            }
            "DNP" | "EXCLUDE_FROM_BOM" | "EXCLUDE_FROM_BOARD" | "EXCLUDE_FROM_SIM"
                if items[0].head() == Some("sheet") =>
            {
                areas::same_sheet_flag(token, [(items[0], old.tree), (items[1], new.tree)])
            }
            token
                if token.eq_ignore_ascii_case("Reference") && items[0].head() == Some("symbol") =>
            {
                references(items[0]).eq(references(items[1]))
                    && !pin_inputs_changed(&self.inputs, items)
            }
            "UNIT" => !pin_inputs_changed(&self.inputs, items),
            _ => {
                let call = token
                    .split_once('(')
                    .filter(|(_, arg)| {
                        arg.strip_suffix(')')
                            .is_some_and(|arg| !arg.is_empty() && !arg.contains(['(', ')', ':']))
                    })
                    .map(|(name, _)| name);
                match call {
                    Some("PIN_BASE_NAME" | "PIN_NAME" | "PIN_ALT_LIST") => {
                        !pin_inputs_changed(&self.inputs, items)
                    }
                    Some("NET_CLASS") => !*self.connection_changed.get_or_init(|| {
                        !connection_inputs(old.tree).eq(connection_inputs(new.tree))
                            || self.inputs.iter().any(Inputs::has_dynamic_net_labels)
                    }),
                    Some("NET_NAME" | "SHORT_NET_NAME") if items[0].head() == Some("symbol") => {
                        // Keep the existing net gate. It can omit weak
                        // single-pin nets and infer power drivers from base
                        // pin data, so compare electrical inputs as well.
                        !*self.net_name_changed.get_or_init(|| {
                            self.inputs.iter().any(Inputs::has_dynamic_net_labels)
                                || !net_name_inputs(old).eq(net_name_inputs(new))
                        }) && !pin_inputs_changed(&self.inputs, items)
                    }
                    _ if before.is_some() => true,
                    _ => return Difference::Unproved,
                }
            }
        };
        if same {
            Difference::Same
        } else {
            Difference::Changed
        }
    }

    pub(super) fn affects(
        &self,
        [before, after]: [&'a SexpNode; 2],
        [old_variant, new_variant]: [Option<&'a SexpNode>; 2],
        value: &str,
    ) -> bool {
        self.check(
            [
                View {
                    item: before,
                    variant: old_variant,
                },
                View {
                    item: after,
                    variant: new_variant,
                },
            ],
            value,
        ) != Difference::Same
    }

    fn check(&self, pair: Pair<'a>, value: &str) -> Difference {
        self.walk(pair, value, 24, &Cell::new(1024))
    }

    // Inspect raw records as well as the usual export views. Legacy
    // defaults, duplicate fields and malformed variant records can supply
    // text not represented by a last-wins map.
    fn exported_changes(&self, pair: Pair<'a>, subject: &str, reasons: &mut Vec<String>) {
        let [before, after] = pair.map(|view| view.item);
        let [old, new] = &self.inputs;
        let mut roots = Fields::new();
        for (view, input) in pair.into_iter().zip([old, new]) {
            if unmodelled_export(view.item).is_some() {
                continue;
            }
            roots.extend(fields(view.item, input.version));
            roots.extend(variant_fields(view.variant));
        }
        for (name, value) in roots {
            let reason = match self.check(pair, value) {
                Difference::Same => continue,
                Difference::Changed => format!("{subject} changed field {name:?}"),
                Difference::Unproved => format!(
                    "{subject} cannot establish unchanged field {name:?}: an unresolved expression has changed saved inputs"
                ),
            };
            if !reasons.contains(&reason) {
                reasons.push(reason);
            }
        }
        for item in [before, after] {
            if let Some(name) = unmodelled_export(item) {
                let reason = format!(
                    "{subject} cannot establish unchanged field {name:?}: exported text uses a substitution"
                );
                if !reasons.contains(&reason) {
                    reasons.push(reason);
                }
            }
        }
    }
}

fn net_name_inputs<'a>(
    source: &Inputs<'a>,
) -> impl Iterator<Item = (Vec<&'a SexpNode>, Fields<'a>)> {
    let version = source.version;
    possible_inputs(source.tree).filter_map(move |node| match node.head() {
        Some(
            "uuid" | "version" | "title_block" | "sheet_instances" | "lib_symbols" | "junction"
            | "no_connect" | "rule_area",
        ) => Some((vec![node], Fields::new())),
        Some(
            "wire" | "bus" | "bus_entry" | "symbol" | "sheet" | "label" | "global_label"
            | "hierarchical_label",
        ) => Some((
            node.children()
                .unwrap_or_default()
                .iter()
                .filter(|child| {
                    !matches!(
                        child.head(),
                        Some("property" | "effects" | "stroke" | "fields_autoplaced")
                    )
                })
                .collect(),
            local_fields(
                View {
                    item: node,
                    variant: None,
                },
                version,
            ),
        )),
        _ => None,
    })
}

fn unmodelled_export(item: &SexpNode) -> Option<&str> {
    if reference(item).is_some_and(opaque_expression) {
        return Some("Reference");
    }
    let legacy = item
        .find_all("default_instance")
        .into_iter()
        .chain(instance_records(item).map(|(_, path)| path));
    for record in legacy {
        for name in ["reference", "value", "footprint"] {
            if record_values(record, name).any(opaque_expression) {
                return Some(name);
            }
        }
    }
    if let Some((name, _, _)) =
        repeated_fields(item)
            .into_values()
            .flatten()
            .find(|(_, value, private)| {
                (!private || item.head() == Some("sheet")) && opaque_expression(value)
            })
    {
        return Some(name);
    }
    for (_, path) in instance_records(item) {
        let ambiguous = ambiguous_variants(Some(path)) || ambiguous_instance_records(item);
        for variant in path.find_all("variant") {
            if ambiguous || has_ambiguous_variant_entries(variant) {
                for field in variant.find_all("field") {
                    if record_values(field, "value").any(opaque_expression) {
                        return Some(field.find_str("name").unwrap_or("variant field"));
                    }
                }
            }
        }
    }
    None
}

pub(super) fn changes(comparison: &Comparison<'_>, reasons: &mut Vec<String>) {
    if !comparison.changed {
        return;
    }
    let [old, new] = &comparison.inputs;
    let now: BTreeMap<_, _> = new
        .items
        .iter()
        .copied()
        .filter_map(|item| Some(((item.head()?, item.find_str("uuid")?), item)))
        .collect();
    for &item in &old.items {
        let (Some(kind), Some(uuid)) = (item.head(), item.find_str("uuid")) else {
            continue;
        };
        let Some(&after) = now.get(&(kind, uuid)) else {
            continue;
        };
        let subject = if kind == "symbol" {
            format!("symbol {uuid} ({})", reference(item).unwrap_or("?"))
        } else {
            format!(
                "sheet instance {uuid} ({:?}; affects its children)",
                fields(item, old.version)["Sheetname"]
            )
        };
        let base = [
            View {
                item,
                variant: None,
            },
            View {
                item: after,
                variant: None,
            },
        ];
        comparison.exported_changes(base, &subject, reasons);
        let [was, now] = base.map(|view| instances(view.item));
        for key @ (project, path) in keys(&was, &now) {
            let was = variants(was.get(key).copied());
            let now = variants(now.get(key).copied());
            for variant in keys(&was, &now) {
                let context =
                    format!("{subject} in project {project:?}, path {path:?}, variant {variant:?}");
                comparison.exported_changes(
                    [
                        View {
                            item,
                            variant: was.get(variant).copied(),
                        },
                        View {
                            item: after,
                            variant: now.get(variant).copied(),
                        },
                    ],
                    &context,
                    reasons,
                );
            }
        }
    }
}
