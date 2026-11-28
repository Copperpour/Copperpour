//! Rule-area effects for the saved layout comparator.

use super::*;
use copperpour_sexp::schematic_rule_area::{
    area_library, find_unique_lib_symbol, sheet_box, symbol_box, RuleArea,
};

const CLASS: &str = "Component Class";

struct Area<'a> {
    geometry: RuleArea<'a>,
    classes: Vec<Directive<'a>>,
}

#[derive(PartialEq)]
struct Directive<'a> {
    name: &'a str,
    attached: Option<bool>,
    uncertain_anchor: Option<&'a SexpNode>,
    // A literal class does not depend on other directive fields.
    expression_owner: Option<&'a SexpNode>,
}

struct Inputs<'a> {
    areas: Vec<Area<'a>>,
    library: Vec<&'a SexpNode>,
}

impl<'a> Inputs<'a> {
    fn read(source: &Source<'a>) -> Self {
        let tree = source.tree;
        let directives: Vec<_> = ["directive_label", "netclass_flag"]
            .into_iter()
            .flat_map(|tag| tree.find_all(tag))
            .flat_map(|directive| {
                directive
                    .find_all("property")
                    .into_iter()
                    .filter_map(property)
                    .filter_map(move |(name, value, _)| {
                        let value = loaded_field_text(value, source.version);
                        (name == CLASS && !value.is_empty()).then_some((directive, value))
                    })
            })
            .collect();
        let areas = tree
            .find_all("rule_area")
            .into_iter()
            .map(|node| {
                let geometry = RuleArea::from_node(node);
                let classes = directives
                    .iter()
                    .filter_map(|&(directive, name)| {
                        let attached = geometry.attaches(directive);
                        (attached != Some(false)).then_some(Directive {
                            name,
                            attached,
                            uncertain_anchor: attached.is_none().then_some(directive),
                            expression_owner: opaque_expression(name).then_some(directive),
                        })
                    })
                    .collect();
                Area { geometry, classes }
            })
            .collect();
        let library = area_library(tree);
        Self { areas, library }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Effect<'a> {
    Known(&'a str),
    Unknown(&'a str),
}

#[derive(Default)]
struct Effects<'a> {
    flags: BTreeMap<&'static str, Effect<'a>>,
    classes: BTreeMap<&'a str, Effect<'a>>,
}

fn add_effect<'a, K: Ord>(
    effects: &mut BTreeMap<K, Effect<'a>>,
    key: K,
    source: &'a str,
    applies: Option<bool>,
) {
    match applies {
        Some(true) => {
            effects.insert(key, Effect::Known(source));
        }
        None => {
            effects.entry(key).or_insert(Effect::Unknown(source));
        }
        Some(false) => {}
    }
}

fn effects<'a>(
    inputs: &'a Inputs<'a>,
    item: &'a SexpNode,
    fields: &Fields<'a>,
    geometry_fields: &Fields<'a>,
    reference: Option<&str>,
) -> Effects<'a> {
    let mut out = Effects::default();
    let sheet = item.head() == Some("sheet");
    let library = find_unique_lib_symbol(&inputs.library, item);
    if library.is_some_and(|lib| lib.find("power").is_some()) {
        return out;
    }
    if !sheet {
        let mut records =
            ordered_fields(item).filter(|(key, _, _)| key.eq_ignore_ascii_case(CLASS));
        let first = records.next();
        let repeated = records.next().is_some();
        if let Some(name) = fields
            .get(CLASS)
            .copied()
            .filter(|name| !name.is_empty())
            .or_else(|| repeated.then_some(first?.1))
        {
            add_effect(
                &mut out.classes,
                name,
                "direct field",
                (!repeated && !opaque_expression(name)).then_some(true),
            );
        }
    }
    let mut bounds = None;
    for area in &inputs.areas {
        let source = area.geometry.uuid.unwrap_or("(unknown UUID)");
        let mut intersects = || {
            if !sheet
                && (library.is_none()
                    || has_legacy_instance_defaults(item)
                    || saved_instance_unit_differs(item)
                    || library.is_some_and(has_named_units))
            {
                // The box cannot account for a superseded value, a different
                // loaded unit or KiCad's custom Reference suffix.
                return None;
            }
            bounds
                .get_or_insert_with(|| {
                    if sheet {
                        sheet_box(item)
                    } else {
                        symbol_box(item, &inputs.library, reference, |name| {
                            geometry_fields.get(name).copied()
                        })
                    }
                })
                .classify(&area.geometry)
        };
        if !sheet {
            for (name, default) in FLAGS {
                if record_values(area.geometry.node, name)
                    .last()
                    .is_some_and(|v| (v == "yes") != (default == "yes"))
                {
                    add_effect(&mut out.flags, name, source, intersects());
                }
            }
        }
        for &Directive { name, attached, .. } in &area.classes {
            let applies = match intersects() {
                Some(false) => Some(false),
                Some(true) if attached == Some(true) && !opaque_expression(name) => Some(true),
                _ => None,
            };
            add_effect(&mut out.classes, name, source, applies);
        }
    }
    out
}

fn saved_instance_unit_differs(item: &SexpNode) -> bool {
    let unit = |value: Option<&str>| value.and_then(|text| text.parse::<u32>().ok()).unwrap_or(1);
    instance_records(item).any(|(_, path)| {
        record_values(path, "unit").any(|saved| unit(Some(saved)) != unit(item.find_str("unit")))
    })
}

fn has_named_units(library: &SexpNode) -> bool {
    std::iter::once(library)
        .chain(library.find_all("symbol"))
        .any(|unit| unit.find("unit_name").is_some())
}

fn effective_flag(active: bool, effect: Option<&Effect<'_>>, same_inputs: bool) -> Option<bool> {
    match (active, effect) {
        (true, _) | (_, Some(Effect::Known(_))) => Some(true),
        (_, Some(Effect::Unknown(_))) if !same_inputs => None,
        _ => Some(false),
    }
}

fn local_fields<'a>(item: &'a SexpNode, source: &Source<'_>) -> Fields<'a> {
    let mut values: Fields<'_> = item
        .find_all("property")
        .into_iter()
        .filter_map(property)
        .filter_map(|(name, value, private)| {
            private.then_some((name, loaded_field_text(value, source.version)))
        })
        .collect();
    values.extend(fields(item, source.version));
    values
}

fn with_variant<'a>(fields: &Fields<'a>, variant: Option<&'a SexpNode>) -> Fields<'a> {
    let mut fields = fields.clone();
    fields.extend(variant_fields(variant));
    fields
}

fn own_flag(
    item: &SexpNode,
    version: u32,
    variant: Option<&SexpNode>,
    name: &str,
    default: bool,
) -> bool {
    if name == "exclude_from_sim"
        && ordered_fields(item).any(|(field, _, _)| legacy_sim_property(field).is_some())
    {
        // The legacy fields are setters, not export fields. Do not use the
        // direct flag to mask an area change when its loaded value is unproved.
        return default;
    }
    // KiCad's parser applies each setter in order. Do not let a superseded
    // local flag hide a change to an effective area flag.
    variant
        .and_then(|record| record_values(record, name).last())
        .map_or_else(
            || {
                if variant.is_some() {
                    inherited_flag(item, name, default)
                } else {
                    record_values(item, name)
                        .last()
                        .map_or(default, |value| value == "yes")
                }
            },
            |value| loaded_flag_value("variant", name, version, value),
        )
}

type View<'a> = (&'a SexpNode, Option<&'a SexpNode>, Option<&'a SexpNode>);

fn view_reference<'a>(
    item: &'a SexpNode,
    tree: &SexpNode,
    path: Option<&'a SexpNode>,
) -> Option<&'a str> {
    match path {
        Some(record) => record.find_str("reference"),
        None => saved_root_reference(item, tree),
    }
}

fn competing_variant_names(path: Option<&SexpNode>) -> bool {
    path.into_iter()
        .flat_map(|path| path.find_all("variant"))
        .any(|variant| variant.find_all("name").len() > 1)
}

fn compare_view<'a>(
    (old, old_inputs, (before, was_path, was)): (&Source<'a>, &Inputs<'a>, View<'a>),
    (new, new_inputs, (after, now_path, now)): (&Source<'a>, &Inputs<'a>, View<'a>),
    same_areas: bool,
    who: &str,
    reasons: &mut Vec<String>,
    expressions: &expressions::Comparison<'a>,
) {
    // The headless loader populates area membership before selecting the
    // export variant. Use the loaded field geometry, then apply variant flags.
    let was_geometry = local_fields(before, old);
    let now_geometry = local_fields(after, new);
    let was_fields = with_variant(&was_geometry, was);
    let now_fields = with_variant(&now_geometry, now);
    let was_effects = effects(
        old_inputs,
        before,
        &was_fields,
        &was_geometry,
        view_reference(before, old.tree, was_path),
    );
    let now_effects = effects(
        new_inputs,
        after,
        &now_fields,
        &now_geometry,
        view_reference(after, new.tree, now_path),
    );
    let same_placement_inputs = same_areas
        && before == after
        && was_path == now_path
        && old.version == new.version
        && was_fields == now_fields;
    let expression_changed = |text| expressions.affects([before, after], [None; 2], text);
    // When text prevents a bound, an unchanged placement record is not
    // proof of unchanged reachability: a different symbol can supply text.
    let same_inputs = same_placement_inputs
        && (!was_effects
            .flags
            .values()
            .chain(now_effects.flags.values())
            .chain(was_effects.classes.values())
            .chain(now_effects.classes.values())
            .any(|effect| matches!(effect, Effect::Unknown(source) if *source != "direct field"))
            || !ordered_fields(before).any(|(_, value, _)| expression_changed(value)));
    let emit = |reasons: &mut Vec<String>, detail: String| {
        let reason = format!("{who} {detail}");
        if !reasons.contains(&reason) {
            reasons.push(reason);
        }
    };
    for (name, default) in FLAGS {
        let default = default == "yes";
        if before.head() == Some("sheet") && name == "in_pos_files" {
            continue;
        }
        let old_area = was_effects.flags.get(name);
        let new_area = now_effects.flags.get(name);
        let was_flag = effective_flag(
            own_flag(before, old.version, was, name, default) != default,
            old_area,
            same_inputs,
        );
        let now_flag = effective_flag(
            own_flag(after, new.version, now, name, default) != default,
            new_area,
            same_inputs,
        );
        if was_flag.is_none() || now_flag.is_none() || was_flag != now_flag {
            if let Some(source) = old_area.or(new_area) {
                let detail = match source {
                    Effect::Unknown(uuid) => {
                        format!("has unknown effective attribute {name} (rule area {uuid})")
                    }
                    Effect::Known(uuid) => format!("changed attribute {name} (rule area {uuid})"),
                };
                emit(reasons, detail);
            }
        }
    }
    let mut unknown = None;
    let mut sources = BTreeSet::new();
    for name in keys(&was_effects.classes, &now_effects.classes) {
        let (was, now) = (was_effects.classes.get(name), now_effects.classes.get(name));
        for effect in [was, now].into_iter().flatten() {
            match effect {
                Effect::Known(source) if was.is_some() != now.is_some() => {
                    sources.insert(*source);
                }
                Effect::Unknown(source) if !same_inputs => {
                    unknown = Some(*source);
                }
                _ => {}
            }
        }
    }
    for name in was_effects
        .classes
        .keys()
        .copied()
        .filter(|name| opaque_expression(name))
    {
        if !now_effects.classes.contains_key(name) {
            unknown = Some("saved class text");
        } else if was_effects.classes[name] == Effect::Unknown("direct field")
            && expressions.affects([before, after], [was, now], name)
        {
            unknown = Some("direct field");
        }
    }
    for area in &old_inputs.areas {
        for directive in &area.classes {
            let Some(owner) = directive.expression_owner else {
                continue;
            };
            if was_effects.classes.contains_key(directive.name)
                && expressions.affects([owner, owner], [None; 2], directive.name)
            {
                emit(reasons, "cannot establish unchanged Component Class or rule-area effects: directive expression inputs changed".into());
            }
        }
    }
    if let Some(source) = unknown {
        emit(
            reasons,
            format!("has unknown effective Component Class (source {source})"),
        );
    } else if !sources.is_empty() {
        let before: Vec<_> = was_effects
            .classes
            .iter()
            .filter_map(|(name, effect)| matches!(effect, Effect::Known(_)).then_some(name))
            .collect();
        let after: Vec<_> = now_effects
            .classes
            .iter()
            .filter_map(|(name, effect)| matches!(effect, Effect::Known(_)).then_some(name))
            .collect();
        emit(
            reasons,
            format!(
                "changed Component Class from {before:?} to {after:?} (sources: {})",
                sources.into_iter().collect::<Vec<_>>().join(", ")
            ),
        );
    }
}

pub(super) fn changes<'a>(
    old: &Source<'a>,
    new: &Source<'a>,
    expressions: &expressions::Comparison<'a>,
    reasons: &mut Vec<String>,
) {
    let (was, now) = (Inputs::read(old), Inputs::read(new));
    if let Some(area) = area_netclass_input(old, &was).or_else(|| area_netclass_input(new, &now)) {
        reasons.push(format!(
            "cannot establish unchanged Netclass assignments (rule area {area})"
        ));
    }
    let same_areas = was.library == now.library
        && was
            .areas
            .iter()
            .map(|area| (area.geometry.node, &area.classes))
            .eq(now
                .areas
                .iter()
                .map(|area| (area.geometry.node, &area.classes)));
    let ordered: BTreeMap<_, _> = placements(new.tree)
        .filter_map(|item| Some(((item.head()?, item.find_str("uuid")?), item)))
        .collect();
    for before in placements(old.tree) {
        let (Some(kind), Some(uuid)) = (before.head(), before.find_str("uuid")) else {
            continue;
        };
        let Some(&after) = ordered.get(&(kind, uuid)) else {
            continue;
        };
        let who = subject(before, old, uuid);
        compare_view(
            (old, &was, (before, None, None)),
            (new, &now, (after, None, None)),
            same_areas,
            &who,
            reasons,
            expressions,
        );
        let (was_paths, now_paths) = (instances(before), instances(after));
        for key @ (project, path) in keys(&was_paths, &now_paths) {
            let (p, q) = (was_paths.get(key).copied(), now_paths.get(key).copied());
            let context = format!("{who} in project {project:?}, path {path:?}");
            compare_view(
                (old, &was, (before, p, None)),
                (new, &now, (after, q, None)),
                same_areas,
                &context,
                reasons,
                expressions,
            );
            if (!was.areas.is_empty() || !now.areas.is_empty())
                && (!same_areas || before != after)
                && (competing_variant_names(p) || competing_variant_names(q))
            {
                reasons.push(format!(
                    "{context} has unknown effective rule-area effects (competing saved variant names)"
                ));
                continue;
            }
            let (was_variants, now_variants) = (variants(p), variants(q));
            for name in keys(&was_variants, &now_variants) {
                compare_view(
                    (old, &was, (before, p, was_variants.get(name).copied())),
                    (new, &now, (after, q, now_variants.get(name).copied())),
                    same_areas,
                    &format!("{context}, variant {name:?}"),
                    reasons,
                    expressions,
                );
            }
        }
    }
}

pub(super) fn same_sheet_flag(token: &str, pair: [(&SexpNode, &SexpNode); 2]) -> bool {
    let (name, default) = match token {
        "DNP" => ("dnp", false),
        "EXCLUDE_FROM_BOM" => ("in_bom", true),
        "EXCLUDE_FROM_BOARD" => ("on_board", true),
        _ => ("exclude_from_sim", false),
    };
    let [(before, old), (after, new)] = pair;
    let applicable = |(sheet, tree): (&SexpNode, &SexpNode)| {
        let bounds = sheet_box(sheet);
        tree.find_all("rule_area")
            .into_iter()
            .filter(|area| {
                record_values(area, name)
                    .last()
                    .is_some_and(|value| (value == "yes") != default)
            })
            .map(|area| bounds.classify(&RuleArea::from_node(area)))
            .collect::<Vec<_>>()
    };
    let [was, now] = pair.map(applicable);
    record_values(before, name).eq(record_values(after, name))
        && (was == now && !was.contains(&None)
            || (before == after
                && !ordered_fields(before).any(|(_, value, _)| opaque_expression(value))
                && old.find_all("rule_area") == new.find_all("rule_area")))
}

fn area_netclass_input<'a>(source: &Source<'a>, inputs: &Inputs<'a>) -> Option<&'a str> {
    let directives: Vec<_> = ["directive_label", "netclass_flag"]
        .into_iter()
        .flat_map(|tag| source.tree.find_all(tag))
        .filter(|directive| {
            // KiCad accepts translated Netclass field names. Avoid guessing
            // which nonempty directive field is an assignment.
            ordered_fields(directive).any(|(name, value, _)| {
                name != CLASS && !loaded_field_text(value, source.version).is_empty()
            })
        })
        .collect();
    inputs.areas.iter().find_map(|area| {
        directives
            .iter()
            .any(|directive| area.geometry.attaches(directive) != Some(false))
            .then_some(area.geometry.uuid.unwrap_or("(unknown UUID)"))
    })
}
