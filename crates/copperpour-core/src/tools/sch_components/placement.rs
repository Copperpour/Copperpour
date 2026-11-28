//! Component placement and library symbol resolution/embedding.

use super::readback::{
    compact_placement, load_committed_component_snapshot, placed_component_readback,
    tool_error_summary, wants_verbose, ComponentTargetUnit,
};
use crate::mcp::protocol::CallToolResult;
use crate::tools::{
    get_path, opt_bool_flag, opt_str, opt_u32, require_f64, require_str, ToolContext,
};
use copperpour_schematic_editor as cse;
use copperpour_sexp::geometry::snap_point;
use serde_json::json;

pub(crate) async fn handle_add_schematic_component(
    args: &serde_json::Value,
    _ctx: &ToolContext,
) -> anyhow::Result<CallToolResult> {
    let sch_path = get_path(args, "schematic")?;
    let autoplace = match opt_bool_flag(args, "autoplace_fields") {
        Ok(value) => value == Some(true),
        Err(error) => return Ok(failed_component_placement(error, &sch_path)),
    };
    let lib_id = match require_str(args, "lib_id") {
        Ok(s) => s.to_string(),
        Err(error) => return Ok(failed_component_placement(error, &sch_path)),
    };
    let x = match require_f64(args, "x") {
        Ok(v) => v,
        Err(error) => return Ok(failed_component_placement(error, &sch_path)),
    };
    let y = match require_f64(args, "y") {
        Ok(v) => v,
        Err(error) => return Ok(failed_component_placement(error, &sch_path)),
    };
    let mirror = match mirror_arg(args, "mirror") {
        Ok(mirror) => mirror,
        Err(error) => return Ok(failed_component_placement(error, &sch_path)),
    };
    let reference = opt_str(args, "reference");
    let value = opt_str(args, "value");
    let footprint = opt_str(args, "footprint");
    let unit = match opt_u32(args, "unit") {
        Ok(unit) => unit.unwrap_or(1),
        Err(error) => return Ok(failed_component_placement(error, &sch_path)),
    };
    let before = copperpour_sexp::read_consistent(&sch_path)?;
    let mut sch = cse::Schematic::from_source(&sch_path, before.clone())?;

    // KiCAD's netlister resolves instances against the ROOT sheet's uuid and
    // the project's name, and silently forms no wire-only nets for symbols
    // whose path doesn't resolve. On a child sheet both differ from this
    // file's own stem and uuid, which is what left hierarchical designs
    // unannotated (#204).
    let context = match crate::tools::sheet_instance_context(&sch_path, &mut sch) {
        Ok(context) => context,
        Err(error) => {
            return Ok(failed_component_placement(
                error.into_tool_result(),
                &sch_path,
            ))
        }
    };
    if let Err(error) = crate::tools::validate_sheet_instance_state(&sch_path, &sch, &context) {
        return Ok(failed_component_placement(
            error.into_tool_result(),
            &sch_path,
        ));
    }
    let source = match crate::tools::library::KiCadSymbolSource::for_file(&sch_path) {
        Ok(source) => source,
        Err(error) => {
            return Ok(failed_component_placement(
                error.into_tool_result(),
                &sch_path,
            ))
        }
    };

    let lcsc_id = opt_str(args, "lcsc_id");
    let fields = args["fields"].as_object();
    let catalog = crate::sourcing::LcscCatalog::resolve(args, _ctx);
    let extras = ComponentPlacementExtras {
        lcsc_id,
        fields,
        catalog: Some(&catalog),
    };
    let mut in_use = crate::tools::sch_annotate::DesignatorsInUse::new(
        &sch_path,
        &context.project_name,
        "reference",
    );

    let placed = match place_one_component(
        &mut sch,
        &context.instance_paths,
        &context.project_name,
        &lib_id,
        x,
        y,
        args.get("rotation"),
        mirror,
        reference,
        value,
        footprint,
        unit,
        &source,
        extras,
        &mut in_use,
    ) {
        Ok(placed) => placed,
        Err(error) => return Ok(failed_component_placement(error, &sch_path)),
    };

    let (expected_x, expected_y) = snap_point(x, y, 1.27);
    let placement = ComponentTargetUnit::placement(
        &placed.uuid,
        &context,
        &lib_id,
        expected_x,
        expected_y,
        placed.requested_rotation,
        mirror,
        &placed.reference,
        &placed.fields,
        unit,
    );
    let mut planned_junctions = None;
    let mut field_readback = None;
    let save = if autoplace {
        use copperpour_sexp::{parse_sexp, schematic::extract_junctions};
        let (candidate, added, _) =
            crate::tools::sch_wiring::reconcile_junctions_for_placement(&before, sch.to_source());
        planned_junctions = Some(if added == 0 {
            Vec::new()
        } else {
            let existing = extract_junctions(&parse_sexp(&before)?);
            extract_junctions(&parse_sexp(&candidate)?)
                .into_iter()
                .filter(|point| !existing.contains(point))
                .collect()
        });
        let (candidate, fields) = crate::tools::sch_field_autoplace::autoplace_authoring(
            &before,
            candidate,
            std::slice::from_ref(&placed.uuid),
        );
        let saved = copperpour_sexp::write_atomic_if_unchanged(&sch_path, &before, &candidate)
            .map_err(anyhow::Error::from);
        field_readback = Some((fields, candidate));
        saved
    } else {
        sch.overwrite().map_err(anyhow::Error::from)
    };
    if let Err(error) = save {
        if let Some(refusal) =
            crate::tools::unwritten_schematic_refusal("add_schematic_component", error.as_ref())
        {
            return Ok(crate::outcome::refused_before_write(
                refusal,
                sch_path.display().to_string(),
                1,
            ));
        }
        let uncertain = crate::tools::mutation_outcome_uncertain(
            &sch_path,
            "add_schematic_component",
            format!("schematic persistence failed: {error}"),
        );
        return Ok(crate::outcome::attach(
            uncertain,
            crate::outcome::summary(
                crate::outcome::OutcomeStatus::Uncertain,
                sch_path.display().to_string(),
                "saved_file_readback",
                1,
                0,
                1,
                Some(crate::outcome::inspect_before_retry()),
            ),
        ));
    }

    // Keep the legacy post-write pass unless the opt-in candidate already
    // reconciled and committed its junctions with the placement.
    let junctions = match planned_junctions
        .map(Ok)
        .unwrap_or_else(|| crate::tools::add_pin_midwire_junctions(&sch_path, &placed.reference))
    {
        Ok(junctions) => junctions,
        Err(error) => {
            let uncertain = crate::tools::mutation_outcome_uncertain(
                &sch_path,
                "add_schematic_component",
                format!("post-write junction processing failed: {error}"),
            );
            return Ok(crate::outcome::attach(
                uncertain,
                crate::outcome::summary(
                    crate::outcome::OutcomeStatus::Uncertain,
                    sch_path.display().to_string(),
                    "saved_file_readback",
                    1,
                    0,
                    1,
                    Some(crate::outcome::inspect_before_retry()),
                ),
            ));
        }
    };
    let (saved, committed) = match load_committed_component_snapshot(&sch_path) {
        Ok(committed) => committed,
        Err(error) => {
            let uncertain = crate::tools::mutation_outcome_uncertain(
                &sch_path,
                "add_schematic_component",
                format!("saved schematic could not be reloaded: {error}"),
            );
            return Ok(crate::outcome::attach(
                uncertain,
                crate::outcome::summary(
                    crate::outcome::OutcomeStatus::Uncertain,
                    sch_path.display().to_string(),
                    "saved_file_readback",
                    1,
                    0,
                    1,
                    Some(crate::outcome::inspect_before_retry()),
                ),
            ));
        }
    };
    let mut result = match placed_component_readback(&sch_path, &committed, &placement, &context) {
        Ok(result) => result,
        Err(error) => {
            let uncertain = crate::tools::mutation_outcome_uncertain(
                &sch_path,
                "add_schematic_component",
                format!(
                    "saved placement readback failed: {}",
                    tool_error_summary(&error)
                ),
            );
            return Ok(crate::outcome::attach(
                uncertain,
                crate::outcome::summary(
                    crate::outcome::OutcomeStatus::Uncertain,
                    sch_path.display().to_string(),
                    "saved_file_readback",
                    1,
                    0,
                    1,
                    Some(crate::outcome::inspect_before_retry()),
                ),
            ));
        }
    };
    if !wants_verbose(args) {
        let mut compact = compact_placement(&result);
        compact["schematic"] = result["schematic"].clone();
        result = compact;
    }
    result["junctions_added"] = json!(junctions
        .iter()
        .map(|(x, y)| json!({ "x": x, "y": y }))
        .collect::<Vec<_>>());
    if let Some(report) = &placed.lcsc_catalog {
        result["lcsc_catalog"] = report.clone();
    }
    in_use.disclose(&mut result);
    if let Some((fields, candidate)) = field_readback {
        match fields.observe(&saved, &sch_path, &candidate, "add_schematic_component") {
            Ok(fields) => result["autoplaced_fields"] = fields,
            // The symbol itself was read back; report it with the uncertainty.
            Err(error) => {
                return Ok(crate::outcome::attach(
                    crate::outcome::extend(error, json!({ "placed": result })),
                    crate::outcome::summary(
                        crate::outcome::OutcomeStatus::Uncertain,
                        sch_path.display().to_string(),
                        "saved_file_readback",
                        1,
                        1,
                        0,
                        Some(crate::outcome::inspect_before_retry()),
                    ),
                ))
            }
        }
    }

    Ok(crate::outcome::attach(
        CallToolResult::json(&result),
        crate::outcome::summary(
            crate::outcome::OutcomeStatus::Complete,
            sch_path.display().to_string(),
            "saved_file_readback",
            1,
            1,
            0,
            None,
        ),
    ))
}

fn failed_component_placement(
    result: CallToolResult,
    schematic: &std::path::Path,
) -> CallToolResult {
    crate::outcome::attach(
        result,
        crate::outcome::summary(
            crate::outcome::OutcomeStatus::Failed,
            schematic.display().to_string(),
            "preflight",
            1,
            0,
            1,
            Some(crate::outcome::retry_whole_request()),
        ),
    )
}

/// Read a placement `mirror` argument in eeschema's own vocabulary.
///
/// `"x"` and `"y"` are the axis names the file format uses; `"none"` and an
/// absent argument both mean unmirrored, which eeschema records by omitting
/// the token rather than writing one. Anything else is refused rather than
/// silently dropped: a caller that misspells the axis is asking for a
/// reflection, and placing the symbol unmirrored while reporting success is
/// the failure this argument exists to end.
///
/// Returns `Ok(None)` for unmirrored so it can be handed straight to
/// [`cse::Symbol::set_mirror`].
pub(crate) fn mirror_arg<'a>(
    args: &'a serde_json::Value,
    key: &str,
) -> Result<Option<&'a str>, CallToolResult> {
    match &args[key] {
        serde_json::Value::Null => Ok(None),
        serde_json::Value::String(axis) => match axis.as_str() {
            "x" => Ok(Some("x")),
            "y" => Ok(Some("y")),
            "none" => Ok(None),
            other => Err(crate::tools::invalid_arg(
                key,
                &format!("expected \"x\", \"y\" or \"none\", got {other:?}"),
            )),
        },
        _ => Err(crate::tools::invalid_arg(
            key,
            "expected a string: \"x\", \"y\" or \"none\"",
        )),
    }
}

#[derive(Debug, Clone, Default)]
pub(crate) struct ComponentPlacementExtras<'a> {
    pub lcsc_id: Option<&'a str>,
    pub fields: Option<&'a serde_json::Map<String, serde_json::Value>>,
    /// Where the LCSC number's MPN, manufacturer, datasheet and description
    /// come from. `None` skips the lookup, as templates do.
    pub catalog: Option<&'a crate::sourcing::LcscCatalog>,
}

/// Place one symbol into `sch`: embeds the lib_symbols definition, validates
/// the unit, and adds the positioned instance. Does not write the file --
/// callers own the read/write cycle (single-add and batch-add alike).
#[allow(clippy::too_many_arguments)]
pub(crate) fn place_one_component(
    sch: &mut cse::Schematic,
    instance_paths: &[String],
    project_name: &str,
    lib_id: &str,
    x: f64,
    y: f64,
    rotation: Option<&serde_json::Value>,
    mirror: Option<&str>,
    reference: Option<&str>,
    value: Option<&str>,
    footprint: Option<&str>,
    unit: u32,
    src: &dyn cse::library::SymbolLibrarySource,
    extras: ComponentPlacementExtras<'_>,
    in_use: &mut crate::tools::sch_annotate::DesignatorsInUse,
) -> Result<PlacedComponent, CallToolResult> {
    let rotation = rotation
        .filter(|value| !value.is_null())
        .map_or(Ok(0.0), |value| {
            crate::tools::symbol_rotation_arg(value, "rotation")
        })?;
    // Snap to 1.27mm grid
    let (x, y) = snap_point(x, y, 1.27);

    // Embed the library symbol definition
    if !cse::library::ensure_lib_symbol(sch, lib_id, src) {
        return Err(crate::tools::lib_symbol_not_found_error(lib_id, src));
    }
    let metadata = cse::library::symbol_metadata(sch, lib_id);
    let fields = PlacementFields::resolve(lib_id, &metadata, value, footprint);
    // Unannotated, but keeping the library prefix: annotation numbers `R?`
    // as `R1`, while a bare `?` becomes `1` and a `#FLG` loses the `#` that
    // keeps it off the board (#669).
    let reference = match reference {
        Some(reference) => reference.to_owned(),
        None => format!("{}?", metadata.reference),
    };

    // Validate the unit against the resolved symbol BEFORE writing anything:
    // eeschema silently renders an out-of-range unit as unit 1 and the
    // netlister mis-assigns its pins (#35).
    let unit_count = cse::library::symbol_unit_count(lib_id, src).unwrap_or(1);
    if unit < 1 || unit > unit_count {
        return Err(super::invalid_unit_error(unit, lib_id, unit_count));
    }
    in_use.check(sch, lib_id, unit, &fields.value, &reference)?;

    // Build the Symbol struct
    let mut sym = cse::Symbol::new(lib_id, x, y);
    sym.at.rotation = Some(rotation);
    sym.set_mirror(mirror);
    sym.normalize_orientation();
    sym.unit = unit;
    // eeschema copies the library symbol's exclusions onto a symbol it
    // places, so a mounting hole stays out of the BOM (#124).
    sym.exclude_from_sim = metadata.exclude_from_sim;
    sym.in_bom = fields.in_bom;
    sym.on_board = fields.on_board;
    sym.in_pos_files = metadata.in_pos_files;

    // Reference and Value go where the library anchors them, carried through
    // the placement transform so they follow a rotated body (#101);
    // Footprint/Datasheet/Description stay hidden at the origin. KiCad reads
    // Value, Footprint, Datasheet and Description from the placed instance,
    // not as a fallback from lib_symbols; every library-owned default must be
    // copied even though the embedded definition carries it (#226, #506).
    // Each field's visibility comes from the library definition, as eeschema
    // copies it onto a placed symbol (#853). A library that omits a field
    // keeps the default KiCad gives it: Footprint, Datasheet, Description and
    // custom fields hidden, Reference and Value shown.
    let hidden_in_library = |name: &str, default: bool| {
        cse::library::field_hidden(sch, lib_id, name).unwrap_or(default)
    };
    let hide_reference = hidden_in_library("Reference", false);
    let hide_value = hidden_in_library("Value", false);
    let anchors = cse::library::field_anchors(sch, lib_id);
    // The anchors follow the placement, mirror included: a mirrored body puts
    // its Reference and Value where the reflection lands them, exactly as a
    // rotated body already did (#101). Hardcoding `false` here left a mirrored
    // symbol's fields sitting on the unmirrored side of it.
    let t = copperpour_sexp::geometry::PinTransform {
        comp_x: x,
        comp_y: y,
        rotation_deg: rotation,
        mirror_x: mirror == Some("x"),
        mirror_y: mirror == Some("y"),
    };
    let (ref_x, ref_y, ref_rot) =
        crate::tools::field_at(anchors.reference_at, crate::tools::FALLBACK_REFERENCE_AT, t);
    let (val_x, val_y, val_rot) =
        crate::tools::field_at(anchors.value_at, crate::tools::FALLBACK_VALUE_AT, t);
    let positioned = crate::tools::positioned_property;
    let centred = cse::library::FieldJustify::default();
    sym.properties.push(positioned(
        "Reference",
        &reference,
        ref_x,
        ref_y,
        ref_rot,
        hide_reference,
        anchors.reference_justify,
    ));
    sym.properties.push(positioned(
        "Value",
        &fields.value,
        val_x,
        val_y,
        val_rot,
        hide_value,
        anchors.value_justify,
    ));
    sym.properties.push(positioned(
        "Footprint",
        &fields.footprint,
        x,
        y,
        0.0,
        hidden_in_library("Footprint", true),
        centred,
    ));
    sym.properties.push(positioned(
        "Datasheet",
        &metadata.datasheet,
        x,
        y,
        0.0,
        hidden_in_library("Datasheet", true),
        centred,
    ));
    sym.properties.push(positioned(
        "Description",
        &metadata.description,
        x,
        y,
        0.0,
        hidden_in_library("Description", true),
        centred,
    ));

    // Carry all custom properties from the library symbol (excluding standard 5 and ki_*)
    let lib_custom_props = cse::library::symbol_custom_properties(sch, lib_id);
    for custom in lib_custom_props {
        let hidden = hidden_in_library(&custom.name, true);
        let mut prop = positioned(&custom.name, &custom.value, x, y, 0.0, hidden, centred);
        prop.private = custom.private;
        sym.properties.push(prop);
    }

    // Determine if an LCSC ID is specified or present in library properties
    let effective_lcsc = extras
        .lcsc_id
        .or_else(|| {
            extras
                .fields
                .and_then(|f| f.get("LCSC").and_then(|v| v.as_str()))
        })
        .or_else(|| sym.property("LCSC"))
        .map(String::from);

    let mut lcsc_catalog = None;
    if let Some(lcsc) = effective_lcsc {
        if let Some(p) = sym.properties.iter_mut().find(|p| p.name == "LCSC") {
            p.value = lcsc.clone();
        } else {
            sym.properties
                .push(positioned("LCSC", &lcsc, x, y, 0.0, true, centred));
        }

        if let Some(catalog) = extras.catalog {
            let lookup = catalog.part_info(&lcsc);
            lcsc_catalog = Some(placement_catalog_report(&lookup, catalog));
            if let crate::sourcing::Lookup::Read(Some(info)) = lookup {
                if !info.mfr_part.is_empty()
                    && (sym.property("MPN").is_none() || sym.property("MPN") == Some(""))
                {
                    if let Some(p) = sym.properties.iter_mut().find(|p| p.name == "MPN") {
                        p.value = info.mfr_part;
                    } else {
                        sym.properties.push(positioned(
                            "MPN",
                            &info.mfr_part,
                            x,
                            y,
                            0.0,
                            true,
                            centred,
                        ));
                    }
                }
                if !info.manufacturer.is_empty()
                    && (sym.property("Manufacturer").is_none()
                        || sym.property("Manufacturer") == Some(""))
                {
                    if let Some(p) = sym.properties.iter_mut().find(|p| p.name == "Manufacturer") {
                        p.value = info.manufacturer;
                    } else {
                        sym.properties.push(positioned(
                            "Manufacturer",
                            &info.manufacturer,
                            x,
                            y,
                            0.0,
                            true,
                            centred,
                        ));
                    }
                }
                if !info.datasheet.is_empty()
                    && (sym.property("Datasheet").is_none()
                        || sym.property("Datasheet") == Some("")
                        || sym.property("Datasheet") == Some("~"))
                {
                    if let Some(p) = sym.properties.iter_mut().find(|p| p.name == "Datasheet") {
                        p.value = info.datasheet;
                    }
                }
                if !info.description.is_empty()
                    && (sym.property("Description").is_none()
                        || sym.property("Description") == Some(""))
                {
                    if let Some(p) = sym.properties.iter_mut().find(|p| p.name == "Description") {
                        p.value = info.description;
                    }
                }
            }
        }
    }

    // Apply explicit fields override if supplied
    if let Some(custom_fields) = extras.fields {
        for (key, val) in custom_fields {
            if let Some(val_str) = val.as_str() {
                if let Some(p) = sym.properties.iter_mut().find(|p| p.name == *key) {
                    p.value = val_str.to_string();
                } else {
                    sym.properties
                        .push(positioned(key, val_str, x, y, 0.0, true, centred));
                }
            }
        }
    }

    // Instance entry, keyed to the root sheet UUID like eeschema writes it:
    // (instances (project "<name>" (path "/<root-uuid>" (reference ...) (unit 1))))
    for instance_path in instance_paths {
        sym.set_instance_path(project_name, instance_path, &reference, unit);
    }

    let uuid = sym.uuid.clone();
    sch.add_symbol(sym);

    Ok(PlacedComponent {
        uuid,
        reference,
        requested_rotation: rotation,
        fields,
        lcsc_catalog,
    })
}

/// What the catalog gave for a placed symbol's LCSC number: `{ read, path,
/// reason }` as every catalog report, plus `part_found` when it was read.
fn placement_catalog_report(
    lookup: &crate::sourcing::Lookup<Option<crate::sourcing::JlcpchPartInfo>>,
    catalog: &crate::sourcing::LcscCatalog,
) -> serde_json::Value {
    let mut report = lookup.report(catalog);
    if let crate::sourcing::Lookup::Read(part) = lookup {
        report["part_found"] = json!(part.is_some());
    }
    report
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PlacementFields {
    pub(crate) value: String,
    pub(crate) footprint: String,
    /// `(in_bom …)` and `(on_board …)` as the library sets them, `yes` where
    /// it omits one.
    pub(crate) in_bom: bool,
    pub(crate) on_board: bool,
}

impl PlacementFields {
    /// Resolve the instance-owned placement fields once for both the writer
    /// and the committed-file intent check. Explicit tool arguments win over
    /// the library; a malformed library missing Value keeps the historical
    /// symbol-name fallback.
    pub(crate) fn resolve(
        lib_id: &str,
        metadata: &cse::library::SymbolMetadata,
        value: Option<&str>,
        footprint: Option<&str>,
    ) -> Self {
        let library_value = if metadata.value.is_empty() {
            lib_id.split(':').next_back().unwrap_or("?")
        } else {
            &metadata.value
        };
        Self {
            value: value.unwrap_or(library_value).to_owned(),
            footprint: footprint.unwrap_or(&metadata.footprint).to_owned(),
            in_bom: metadata.in_bom.unwrap_or(true),
            on_board: metadata.on_board.unwrap_or(true),
        }
    }
}

#[derive(Debug, Clone)]
pub(crate) struct PlacedComponent {
    pub(crate) uuid: String,
    pub(crate) reference: String,
    pub(crate) requested_rotation: f64,
    pub(crate) fields: PlacementFields,
    /// The catalog's report on the symbol's LCSC number, when it has one.
    pub(crate) lcsc_catalog: Option<serde_json::Value>,
}
