//! Schematic component inspection, listing, pin locations, annotation, and SVG rendering.

use super::readback::wants_verbose;
use super::{component_not_found_error, malformed_schematic_error};
use crate::mcp::protocol::CallToolResult;
use crate::tools::{get_path, require_array, require_str, ToolContext};
use copperpour_schematic_editor as cse;
use copperpour_sexp::schematic::{
    extract_lib_pins_for_unit, extract_symbol_instances, find_lib_symbol, pin_endpoint,
    pin_outward_direction, read_schematic,
};
use serde_json::json;
use std::collections::BTreeMap;

/// Every property of a placed symbol by name. A repeated name keeps its first
/// value, the one `value` and `footprint` are read from.
fn symbol_properties(symbol: &cse::Symbol) -> BTreeMap<&str, &str> {
    let mut properties = BTreeMap::new();
    for property in &symbol.properties {
        properties
            .entry(property.name.as_str())
            .or_insert(property.value.as_str());
    }
    properties
}

pub(super) async fn handle_get_schematic_component(
    args: &serde_json::Value,
    _ctx: &ToolContext,
) -> anyhow::Result<CallToolResult> {
    let sch_path = get_path(args, "schematic")?;
    let reference = match require_str(args, "reference") {
        Ok(r) => r.to_string(),
        Err(e) => return Ok(e),
    };

    let sch = cse::Schematic::load(&sch_path)?;

    let placed: Vec<_> = sch
        .symbols
        .iter()
        .filter(|symbol| symbol.reference() == Some(reference.as_str()))
        .collect();
    let Some(anchor) = placed.iter().copied().min_by_key(|symbol| symbol.unit) else {
        return Ok(component_not_found_error(&sch_path, &reference));
    };
    let (x, y) = anchor.position();
    let rotation = anchor.at.rotation.unwrap_or(0.0);
    let mirror = anchor.mirror.as_deref().unwrap_or("");
    let units: Vec<_> = placed
        .iter()
        .map(|symbol| {
            let (unit_x, unit_y) = symbol.position();
            let unit_mirror = symbol.mirror.as_deref().unwrap_or("");
            json!({
                "unit": symbol.unit,
                "x": unit_x,
                "y": unit_y,
                "rotation": symbol.at.rotation.unwrap_or(0.0),
                "mirror_x": unit_mirror.contains('x'),
                "mirror_y": unit_mirror.contains('y'),
                "dnp": symbol.dnp,
                "uuid": symbol.uuid
            })
        })
        .collect();
    Ok(CallToolResult::json(&json!({
        "reference": anchor.reference().unwrap_or("?"),
        "value": anchor.value_str().unwrap_or(""),
        "footprint": anchor.footprint().unwrap_or(""),
        "lib_id": anchor.lib_id,
        "x": x,
        "y": y,
        "rotation": rotation,
        "mirror_x": mirror.contains('x'),
        "mirror_y": mirror.contains('y'),
        "dnp": anchor.dnp,
        "uuid": anchor.uuid,
        "unit_count": units.len(),
        "units": units,
        "properties": symbol_properties(anchor)
    })))
}

/// The properties `list_schematic_components` shows by default: not the ones
/// each row already carries (Reference, Value, Footprint), not empty ones, and
/// not the library's Datasheet and Description, which repeat on every
/// instance of a symbol. On a 57-part sheet they were most of the response.
fn listed_properties<'a>(mut properties: BTreeMap<&'a str, &'a str>) -> BTreeMap<&'a str, &'a str> {
    const LEFT_OUT: [&str; 5] = [
        "Reference",
        "Value",
        "Footprint",
        "Datasheet",
        "Description",
    ];
    properties.retain(|name, value| !LEFT_OUT.contains(name) && !value.is_empty());
    properties
}

pub(super) async fn handle_list_schematic_components(
    args: &serde_json::Value,
    _ctx: &ToolContext,
) -> anyhow::Result<CallToolResult> {
    let sch_path = get_path(args, "schematic")?;
    let verbose = wants_verbose(args);
    let sch = cse::Schematic::load(&sch_path)?;

    let items: Vec<serde_json::Value> = sch
        .symbols
        .iter()
        .map(|sym| {
            let (x, y) = sym.position();
            let rotation = sym.at.rotation.unwrap_or(0.0);
            let mirror = sym.mirror.as_deref().unwrap_or("");
            json!({
                "reference": sym.reference().unwrap_or("?"),
                "value": sym.value_str().unwrap_or(""),
                "footprint": sym.footprint().unwrap_or(""),
                "lib_id": sym.lib_id,
                "x": x,
                "y": y,
                "rotation": rotation,
                "mirror_x": mirror.contains('x'),
                "mirror_y": mirror.contains('y'),
                "dnp": sym.dnp,
                "properties": if verbose {
                    symbol_properties(sym)
                } else {
                    listed_properties(symbol_properties(sym))
                }
            })
        })
        .collect();

    Ok(CallToolResult::json(&json!({
        "count": items.len(),
        "components": items
    })))
}

pub(super) async fn handle_annotate_schematic(
    args: &serde_json::Value,
    _ctx: &ToolContext,
) -> anyhow::Result<CallToolResult> {
    use crate::tools::sch_annotate::{
        annotate_file, annotate_hierarchy, opt_bool, opt_string, AnnotateOptions,
    };
    let sch_path = get_path(args, "schematic")?;
    let hierarchy = match crate::tools::design_review::requested_schematic_scope(args) {
        Ok(scope) => scope == "hierarchy",
        Err(refusal) => return Ok(refusal),
    };
    let options = AnnotateOptions {
        resolve_duplicates: match opt_bool(args, "resolve_duplicates") {
            Ok(value) => value,
            Err(refusal) => return Ok(refusal),
        },
        dry_run: match opt_bool(args, "dry_run") {
            Ok(value) => value,
            Err(refusal) => return Ok(refusal),
        },
        project: match opt_string(args, "project") {
            Ok(value) => value,
            Err(refusal) => return Ok(refusal),
        },
    };
    Ok(tokio::task::spawn_blocking(move || {
        if hierarchy {
            annotate_hierarchy(&sch_path, options)
        } else {
            annotate_file(&sch_path, options)
        }
    })
    .await?)
}

pub(super) async fn handle_get_schematic_pin_locations(
    args: &serde_json::Value,
    _ctx: &ToolContext,
) -> anyhow::Result<CallToolResult> {
    let sch_path = get_path(args, "schematic")?;
    let reference = match require_str(args, "reference") {
        Ok(r) => r.to_string(),
        Err(e) => return Ok(e),
    };

    let (_, tree) = read_schematic(&sch_path)?;
    match pin_locations_for_reference(&tree, &reference) {
        Ok(result) => Ok(CallToolResult::json(&result)),
        // Not on this sheet: say which sheet it is on, if any.
        Err(_)
            if !extract_symbol_instances(&tree)
                .iter()
                .any(|instance| instance.reference == reference) =>
        {
            Ok(component_not_found_error(&sch_path, &reference))
        }
        Err(error) => Ok(malformed_schematic_error(&sch_path, error)),
    }
}

pub(super) fn pin_locations_for_reference(
    tree: &copperpour_sexp::SexpNode,
    reference: &str,
) -> Result<serde_json::Value, String> {
    let instances = extract_symbol_instances(tree);
    let placed: Vec<_> = instances
        .iter()
        .filter(|instance| instance.reference == reference)
        .collect();
    let Some(anchor) = placed.iter().copied().min_by_key(|instance| instance.unit) else {
        return Err(format!("Component '{reference}' not found"));
    };
    let lib_syms = tree
        .find("lib_symbols")
        .map(|node| node.find_all("symbol"))
        .unwrap_or_default();

    let mut all_pins = Vec::new();
    let mut units = Vec::new();
    for instance in placed.iter().copied() {
        // A missing embedded definition is an error, not an empty pin list —
        // silently returning [] hid bad lib_ids until netlisting (#34).
        let Some(symbol) = find_lib_symbol(&lib_syms, instance) else {
            return Err(format!(
                "Component '{}' unit {} has no embedded definition for '{}' in this \
                 schematic's lib_symbols — re-add it with a valid lib_id",
                reference,
                instance.unit,
                instance.lib_symbol_name()
            ));
        };
        let lib_pins = extract_lib_pins_for_unit(symbol, instance.unit);
        if lib_pins.is_empty() {
            if let Some(parent) = symbol.find_str("extends") {
                return Err(format!(
                    "Component '{}' unit {}: the embedded definition for '{}' is an \
                     (extends \"{}\") stub with no pins — re-add the component so the \
                     definition is embedded in full",
                    reference,
                    instance.unit,
                    instance.lib_symbol_name(),
                    parent
                ));
            }
        }

        let transform = instance.pin_transform();
        let pins: Vec<serde_json::Value> = lib_pins
            .iter()
            .map(|pin| {
                let (x, y) = pin_endpoint(pin, transform);
                json!({
                    "number": pin.number,
                    "name": pin.name,
                    "unit": instance.unit,
                    "x": x,
                    "y": y,
                    "orientation_degrees": pin_outward_direction(pin, transform),
                    "length_mm": pin.length
                })
            })
            .collect();
        all_pins.extend(pins.iter().cloned());
        units.push(json!({
            "unit": instance.unit,
            "x": instance.x,
            "y": instance.y,
            "rotation": instance.rotation,
            "mirror_x": instance.mirror_x,
            "mirror_y": instance.mirror_y,
            "pins": pins
        }));
    }

    Ok(json!({
        "reference": reference,
        // Preserve the original single-placement fields as the logical
        // component anchor while exposing every real placement below.
        "component_x": anchor.x,
        "component_y": anchor.y,
        "x": anchor.x,
        "y": anchor.y,
        "rotation": anchor.rotation,
        "mirror_x": anchor.mirror_x,
        "mirror_y": anchor.mirror_y,
        "unit_count": units.len(),
        "units": units,
        "pins": all_pins
    }))
}

pub(super) async fn handle_batch_get_pin_locations(
    args: &serde_json::Value,
    _ctx: &ToolContext,
) -> anyhow::Result<CallToolResult> {
    let sch_path = get_path(args, "schematic")?;
    // Required by the schema. Defaulting it returned `{"components": []}` —
    // indistinguishable from "none of your references exist" (#218).
    let refs = match require_array(args, "references") {
        Ok(a) => a
            .iter()
            .filter_map(|v| v.as_str().map(String::from))
            .collect::<Vec<_>>(),
        Err(e) => return Ok(e),
    };

    let (_, tree) = read_schematic(&sch_path)?; // single read
    let results: Vec<serde_json::Value> = refs
        .iter()
        .map(
            |reference| match pin_locations_for_reference(&tree, reference) {
                Ok(component) => component,
                Err(error) => json!({ "reference": reference, "error": error }),
            },
        )
        .collect();

    Ok(CallToolResult::json(&json!({ "components": results })))
}

/// A stable per-schematic directory under the system temp dir.
///
/// The old handler made a fresh `konnect_<uuid>` directory for every call and
/// deleted it again, so nothing survived to be returned. Keeping a uuid per call
/// would instead leak a directory per call, so the slot is derived from the
/// schematic's path: repeated views of the same sheet overwrite one file, and
/// two sheets that merely share a stem do not collide.
pub(super) fn schematic_view_dir(schematic: &std::path::Path) -> std::path::PathBuf {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    schematic.hash(&mut hasher);
    std::env::temp_dir()
        .join("copperpour-schematic-views")
        .join(format!("{:016x}", hasher.finish()))
}

pub(super) async fn handle_get_schematic_view(
    args: &serde_json::Value,
    ctx: &ToolContext,
) -> anyhow::Result<CallToolResult> {
    let sch_path = get_path(args, "schematic")?;
    let out_dir = schematic_view_dir(&sch_path);
    tokio::fs::create_dir_all(&out_dir).await?;

    // KiCad has no schematic rasteriser: `sch export` offers no bitmap format
    // and there is no `sch render` at all. SVG is what there is.
    let svg_path = crate::tools::cli::render_schematic_svg(
        &ctx.config.kicad_cli,
        &sch_path,
        &out_dir,
        &crate::tools::cli::SchematicSvgOptions::default(),
    )
    .await?;

    // Deliberately not deleted. The previous handler rendered the file, read its
    // length, removed it, and then reported "The SVG file has been generated" —
    // the caller got neither the image nor a path to it.
    let bytes = tokio::fs::metadata(&svg_path).await?.len();

    Ok(CallToolResult::json(&json!({
        "schematic": sch_path.display().to_string(),
        "svg": svg_path.display().to_string(),
        "bytes": bytes,
        "format": "svg"
    })))
}
