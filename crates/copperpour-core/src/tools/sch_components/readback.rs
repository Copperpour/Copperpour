//! Component mutation readback, prospective verification, and observation formatting.

use super::deletion::{indexed_uuid_items, ComponentDeleteTargetError};
use super::placement::PlacementFields;
use super::properties::{placement_landed, FieldPlacement};
use crate::mcp::protocol::{CallToolResult, ToolContent};
use copperpour_schematic_editor as cse;
use copperpour_sexp::{
    commit_command, parse_sexp, prepare_command, schematic::extract_symbol_instances, ItemId,
    SchematicCommand,
};
use serde_json::json;
use std::collections::{BTreeMap, BTreeSet};

#[derive(Debug, Clone)]
pub(crate) struct ComponentTargetUnit {
    pub(super) uuid: String,
    pub(super) unit: u32,
    pub(super) fields: BTreeMap<String, String>,
    /// Properties the unit must no longer carry, so a removal that fails to
    /// reach the file cannot pass the readback.
    absent_fields: BTreeSet<String>,
    pub(super) lib_id: String,
    pub(super) x: f64,
    pub(super) y: f64,
    pub(super) rotation: f64,
    /// The placement's `(mirror ...)` axis, bound like rotation so a
    /// reflection that fails to reach the file cannot pass the readback.
    pub(super) mirror: Option<String>,
    /// The placement's native `(dnp …)` attribute, bound like the mirror so
    /// a DNP change that fails to reach the file cannot pass the readback.
    dnp: bool,
    /// The placement's native `(in_bom …)` and `(on_board …)` flags, bound
    /// like `dnp`.
    in_bom: bool,
    on_board: bool,
    instances: Vec<(String, String)>,
    /// The designators the instance records carry, sorted. On a sheet placed
    /// more than once they need not match the property, so the readback holds
    /// them to what was bound rather than to it.
    pub(super) instance_references: Vec<String>,
}

impl ComponentTargetUnit {
    /// Bind placement intent independently of the model produced by the writer.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn placement(
        uuid: &str,
        context: &crate::tools::SheetInstanceContext,
        lib_id: &str,
        x: f64,
        y: f64,
        rotation: f64,
        mirror: Option<&str>,
        reference: &str,
        fields: &PlacementFields,
        unit: u32,
    ) -> Self {
        let mut instances = context
            .instance_paths
            .iter()
            .map(|path| (context.project_name.clone(), path.clone()))
            .collect::<Vec<_>>();
        instances.sort();
        let (rotation, mirror) = cse::kicad_orientation(rotation, mirror);
        Self {
            uuid: uuid.to_owned(),
            unit,
            lib_id: lib_id.to_owned(),
            x,
            y,
            rotation,
            mirror: mirror.map(str::to_owned),
            // Placement writes `(dnp no)`: a new part is fitted.
            dnp: false,
            // And `(in_bom …)` and `(on_board …)` as the library sets them.
            in_bom: fields.in_bom,
            on_board: fields.on_board,
            fields: BTreeMap::from([
                ("Reference".to_owned(), reference.to_owned()),
                ("Value".to_owned(), fields.value.clone()),
                ("Footprint".to_owned(), fields.footprint.clone()),
            ]),
            absent_fields: BTreeSet::new(),
            instances,
            // A placement writes its one reference to every record.
            instance_references: vec![reference.to_owned()],
        }
    }

    /// Bind the set of designators the instance records carry when they differ
    /// per instance, as on a sheet placed more than once. The readback compares
    /// the sorted set, not which instance carries which.
    pub(crate) fn with_instance_references<'a>(
        mut self,
        references: impl IntoIterator<Item = &'a str>,
    ) -> Self {
        let mut references: Vec<String> = references.into_iter().map(str::to_owned).collect();
        references.sort();
        references.dedup();
        self.instance_references = references;
        self
    }
}

/// Inspect every record before sorting: duplicate identities and conflicting
/// project/unit records must not be collapsed into a plausible first answer.
pub(super) fn checked_instance_paths(
    path: &std::path::Path,
    symbol: &cse::Symbol,
) -> Result<Vec<(String, String)>, ComponentDeleteTargetError> {
    let mut identities = BTreeSet::new();
    let mut projects = BTreeSet::new();
    let references = symbol
        .instances()
        .into_iter()
        .filter_map(|instance| instance.reference)
        .collect::<Vec<_>>();
    for instance in symbol.instances() {
        let (Some(project), Some(instance_path), Some(_), Some(unit)) = (
            instance.project,
            instance.path,
            instance.reference,
            instance.unit,
        ) else {
            return Err(ComponentDeleteTargetError::stale(
                path,
                format!(
                    "component UUID {} has incomplete instance metadata",
                    symbol.uuid
                ),
            ));
        };
        if symbol
            .reference()
            .is_none_or(|own| !crate::tools::instance_references_agree(own, &references))
        {
            return Err(ComponentDeleteTargetError::stale(
                path,
                format!(
                    "component UUID {} has a conflicting instance reference: its records name {} and its Reference is {}",
                    symbol.uuid,
                    references.join(", "),
                    symbol.reference().unwrap_or("absent")
                ),
            ));
        }
        projects.insert(project.clone());
        if unit != symbol.unit || !identities.insert((project, instance_path)) || projects.len() > 1
        {
            return Err(ComponentDeleteTargetError::Ambiguous {
                target: format!("component UUID {} instance records", symbol.uuid),
                candidates: symbol
                    .instances()
                    .iter()
                    .map(|entry| format!("{entry:?}"))
                    .collect(),
            });
        }
    }
    Ok(identities.into_iter().collect())
}

fn verify_component_expectations(
    path: &std::path::Path,
    observed: &serde_json::Value,
    expected: &[ComponentTargetUnit],
) -> Result<(), CallToolResult> {
    for target in expected {
        let unit = observed["units"]
            .as_array()
            .and_then(|units| {
                units
                    .iter()
                    .find(|unit| unit["uuid"].as_str() == Some(&target.uuid))
            })
            .ok_or_else(|| {
                ComponentDeleteTargetError::stale(path, "bound unit missing from readback")
                    .into_result()
            })?;
        // Separate comparisons keep each identity/intent obligation explicit.
        for (field, matches) in [
            (
                "unit",
                unit["unit"].as_u64() == Some(u64::from(target.unit)),
            ),
            ("lib_id", unit["lib_id"].as_str() == Some(&target.lib_id)),
            (
                "x",
                unit["x"]
                    .as_f64()
                    .is_some_and(|v| (v - target.x).abs() < 1e-6),
            ),
            (
                "y",
                unit["y"]
                    .as_f64()
                    .is_some_and(|v| (v - target.y).abs() < 1e-6),
            ),
            (
                "rotation",
                unit["rotation"]
                    .as_f64()
                    .is_some_and(|v| (v - target.rotation).abs() < 1e-6),
            ),
            (
                "mirror",
                unit["mirror_x"].as_bool()
                    == Some(target.mirror.as_deref().is_some_and(|m| m.contains('x')))
                    && unit["mirror_y"].as_bool()
                        == Some(target.mirror.as_deref().is_some_and(|m| m.contains('y'))),
            ),
            ("dnp", unit["dnp"].as_bool() == Some(target.dnp)),
            ("in_bom", unit["in_bom"].as_bool() == Some(target.in_bom)),
            (
                "on_board",
                unit["on_board"].as_bool() == Some(target.on_board),
            ),
            (
                "instance_paths",
                unit["instance_paths"] == json!(target.instances),
            ),
            (
                "instance_references",
                unit["instance_references"] == json!(target.instance_references),
            ),
        ] {
            if !matches {
                return Err(ComponentDeleteTargetError::stale(
                    path,
                    format!(
                        "observed {field} differs from bound intent for UUID {}",
                        target.uuid
                    ),
                )
                .into_result());
            }
        }
        for (field, value) in &target.fields {
            if unit["fields"][field].as_str() != Some(value) {
                return Err(ComponentDeleteTargetError::stale(
                    path,
                    format!(
                        "observed property {field} differs from bound intent for UUID {}",
                        target.uuid
                    ),
                )
                .into_result());
            }
        }
        for field in &target.absent_fields {
            if unit["fields"].get(field).is_some() {
                return Err(ComponentDeleteTargetError::stale(
                    path,
                    format!(
                        "observed property {field} is still present after its removal for UUID {}",
                        target.uuid
                    ),
                )
                .into_result());
            }
        }
    }
    Ok(())
}

#[derive(Debug, Clone)]
pub(super) struct ComponentTarget {
    pub(super) reference: String,
    pub(super) units: Vec<ComponentTargetUnit>,
}

impl ComponentTarget {
    pub(super) fn with_fields(&self, fields: &BTreeMap<String, String>) -> Self {
        let mut expected = self.clone();
        if let Some(reference) = fields.get("Reference") {
            expected.reference = reference.clone();
        }
        for unit in &mut expected.units {
            unit.fields.extend(fields.clone());
            // A rename is refused unless every record carries the old one.
            if let Some(reference) = fields.get("Reference") {
                if !unit.instance_references.is_empty() {
                    unit.instance_references = vec![reference.clone()];
                }
            }
        }
        expected
    }

    pub(super) fn with_dnp(mut self, dnp: Option<bool>) -> Self {
        if let Some(dnp) = dnp {
            for unit in &mut self.units {
                unit.dnp = dnp;
            }
        }
        self
    }

    pub(super) fn with_bom_and_board(
        mut self,
        in_bom: Option<bool>,
        on_board: Option<bool>,
    ) -> Self {
        for unit in &mut self.units {
            if let Some(in_bom) = in_bom {
                unit.in_bom = in_bom;
            }
            if let Some(on_board) = on_board {
                unit.on_board = on_board;
            }
        }
        self
    }

    pub(super) fn without_fields(&self, names: &[String]) -> Self {
        let mut expected = self.clone();
        for unit in &mut expected.units {
            for name in names {
                unit.fields.remove(name);
                unit.absent_fields.insert(name.clone());
            }
        }
        expected
    }

    pub(super) fn uuids(&self) -> Vec<String> {
        self.units.iter().map(|unit| unit.uuid.clone()).collect()
    }

    pub(super) fn item_ids(&self) -> Result<Vec<ItemId>, ComponentDeleteTargetError> {
        self.units
            .iter()
            .map(|unit| {
                ItemId::new(unit.uuid.clone()).map_err(|error| ComponentDeleteTargetError::Stale {
                    target: format!("component {}", self.reference),
                    reason: error.to_string(),
                })
            })
            .collect()
    }
}

pub(super) fn component_target_from_source(
    path: &std::path::Path,
    content: &str,
    reference: &str,
) -> Result<ComponentTarget, ComponentDeleteTargetError> {
    let tree =
        parse_sexp(content).map_err(|error| ComponentDeleteTargetError::from_sexp(path, error))?;
    let indexed = indexed_uuid_items(path, content)?;
    let instances = extract_symbol_instances(&tree)
        .into_iter()
        .filter(|instance| instance.reference == reference)
        .collect::<Vec<_>>();
    if instances.is_empty() {
        return Err(ComponentDeleteTargetError::stale(
            path,
            format!("component {reference} is not present"),
        ));
    }

    let mut by_unit: BTreeMap<u32, Vec<String>> = BTreeMap::new();
    let mut units = Vec::new();
    for instance in instances {
        let uuid = instance.uuid.ok_or_else(|| {
            ComponentDeleteTargetError::stale(
                path,
                format!("component {reference} unit {} has no UUID", instance.unit),
            )
        })?;
        let Some(item) = indexed.get(&uuid) else {
            return Err(ComponentDeleteTargetError::stale(
                path,
                format!("component {reference} UUID {uuid} is not a top-level item"),
            ));
        };
        if item.kind != "symbol" {
            return Err(ComponentDeleteTargetError::stale(
                path,
                format!("component {reference} UUID {uuid} identifies {}", item.kind),
            ));
        }
        let node = parse_sexp(&item.source)
            .map_err(|error| ComponentDeleteTargetError::from_sexp(path, error))?;
        let mut fields = BTreeMap::new();
        for property in node.find_all("property") {
            // KiCad writes a private field as `(property private "NAME" "VALUE" …)`;
            // a quoted "private" is a Str and stays the field name.
            let private = matches!(
                property.get(1),
                Some(copperpour_sexp::SexpNode::Atom(token)) if token == "private"
            );
            let first = if private { 2 } else { 1 };
            let Some(name) = property.get(first).and_then(|value| value.as_str()) else {
                continue;
            };
            let value = property
                .get(first + 1)
                .and_then(|value| value.as_str())
                .unwrap_or_default();
            if fields.insert(name.to_owned(), value.to_owned()).is_some() {
                return Err(ComponentDeleteTargetError::Ambiguous {
                    target: format!("component {reference} unit {} field {name}", instance.unit),
                    candidates: vec![uuid.clone(), uuid],
                });
            }
        }
        by_unit.entry(instance.unit).or_default().push(uuid.clone());
        let symbol = cse::sexp::parser::parse(&item.source)
            .and_then(|node| cse::Symbol::from_sexp(&node))
            .map_err(|error| ComponentDeleteTargetError::stale(path, error.to_string()))?;
        let instance_paths = checked_instance_paths(path, &symbol)?;
        let mut instance_references = symbol
            .instances()
            .into_iter()
            .filter_map(|instance| instance.reference)
            .collect::<Vec<_>>();
        instance_references.sort();
        instance_references.dedup();
        units.push(ComponentTargetUnit {
            uuid,
            unit: instance.unit,
            fields,
            absent_fields: BTreeSet::new(),
            lib_id: instance.lib_id,
            x: instance.x,
            y: instance.y,
            rotation: instance.rotation,
            // Observed, not requested: rotate/move/delete do not change the
            // mirror, so binding what the file already carries makes the
            // readback assert they left it alone.
            mirror: symbol.mirror.clone(),
            dnp: symbol.dnp,
            in_bom: symbol.in_bom,
            on_board: symbol.on_board,
            instances: instance_paths,
            instance_references,
        });
    }
    if let Some((unit, uuids)) = by_unit.iter().find(|(_, uuids)| uuids.len() > 1) {
        return Err(ComponentDeleteTargetError::Ambiguous {
            target: format!("component {reference} unit {unit}"),
            candidates: uuids.clone(),
        });
    }
    if units
        .iter()
        .any(|unit| unit.instances != units[0].instances)
    {
        return Err(ComponentDeleteTargetError::Ambiguous {
            target: format!("component {reference} hierarchy instances"),
            candidates: units
                .iter()
                .map(|unit| format!("{}: {:?}", unit.uuid, unit.instances))
                .collect(),
        });
    }
    units.sort_by(|left, right| {
        left.unit
            .cmp(&right.unit)
            .then_with(|| left.uuid.cmp(&right.uuid))
    });
    Ok(ComponentTarget {
        reference: reference.to_owned(),
        units,
    })
}

pub(super) fn component_mutation_readback_from_schematic(
    path: &std::path::Path,
    committed: &cse::Schematic,
    expected_uuids: &[String],
    expected_reference: Option<&str>,
) -> Result<serde_json::Value, CallToolResult> {
    if !crate::tools::same_schematic_document(path, committed.filepath()) {
        return Err(ComponentDeleteTargetError::stale(
            path,
            "component mutation readback came from a different schematic",
        )
        .into_result());
    }
    if expected_uuids.is_empty() {
        return Err(
            ComponentDeleteTargetError::stale(path, "no component UUIDs were bound").into_result(),
        );
    }
    let expected = expected_uuids.iter().cloned().collect::<BTreeSet<_>>();
    if expected.len() != expected_uuids.len() {
        return Err(ComponentDeleteTargetError::Ambiguous {
            target: path.display().to_string(),
            candidates: expected_uuids.to_vec(),
        }
        .into_result());
    }
    let committed_source = committed.to_source();
    let indexed =
        indexed_uuid_items(path, &committed_source).map_err(|error| error.into_result())?;
    for uuid in &expected {
        if indexed.get(uuid).is_none_or(|item| item.kind != "symbol") {
            return Err(ComponentDeleteTargetError::stale(
                path,
                format!("bound UUID {uuid} no longer identifies one top-level symbol"),
            )
            .into_result());
        }
    }

    let mut observed = Vec::new();
    for uuid in &expected {
        let matches = committed
            .symbols
            .iter()
            .filter(|symbol| symbol.uuid == *uuid)
            .collect::<Vec<_>>();
        if matches.is_empty() {
            return Err(ComponentDeleteTargetError::stale(
                path,
                format!("component UUID {uuid} is absent from the observed schematic"),
            )
            .into_result());
        }
        if matches.len() > 1 {
            return Err(ComponentDeleteTargetError::Ambiguous {
                target: format!("schematic symbol UUID {uuid}"),
                candidates: matches.iter().map(|symbol| symbol.uuid.clone()).collect(),
            }
            .into_result());
        }
        observed.push(matches[0]);
    }

    let references = observed
        .iter()
        .filter_map(|symbol| symbol.reference().map(str::to_owned))
        .collect::<BTreeSet<_>>();
    if observed.iter().any(|symbol| symbol.reference().is_none()) {
        return Err(ComponentDeleteTargetError::stale(
            path,
            "a bound component UUID has no observed Reference field",
        )
        .into_result());
    }
    if references.len() != 1 {
        return Err(ComponentDeleteTargetError::Ambiguous {
            target: path.display().to_string(),
            candidates: references.iter().cloned().collect(),
        }
        .into_result());
    }
    let reference = references.iter().next().expect("one reference").clone();
    if expected_reference.is_some_and(|expected| expected != reference) {
        return Err(ComponentDeleteTargetError::stale(
            path,
            format!(
                "bound component UUIDs resolve to {reference}, not {}",
                expected_reference.unwrap_or_default()
            ),
        )
        .into_result());
    }

    observed.sort_by(|left, right| {
        left.unit
            .cmp(&right.unit)
            .then_with(|| left.uuid.cmp(&right.uuid))
    });
    let mut units = Vec::new();
    let mut component_instances = None;
    for symbol in &observed {
        let mut fields = BTreeMap::new();
        for property in &symbol.properties {
            if fields
                .insert(property.name.clone(), property.value.clone())
                .is_some()
            {
                return Err(ComponentDeleteTargetError::Ambiguous {
                    target: format!(
                        "component {reference} unit {} field {}",
                        symbol.unit, property.name
                    ),
                    candidates: vec![symbol.uuid.clone(), symbol.uuid.clone()],
                }
                .into_result());
            }
        }
        let instance_paths =
            checked_instance_paths(path, symbol).map_err(|error| error.into_result())?;
        if component_instances
            .as_ref()
            .is_some_and(|expected| expected != &instance_paths)
        {
            return Err(ComponentDeleteTargetError::Ambiguous {
                target: format!("component {reference} hierarchy instances"),
                candidates: observed
                    .iter()
                    .map(|symbol| format!("{}: {:?}", symbol.uuid, symbol.instance_paths()))
                    .collect(),
            }
            .into_result());
        }
        component_instances = Some(instance_paths.clone());
        let mut instance_references = Vec::new();
        for instances in symbol
            .raw_sub_nodes
            .iter()
            .filter(|node| node.tag() == Some("instances"))
        {
            for project in instances.find_all("project") {
                for path_node in project.find_all("path") {
                    let Some(instance_reference) = path_node.get_value("reference") else {
                        return Err(ComponentDeleteTargetError::stale(
                            path,
                            format!(
                                "component UUID {} has an instance path without a reference",
                                symbol.uuid
                            ),
                        )
                        .into_result());
                    };
                    instance_references.push(instance_reference.to_owned());
                }
            }
        }
        // `checked_instance_paths` above has held these to the property.
        instance_references.sort();
        instance_references.dedup();
        // Where each field's text actually sits in the observed schematic, so
        // a move can be checked rather than taken on trust.
        let mut field_placements = serde_json::Map::new();
        for property in &symbol.properties {
            // KiCad writes a *placement's* hidden flag inside the property's
            // `(effects …)`; the property-level token is what `lib_symbols`
            // definitions carry. Reading only direct children therefore
            // reports every field KiCad itself hid as visible — and, because
            // the writer's own check is textual and does see the nested
            // token, it also turned "hide an already-hidden field" into a
            // post-write refusal over a file that was already correct.
            // Both forms render identically in KiCad 10.0.6, and a file may
            // hold either, so accept both.
            fn hide_yes(node: &cse::sexp::SexpNode) -> bool {
                node.tag() == Some("hide") && node.value() == Some("yes")
            }
            let hidden = property.sub_nodes.iter().any(|node| {
                hide_yes(node)
                    || (node.tag() == Some("effects") && node.children().iter().any(hide_yes))
            });
            if let Some(at) = property
                .sub_nodes
                .iter()
                .filter(|node| node.tag() == Some("at"))
                .find_map(cse::At::from_sexp)
            {
                let field_rotation = at.rotation.unwrap_or(0.0);
                let symbol_rotation = symbol.at.rotation.unwrap_or(0.0);
                field_placements.insert(
                    property.name.clone(),
                    json!({
                        "x": at.x,
                        "y": at.y,
                        "rotation": field_rotation,
                        "drawn_orientation": copperpour_sexp::text_bounds::field_drawn_orientation(
                            field_rotation,
                            symbol_rotation,
                        ),
                        "hide": hidden
                    }),
                );
            }
        }
        let unit_mirror = symbol.mirror.as_deref().unwrap_or("");
        units.push(json!({
            "uuid": symbol.uuid,
            "unit": symbol.unit,
            "x": symbol.at.x,
            "y": symbol.at.y,
            "rotation": symbol.at.rotation.unwrap_or(0.0),
            "mirror_x": unit_mirror.contains('x'),
            "mirror_y": unit_mirror.contains('y'),
            "dnp": symbol.dnp,
            "in_bom": symbol.in_bom,
            "on_board": symbol.on_board,
            "lib_id": symbol.lib_id,
            "fields": fields,
            "field_placements": field_placements,
            "instance_paths": instance_paths,
            "instance_references": instance_references
        }));
    }
    let anchor = observed[0];
    let fields = units[0]["fields"].clone();
    let anchor_mirror = anchor.mirror.as_deref().unwrap_or("");
    Ok(json!({
        "schematic": committed.filepath().display().to_string(),
        "reference": reference,
        "value": anchor.value_str().unwrap_or(""),
        "footprint": anchor.footprint().unwrap_or(""),
        "datasheet": anchor.datasheet().unwrap_or(""),
        "lib_id": anchor.lib_id,
        "uuid": anchor.uuid,
        "x": anchor.at.x,
        "y": anchor.at.y,
        "rotation": anchor.at.rotation.unwrap_or(0.0),
        "mirror_x": anchor_mirror.contains('x'),
        "mirror_y": anchor_mirror.contains('y'),
        "dnp": anchor.dnp,
        "in_bom": anchor.in_bom,
        "on_board": anchor.on_board,
        "unit_count": units.len(),
        "units": units,
        "fields": fields
    }))
}

pub(super) fn load_component_mutation_readback(
    path: &std::path::Path,
    expected: &ComponentTarget,
) -> anyhow::Result<Result<serde_json::Value, CallToolResult>> {
    let committed = cse::Schematic::load(path)?;
    Ok(verified_component_readback(path, &committed, expected))
}

#[derive(Debug, Clone)]
pub(crate) struct ComponentMutationIntent {
    pub(super) target: ComponentTarget,
    field_placements: Vec<(String, FieldPlacement)>,
    placement_unit: Option<u32>,
}

impl ComponentMutationIntent {
    pub(super) fn new(target: ComponentTarget) -> Self {
        Self {
            target,
            field_placements: Vec::new(),
            placement_unit: None,
        }
    }

    /// Later batch entries may update values/DNP on the same component. Hold
    /// those to their final successful assignments while retaining placement
    /// identity and geometry bound before the field-text edit.
    pub(crate) fn with_batch_updates(
        mut self,
        fields: &BTreeMap<String, String>,
        dnp: Option<bool>,
    ) -> Self {
        self.target = self.target.with_fields(fields).with_dnp(dnp);
        self
    }

    pub(crate) fn placement_keys(&self) -> Vec<(String, String)> {
        self.target
            .units
            .iter()
            .filter(|unit| self.placement_unit.is_none_or(|wanted| unit.unit == wanted))
            .flat_map(|unit| {
                self.field_placements
                    .iter()
                    .map(move |(name, _)| (unit.uuid.clone(), name.clone()))
            })
            .collect()
    }

    pub(super) fn with_field_placements(
        mut self,
        field_placements: Vec<(String, FieldPlacement)>,
        placement_unit: Option<u32>,
    ) -> Self {
        self.field_placements = field_placements;
        self.placement_unit = placement_unit;
        self
    }
}

fn verify_requested_field_placements(
    path: &std::path::Path,
    observed: &serde_json::Value,
    intent: &ComponentMutationIntent,
    phase: &str,
) -> Result<(), CallToolResult> {
    for (name, placement) in &intent.field_placements {
        for unit in observed["units"].as_array().into_iter().flatten() {
            if intent
                .placement_unit
                .is_some_and(|wanted| unit["unit"].as_u64() != Some(u64::from(wanted)))
            {
                continue;
            }
            if !placement_landed(&unit["field_placements"][name], placement) {
                return Err(ComponentDeleteTargetError::stale(
                    path,
                    format!(
                        "{phase} placement of '{name}' on unit {} differs from what was requested",
                        unit["unit"]
                    ),
                )
                .into_result());
            }
        }
    }
    Ok(())
}

pub(crate) fn verify_component_mutation_intents(
    path: &std::path::Path,
    schematic: &cse::Schematic,
    intents: &[ComponentMutationIntent],
    phase: &str,
) -> Result<Vec<serde_json::Value>, CallToolResult> {
    let mut observations = Vec::with_capacity(intents.len());
    for intent in intents {
        let observed = verified_component_readback(path, schematic, &intent.target)?;
        verify_requested_field_placements(path, &observed, intent, phase)?;
        observations.push(observed);
    }
    Ok(observations)
}

pub(super) fn tool_error_summary(error: &CallToolResult) -> String {
    error
        .content
        .iter()
        .find_map(|content| match content {
            ToolContent::Text { text } => Some(
                serde_json::from_str::<serde_json::Value>(text)
                    .ok()
                    .and_then(|body| body["message"].as_str().map(str::to_owned))
                    .unwrap_or_else(|| text.clone()),
            ),
            ToolContent::Image { .. } => None,
        })
        .unwrap_or_else(|| "readback returned no diagnostic text".to_owned())
}

#[cfg(test)]
thread_local! {
    /// One-shot corruption of a prospective command result. This lets tests
    /// drive the public handlers through the exact writer-mismatch failure
    /// that #499 is about without altering the file on disk first.
    pub(super) static COMPONENT_PROSPECTIVE_FAULT: std::cell::RefCell<Option<(String, String)>> =
        const { std::cell::RefCell::new(None) };
    /// One-shot failure at the saved-file readback boundary. The mutation has
    /// already committed when this is consumed.
    pub(super) static COMPONENT_COMMITTED_LOAD_FAULT: std::cell::RefCell<Option<String>> =
        const { std::cell::RefCell::new(None) };
}

/// One-shot external save that appends the text to a sheet after a placement
/// wrote it and before the readback snapshot, as an editor autosave would.
#[cfg(test)]
pub(crate) static COMPONENT_SAVE_BEFORE_SNAPSHOT: std::sync::Mutex<
    Vec<(std::path::PathBuf, String)>,
> = std::sync::Mutex::new(Vec::new());

pub(crate) fn load_committed_component_snapshot(
    path: &std::path::Path,
) -> anyhow::Result<(String, cse::Schematic)> {
    check_committed_load()?;
    #[cfg(test)]
    {
        let mut saves = COMPONENT_SAVE_BEFORE_SNAPSHOT.lock().unwrap();
        let same = |armed: &std::path::Path| {
            std::fs::canonicalize(armed).ok() == std::fs::canonicalize(path).ok()
        };
        if let Some(index) = saves.iter().position(|(armed, _)| same(armed)) {
            let saved = std::fs::read_to_string(path)? + &saves.remove(index).1;
            std::fs::write(path, saved)?;
        }
    }
    let source = copperpour_sexp::read_consistent(path)?;
    let schematic = cse::Schematic::from_source(path, source.clone())?;
    Ok((source, schematic))
}

fn check_committed_load() -> anyhow::Result<()> {
    #[cfg(test)]
    if let Some(reason) = COMPONENT_COMMITTED_LOAD_FAULT.with(|fault| fault.borrow_mut().take()) {
        anyhow::bail!(reason);
    }
    Ok(())
}

#[cfg(test)]
fn apply_component_prospective_fault(source: String) -> String {
    COMPONENT_PROSPECTIVE_FAULT.with(|fault| {
        let Some((from, to)) = fault.borrow_mut().take() else {
            return source;
        };
        assert!(
            source.contains(&from),
            "prospective fault anchor was not present: {from}"
        );
        source.replacen(&from, &to, 1)
    })
}

/// Validate the exact prospective command result before committing it, then
/// independently verify the saved file. A prospective mismatch is a true
/// no-op; a post-commit mismatch is reported as an uncertain mutation outcome.
pub(super) fn commit_verified_component_mutation(
    path: &std::path::Path,
    before: &str,
    command: &SchematicCommand,
    intents: &[ComponentMutationIntent],
    operation: &str,
) -> anyhow::Result<Result<Vec<serde_json::Value>, CallToolResult>> {
    let (prospective_source, _) = prepare_command(path, before, command)?;
    #[cfg(test)]
    let prospective_source = apply_component_prospective_fault(prospective_source);
    let prospective = match cse::Schematic::from_source(path, prospective_source) {
        Ok(schematic) => schematic,
        Err(error) => {
            return Ok(Err(ComponentDeleteTargetError::stale(
                path,
                format!("prospective schematic is not readable: {error}"),
            )
            .into_result()))
        }
    };
    if let Err(error) =
        verify_component_mutation_intents(path, &prospective, intents, "prospective")
    {
        return Ok(Err(error));
    }
    commit_command(path, command)?;

    let committed = match cse::Schematic::load(path) {
        Ok(schematic) => schematic,
        Err(error) => {
            return Ok(Err(crate::tools::mutation_outcome_uncertain(
                path,
                operation,
                format!("The committed schematic could not be loaded: {error}"),
            )))
        }
    };
    match verify_component_mutation_intents(path, &committed, intents, "post-commit") {
        Ok(observations) => Ok(Ok(observations)),
        Err(error) => Ok(Err(crate::tools::mutation_outcome_uncertain(
            path,
            operation,
            tool_error_summary(&error),
        ))),
    }
}

pub(super) fn verified_component_readback(
    path: &std::path::Path,
    committed: &cse::Schematic,
    expected: &ComponentTarget,
) -> Result<serde_json::Value, CallToolResult> {
    let observed = component_mutation_readback_from_schematic(
        path,
        committed,
        &expected.uuids(),
        Some(&expected.reference),
    )?;
    verify_component_expectations(path, &observed, &expected.units)?;
    Ok(observed)
}

pub(super) fn copy_component_observation(
    result: &mut serde_json::Value,
    observed: &serde_json::Value,
) {
    for key in [
        "schematic",
        "reference",
        "value",
        "footprint",
        "datasheet",
        "lib_id",
        "uuid",
        "x",
        "y",
        "rotation",
        "mirror_x",
        "mirror_y",
        "dnp",
        "in_bom",
        "on_board",
        "unit_count",
        "units",
        "fields",
    ] {
        result[key] = observed[key].clone();
    }
}

/// Build a placement response only from the committed schematic that was read
/// back after the write. The UUID is the mutation's stable identity; requested
/// coordinates, fields, and hierarchy paths are never echoed as proof.
/// Whether the caller asked for the full placement readback (`verbose`).
pub(crate) fn wants_verbose(args: &serde_json::Value) -> bool {
    args["verbose"].as_bool().unwrap_or(false)
}

/// The compact entry for a placed part: what a caller needs to go on
/// (#114). The full readback repeated every field twice, `lib_id` three
/// times and each field's placement; `verbose: true` still returns it, and
/// the readback is verified the same either way.
pub(crate) fn compact_placement(full: &serde_json::Value) -> serde_json::Value {
    let mut entry = serde_json::Map::new();
    for key in [
        "reference",
        "uuid",
        "lib_id",
        "value",
        "footprint",
        "x",
        "y",
        "rotation",
        "unit",
    ] {
        if let Some(value) = full.get(key) {
            entry.insert(key.to_string(), value.clone());
        }
    }
    // Only when they say something.
    for key in ["mirror_x", "mirror_y", "dnp"] {
        if full[key] == json!(true) {
            entry.insert(key.to_string(), json!(true));
        }
    }
    if full["unit_count"].as_u64().is_some_and(|count| count > 1) {
        entry.insert("unit_count".to_string(), full["unit_count"].clone());
    }
    for key in ["in_bom", "on_board"] {
        if full[key] == json!(false) {
            entry.insert(key.to_string(), json!(false));
        }
    }
    serde_json::Value::Object(entry)
}

pub(crate) fn placed_component_readback(
    sch_path: &std::path::Path,
    committed: &cse::Schematic,
    expected: &ComponentTargetUnit,
    context: &crate::tools::SheetInstanceContext,
) -> Result<serde_json::Value, CallToolResult> {
    if !crate::tools::same_schematic_document(sch_path, committed.filepath()) {
        return Err(crate::tools::SchematicTargetError::StaleTarget {
            target: sch_path.to_path_buf(),
            reason: "placement readback came from a different schematic".to_string(),
        }
        .into_tool_result());
    }
    let uuid = &expected.uuid;
    let mut result = component_mutation_readback_from_schematic(
        sch_path,
        committed,
        &[uuid.to_owned()],
        expected.fields.get("Reference").map(String::as_str),
    )?;
    verify_component_expectations(sch_path, &result, std::slice::from_ref(expected))?;
    if let Err(error) = crate::tools::validate_sheet_instance_state(sch_path, committed, context) {
        return Err(error.into_tool_result());
    }
    let symbol = committed
        .symbols
        .iter()
        .find(|symbol| symbol.uuid == *uuid)
        .expect("shared readback proved this UUID");
    let mut observed_instances = symbol.instance_paths();
    observed_instances.sort();
    let Some((project, _)) = observed_instances.first() else {
        return Err(ComponentDeleteTargetError::stale(
            committed.filepath(),
            format!("placed symbol UUID '{uuid}' has no project in post-write readback"),
        )
        .into_result());
    };
    let project = project.clone();
    let mut instance_paths = observed_instances
        .into_iter()
        .map(|(_, path)| path)
        .collect::<Vec<_>>();
    instance_paths.sort();

    result["added"] = result["lib_id"].clone();
    result["unit"] = json!(symbol.unit);
    result["project"] = json!(project);
    result["instance_paths"] = json!(instance_paths);
    Ok(result)
}
