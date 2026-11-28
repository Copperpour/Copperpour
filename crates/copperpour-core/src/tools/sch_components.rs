//! `sch_components` toolset — add, edit, move, rotate, delete schematic symbols.
//!
//! Simple CRUD operations use `copperpour_schematic_editor` (cse) for structured
//! round-trip parsing.  Pin coordinate math still delegates to
//! `copperpour_sexp::geometry::transform_pin`.

mod deletion;
mod library_ops;
mod placement;
mod properties;
mod query;
mod readback;
mod transform;

#[cfg(test)]
mod component_ops_tests;
#[cfg(test)]
mod multi_unit_component_tests;
#[cfg(test)]
mod native_dnp_tests;
#[cfg(test)]
mod tests;
#[cfg(test)]
mod transform_tests;

use deletion::handle_delete_schematic_component;
pub(crate) use deletion::{
    commit_component_deletion, indexed_uuid_items, plan_component_and_item_deletions,
    ComponentDeleteTargetError,
};
use library_ops::{
    handle_add_component_annotation, handle_group_components, handle_replace_component,
    handle_reset_schematic_field_positions, handle_update_symbols_from_library,
};
pub(crate) use placement::{
    handle_add_schematic_component, mirror_arg, place_one_component, ComponentPlacementExtras,
    PlacementFields,
};
use properties::handle_edit_schematic_component;
pub(crate) use properties::{
    dnp_property_refusal, field_placement_unit, is_dnp_property, plan_batch_field_placements,
    set_dnp_attribute, set_property_value, validate_field_placement_unit_scope,
};
use query::{
    handle_annotate_schematic, handle_batch_get_pin_locations, handle_get_schematic_component,
    handle_get_schematic_pin_locations, handle_get_schematic_view,
    handle_list_schematic_components,
};
pub(crate) use readback::{
    compact_placement, load_committed_component_snapshot, placed_component_readback,
    verify_component_mutation_intents, wants_verbose, ComponentMutationIntent, ComponentTargetUnit,
};
pub(crate) use transform::handle_move_region;
use transform::{
    handle_mirror_schematic_component, handle_move_connected, handle_move_schematic_component,
    handle_rotate_schematic_component,
};

use crate::mcp::error::ToolErrorKind;
use crate::mcp::protocol::CallToolResult;
#[cfg(test)]
use crate::mcp::protocol::ToolContent;
#[cfg(test)]
use crate::tools::{
    find_all_symbol_instance_blocks, sch_wiring::observed_no_connect_moves, ToolContext,
};
#[cfg(test)]
use copperpour_schematic_editor as cse;
#[cfg(test)]
use copperpour_sexp::{
    commit_command, parse_sexp,
    schematic::{extract_symbol_instances, parse_at, read_schematic},
    writer::{apply_edits, find_block_with_leading_whitespace, SexpEdit},
    SchematicCommand, SexpError,
};
#[cfg(test)]
use deletion::{component_delete_commit_refusal, plan_component_deletion};
#[cfg(test)]
use properties::{placement_landed, symbol_unit_from_block, FieldPlacement, PlacementRefusal};
#[cfg(test)]
use query::{pin_locations_for_reference, schematic_view_dir};
#[cfg(test)]
pub(crate) use readback::COMPONENT_SAVE_BEFORE_SNAPSHOT;
#[cfg(test)]
use readback::{
    checked_instance_paths, component_mutation_readback_from_schematic,
    component_target_from_source, tool_error_summary, verified_component_readback,
    COMPONENT_COMMITTED_LOAD_FAULT, COMPONENT_PROSPECTIVE_FAULT,
};
#[cfg(test)]
use std::collections::{BTreeMap, BTreeSet};

use crate::tool;
use crate::tools::reference_hint::component_not_found;
use crate::tools::ToolDef;
use serde_json::json;

pub(super) fn component_not_found_error(
    sch_path: &std::path::Path,
    reference: &str,
) -> CallToolResult {
    let message = component_not_found(sch_path, reference);
    CallToolResult::error_kind(
        ToolErrorKind::StaleTarget {
            target: format!("component {reference}"),
            reason: message.clone(),
        },
        message,
    )
}

pub(super) fn invalid_unit_error(unit: u32, lib_id: &str, unit_count: u32) -> CallToolResult {
    let message = format!(
        "Invalid unit {unit} for '{lib_id}': the symbol has {unit_count} unit(s) (valid: 1..={unit_count})."
    );
    CallToolResult::error_kind(
        ToolErrorKind::InvalidArgument {
            field: "unit".to_owned(),
            reason: message.clone(),
        },
        message,
    )
}

pub(super) fn malformed_schematic_error(
    sch_path: &std::path::Path,
    message: impl Into<String>,
) -> CallToolResult {
    let message = message.into();
    CallToolResult::error_kind(
        ToolErrorKind::MalformedFile {
            path: copperpour_sexp::paths::display_path(sch_path),
            reason: message.clone(),
        },
        message,
    )
}

pub(crate) fn field_placements_schema() -> serde_json::Value {
    json!({
        "type": "object",
        "description": "Move or hide a field's text, keyed by field name (Reference, Value, Footprint, or a custom property). Each entry may set x, y, rotation and hide; an omitted part is left as the file has it. Coordinates are absolute schematic millimetres, as KiCad stores them — not offsets from the body; rotation is stored relative to the parent symbol (so on a 90° or 270° symbol, rotation: 90 draws horizontally).",
        "additionalProperties": {
            "type": "object",
            "properties": {
                "x": { "type": "number" },
                "y": { "type": "number" },
                "rotation": { "type": "number", "description": "Field text rotation in degrees, stored relative to the parent symbol as KiCad does (not an absolute sheet angle — on a 90° or 270° symbol, rotation: 0 draws vertically and rotation: 90 draws horizontally; see units[].field_placements.<name>.drawn_orientation in the response)." },
                "hide": { "type": "boolean", "description": "true hides the field's text on the sheet; false shows it." }
            }
        }
    })
}

pub(crate) fn field_placement_unit_schema() -> serde_json::Value {
    json!({
        "type": "integer",
        "description": "Which placed unit field_placements applies to. Required when x or y is given and the component has more than one placed unit, because a field position is absolute and one coordinate cannot be right for several placements."
    })
}

pub fn tools() -> Vec<ToolDef> {
    vec![
        tool!(
            "add_schematic_component",
            "Add a symbol from a KiCAD library to the schematic. The symbol is snapped \
             to the 1.27mm schematic grid. Preserves every saved hierarchy instance, reports \
             committed-file readback, copies the library Value and Footprint unless explicitly \
             overridden, and refuses stale instance metadata before writing. \
             Specify position in schematic mm coordinates.",
            super::placement_schema(json!({
                "type": "object",
                "properties": {
                    "verbose": { "type": "boolean", "description": "Return the full saved readback per part (fields, field placements, instance paths, units) instead of the compact entry. Default false." },
                    "schematic": { "type": "string", "description": "Path to .kicad_sch file" },
                    "lib_id": { "type": "string", "description": "Library:Symbol (e.g. 'Device:R')" },
                    "x": { "type": "number", "description": "X position in mm" },
                    "y": { "type": "number", "description": "Y position in mm" },
                    "rotation": { "type": "number", "description": "Symbol rotation in degrees, an exact multiple of 90.", "default": 0 },
                    "autoplace_fields": super::sch_field_autoplace::opt_in_schema(),
                    "mirror": { "type": "string", "enum": ["x", "y", "none"], "description": "Reflect the placed symbol about an axis, using eeschema's own vocabulary: 'x' negates screen-Y, 'y' negates screen-X. Omit (or 'none') for an unmirrored symbol. Mirroring is applied after rotation, and is not interchangeable with rotation 180 for a symbol whose pins are not symmetric. The stored orientation is normalized to the rotation and mirror pair eeschema writes (for example, 180° with 'x' is stored and reported as 0° with 'y')." },
                    "reference": { "type": "string", "description": "Optional reference designator. Default: the library prefix plus '?' (e.g. 'R?', '#FLG?'), which annotate_schematic numbers. A numbered one already placed on any sheet of the hierarchy is refused as invalid_argument, unless this is another unit of that part with its value" },
                    "value": { "type": "string", "description": "Optional override for the library symbol's Value" },
                    "footprint": { "type": "string", "description": "Optional override for the library symbol's Footprint" },
                    "unit": { "type": "integer", "minimum": 1, "description": "Unit number for multi-unit symbols (gate/part selection). Default 1.", "default": 1 },
                    "lcsc_id": { "type": "string", "description": "Optional LCSC part number (e.g. 'C2856837'). Fills MPN, Manufacturer, Datasheet and Description from the local JLCPCB catalog (the jlcpcb_db_path setting, else its default path) at placement time; the response's lcsc_catalog says what the catalog gave." },
                    "fields": { "type": "object", "additionalProperties": true, "description": "Additional custom properties to set on the placed component instance (e.g. {\"LCSC\": \"C123\", \"MPN\": \"...\"})" }
                },
                "required": ["schematic", "lib_id", "x", "y"]
            })),
            |args, ctx| async move { handle_add_schematic_component(args, ctx).await }
        ),
        tool!(
            "delete_schematic_component",
            "Remove a component by reference designator, including every placed unit of a multi-unit symbol.",
            json!({
                "type": "object",
                "properties": {
                    "schematic": { "type": "string" },
                    "reference": { "type": "string", "description": "Reference designator (e.g. 'R1')" }
                },
                "required": ["schematic", "reference"]
            }),
            |args, ctx| async move { handle_delete_schematic_component(args, ctx).await }
        ),
        tool!(
            "edit_schematic_component",
            "Update fields (Reference, Value, Footprint, custom properties), KiCad's native do-not-populate (DNP) attribute and its Exclude from BOM and Exclude from board flags consistently across every placed unit of a component, remove custom properties, and move or hide the text of any field. On a sheet placed more than once, the change reaches every instance of the sheet.",
            json!({
                "type": "object",
                "properties": {
                    "schematic": { "type": "string" },
                    "reference": { "type": "string", "description": "Current reference designator" },
                    "new_reference": { "type": "string", "description": "New reference designator (optional). Refused as ambiguous_target on a sheet placed more than once whose instances carry different designators; annotate_schematic sets those" },
                    "value": { "type": "string", "description": "New value (optional)" },
                    "footprint": { "type": "string", "description": "New footprint (optional)" },
                    "datasheet": { "type": "string", "description": "New datasheet URL (optional)" },
                    "dnp": {
                        "type": "boolean",
                        "description": "Set KiCad's native do-not-populate attribute, (dnp yes|no), on every placed unit (never one unit: 'unit' scopes field_placements only): true marks the part DNP, false fits it. This is the attribute KiCad's plot marks and a BOM export with DNP exclusion honours; a property named DNP is not, and 'fields' refuses one. Per-variant DNP overrides are left as they are."
                    },
                    "in_bom": {
                        "type": "boolean",
                        "description": "Set KiCad's native (in_bom yes|no) flag on every placed unit (never one unit: 'unit' scopes field_placements only): false is Exclude from bill of materials, which leaves the part out of a BOM export; true lists it. Omitted, the file's value is kept."
                    },
                    "on_board": {
                        "type": "boolean",
                        "description": "Set KiCad's native (on_board yes|no) flag on every placed unit (never one unit: 'unit' scopes field_placements only): false is Exclude from board, which keeps the part off the PCB when the board is updated from the schematic; true puts it there. Omitted, the file's value is kept."
                    },
                    "fields": {
                        "type": "object",
                        "additionalProperties": true,
                        "description": "Additional property fields to set as key:value pairs"
                    },
                    "remove_fields": {
                        "type": "array",
                        "items": { "type": "string" },
                        "description": "Custom property names to delete from every placed unit. KiCad's mandatory fields (Reference, Value, Footprint, Datasheet, Description) cannot be removed. A name no unit carries is reported in absent_fields."
                    },
                    "field_placements": field_placements_schema(),
                    "unit": field_placement_unit_schema()
                },
                "required": ["schematic", "reference"]
            }),
            |args, ctx| async move { handle_edit_schematic_component(args, ctx).await }
        ),
        tool!(
            "get_schematic_component",
            "Get a component's shared properties, its DNP state (the lowest placed unit's at the top level; each placed unit's under units) and every placed unit's position. Use get_schematic_pin_locations for pins.",
            json!({
                "type": "object",
                "properties": {
                    "schematic": { "type": "string" },
                    "reference": { "type": "string" }
                },
                "required": ["schematic", "reference"]
            }),
            |args, ctx| async move { handle_get_schematic_component(args, ctx).await }
        ),
        tool!(
            "list_schematic_components",
            "List all symbol instances in a schematic with their positions, values, \
             footprints, DNP state, and properties. properties leaves out what is already \
             listed (Reference, Value, Footprint), empty fields, and the library's \
             Datasheet and Description; verbose: true lists every field.",
            json!({
                "type": "object",
                "properties": {
                    "schematic": { "type": "string" },
                    "verbose": { "type": "boolean", "description": "List every field in properties, including the ones repeated above and empty ones. Default false." }
                },
                "required": ["schematic"]
            }),
            |args, ctx| async move { handle_list_schematic_components(args, ctx).await }
        ),
        tool!(
            "move_schematic_component",
            "Move a component's lowest-numbered unit to a new position and translate every \
             other placed unit by the same delta; with unit, move only that placed unit. \
             Does NOT adjust connected wires. \
             Junction dots are re-judged where the pins moved: a dot left with fewer \
             than three exits (wires, pins, sheet pins, bus entries) is removed unless \
             it is what joins wires there, and a pin that makes three, mid-span on one \
             wire or where wire ends meet, gains one, \
             reported as junctions_pruned_count and junctions_added_count. A no-connect \
             flag travels with the pin it protects, reported as no_connects_moved; the \
             move is refused before writing when that pin cannot be followed one-to-one. \
             On a sheet placed more than once, the change reaches every instance of the sheet.",
            json!({
                "type": "object",
                "properties": {
                    "schematic": { "type": "string" },
                    "reference": { "type": "string" },
                    "x": { "type": "number", "description": "New X position in mm" },
                    "y": { "type": "number", "description": "New Y position in mm" },
                    "unit": { "type": "integer", "minimum": 1, "description": "Which placed unit to move to x, y. Every other placed unit stays where it is. Refused, naming the placed units, when the component has no such unit on this sheet. When omitted, every placed unit moves together." }
                },
                "required": ["schematic", "reference", "x", "y"]
            }),
            |args, ctx| async move { handle_move_schematic_component(args, ctx).await }
        ),
        tool!(
            "rotate_schematic_component",
            "Set the lowest-numbered unit's absolute rotation and rotate every other placed \
             unit by the same delta. Does NOT adjust connected wires. Each unit's \
             Reference and Value text turns with its body, keeping any offset the caller \
             gave it; use reset_schematic_field_positions to put fields back on their \
             library anchors instead. Each unit's orientation is stored in the rotation \
             and mirror pair eeschema writes for it (for example, 90° with 'y' is stored \
             as 270° with 'x') and reported from the saved file. Junction dots are \
             re-judged where the pins turned, reported as junctions_pruned_count and \
             junctions_added_count. A no-connect flag travels with the pin it protects, \
             reported as no_connects_moved; the turn is refused before writing when that \
             pin cannot be followed one-to-one or a selected unit has a saved angle that \
             is not a multiple of 90°. On a sheet placed more than once, the \
             change reaches every instance of the sheet.",
            json!({
                "type": "object",
                "properties": {
                    "schematic": { "type": "string" },
                    "reference": { "type": "string" },
                    "rotation": { "type": "number", "description": "Absolute symbol rotation in degrees, an exact multiple of 90." },
                    "autoplace_fields": { "type":"boolean", "default":false, "description":"Preview the prospective rotation and clearer Reference and Value positions without writing. Repeat with the preview revision and individual approvals to apply both in one write." },
                    "revision": { "type":"string", "description":"Saved revision from the rotation preview; requires approvals." },
                    "turn": { "type":"string", "description":"Turn token from the rotation preview; required to apply even with no field approvals." },
                    "approvals": super::sch_field_autoplace::approval_schema()
                },
                "required": ["schematic", "reference", "rotation"]
            }),
            |args, ctx| async move { handle_rotate_schematic_component(args, ctx).await }
        ),
        tool!(
            "mirror_schematic_component",
            "Set or clear the mirror of a placed component, in eeschema's own vocabulary: \
             'x' negates screen-Y, 'y' negates screen-X, 'none' clears it. Each unit is \
             reflected about its own origin, after its rotation. Without 'unit', every \
             placed unit takes the requested mirror; with 'unit', only that unit changes, \
             so one gate of a package can be reflected while its siblings and its power \
             unit are left as they are. Reference and Value text reflect with the body. \
             The orientation is stored in the rotation and mirror pair eeschema writes for \
             it, so a reflection can change the stored rotation without turning the body \
             (180° with 'x' is stored as 0° with 'y'). A later rotate_schematic_component \
             works from the stored angle, so read it from mirrored_units rather than \
             assuming the one you set. mirrored_units reports each selected unit as \
             written, with changed=false for one already as requested, and the file is \
             not rewritten when nothing changes. Does NOT adjust connected \
             wires. Junction dots are \
             re-judged where the pins moved, reported as junctions_pruned_count and \
             junctions_added_count. A no-connect flag travels with the pin it protects, \
             reported as no_connects_moved; the change is refused before writing when \
             that pin cannot be followed one-to-one or a selected unit has a saved angle \
             that is not a multiple of 90°. On a sheet placed more than once, the \
             change reaches every instance of the sheet.",
            json!({
                "type": "object",
                "properties": {
                    "schematic": { "type": "string" },
                    "reference": { "type": "string" },
                    "mirror": { "type": "string", "enum": ["x", "y", "none"], "description": "The mirror each selected unit ends up with. Absolute, not a toggle: asking for the state a unit already has leaves it unchanged. 'x' and 'y' are exclusive, as in eeschema; reflecting about both axes is rotation 180." },
                    "unit": { "type": "integer", "minimum": 1, "description": "Optional unit number. Only that placed unit is changed, and it must be placed on this sheet. When omitted, every placed unit of the component is changed." }
                },
                "required": ["schematic", "reference", "mirror"]
            }),
            |args, ctx| async move { handle_mirror_schematic_component(args, ctx).await }
        ),
        tool!(
            "move_connected",
            "Not implemented: always refuses. Use move_schematic_component to move the symbol \
             without moving its wires, then re-route or use connect_pins for the affected nets.",
            // No parameters: the handler refuses unconditionally, and the
            // schema-parameter guard (rightly) refuses a schema that advertises
            // arguments nothing reads. The old parameters are documented in
            // Konnect v0.13.0's docs/API_MIGRATIONS.md alongside #285's removals.
            json!({
                "type": "object",
                "properties": {},
                "required": []
            }),
            |args, ctx| async move { handle_move_connected(args, ctx).await }
        ),
        tool!(
            "move_region",
            "Move all symbol units whose origins are within a bounding box by a given offset. \
             With include 'symbols' (the default) only symbols move, snapped to the 1.27 mm \
             grid, and connected wires are NOT adjusted. With include 'all' the block moves \
             rigidly by exactly (dx, dy): symbols, wires, buses, bus entries, junctions, labels, \
             no-connects, texts, text boxes, directive labels and sheets inside the box, counted \
             by kind in items_moved (no-connects in no_connects_moved); graphic shapes and \
             images stay. It is refused before writing (plan_blocked) when a wire, \
             bus, bus entry, symbol or sheet crosses the box edge, or when a pin's net or \
             a named net would change. \
             Junction dots are re-judged once for the whole region where its pins moved, \
             reported as junctions_pruned_count and junctions_added_count. No-connect flags \
             travel with the pins they protect, reported as no_connects_moved; the move is \
             refused before writing when a pin cannot be followed one-to-one.",
            json!({
                "type": "object",
                "properties": {
                    "schematic": { "type": "string" },
                    "x1": { "type": "number", "description": "Region bounding box min X" },
                    "y1": { "type": "number", "description": "Region bounding box min Y" },
                    "x2": { "type": "number", "description": "Region bounding box max X" },
                    "y2": { "type": "number", "description": "Region bounding box max Y" },
                    "dx": { "type": "number", "description": "X offset to move by" },
                    "dy": { "type": "number", "description": "Y offset to move by" },
                    "include": {
                        "type": "string",
                        "enum": ["symbols", "all"],
                        "default": "symbols",
                        "description": "'symbols' moves symbol units only; 'all' moves everything inside the box as one rigid block"
                    }
                },
                "required": ["schematic", "x1", "y1", "x2", "y2", "dx", "dy"]
            }),
            |args, ctx| async move { handle_move_region(args, ctx).await }
        ),
        tool!(
            "annotate_schematic",
            "Assign reference designators in the saved schematic the way eeschema's Tools → \
             Annotate does with its defaults: every '?' designator gets the first free number \
             for its prefix in the project, in ascending X per sheet instance, numbers used on \
             any sheet instance in the file are reserved, and numbered designators are kept. \
             The units of one multi-unit part (same library item, value and unit count, distinct \
             units) share one designator; when which units belong together is not proven they \
             are reported in 'unresolved', never guessed. Two separate parts sharing a \
             designator are reported in 'unresolved' with a 'partial' outcome; they are \
             renumbered only when resolve_duplicates is true (the first in ascending X keeps \
             the number). Annotates one project's instance records — the schematic's owner by \
             default, or 'project' — and never touches another project's. Writes both the \
             Reference property and the instances block, then reads the file back; every count \
             in the response comes from that readback. By default only the supplied file is \
             annotated, and numbers used on the project's other sheets are reserved through \
             its sheet tree ('other_sheets_consulted'). With schematic_scope 'hierarchy' on the \
             project's root, every sheet instance of the hierarchy is annotated at once, as \
             eeschema's 'Entire schematic' does: ordered by page, then X, duplicates found \
             across sheets, every changed file committed in one transaction. Copperpour's own \
             implementation — kicad-cli has no annotate command. dry_run returns the plan \
             without writing.",
            json!({
                "type": "object",
                "properties": {
                    "schematic": { "type": "string", "description": "Path to the .kicad_sch file" },
                    "resolve_duplicates": {
                        "type": "boolean",
                        "description": "Renumber separate single-unit parts that share a designator in the project: the first in ascending X keeps it (in hierarchy scope, the first by page, then X), the rest get the first free number. Default false: duplicates are reported, not changed. A shared designator that could be the units of one multi-unit package is never renumbered either way.",
                        "default": false
                    },
                    "dry_run": {
                        "type": "boolean",
                        "description": "Report what would be assigned without writing. Default false.",
                        "default": false
                    },
                    "project": {
                        "type": "string",
                        "description": "Project whose instance records to annotate, as named in the symbols' (instances (project …)) blocks. Default: the project that owns the schematic (its .kicad_pro), else the only project the file names; refused with the candidates when that is ambiguous. Other projects' records are never edited."
                    },
                    "schematic_scope": {
                        "type": "string",
                        "enum": ["file", "hierarchy"],
                        "description": "Annotate only the supplied file (default) or, given the project's root schematic, every sheet instance of its hierarchy",
                        "default": "file"
                    }
                },
                "required": ["schematic"]
            }),
            |args, ctx| async move { handle_annotate_schematic(args, ctx).await }
        ),
        tool!(
            "get_schematic_pin_locations",
            "Get the exact schematic-space (X,Y) coordinates of every pin on every placed unit of a component, \
             accounting for rotation and mirroring. Uses the canonical pin transform. \
             Each pin also reports 'orientation_degrees', the direction leading away \
             from the symbol body (0 = east) — a net label at the pin must read that \
             way or its text runs back over the symbol's pin names — and 'length_mm'. \
             The component and each unit report 'rotation', 'mirror_x' and 'mirror_y'.",
            json!({
                "type": "object",
                "properties": {
                    "schematic": { "type": "string" },
                    "reference": { "type": "string" }
                },
                "required": ["schematic", "reference"]
            }),
            |args, ctx| async move { handle_get_schematic_pin_locations(args, ctx).await }
        ),
        tool!(
            "batch_get_schematic_pin_locations",
            "Get pin locations for multiple components in a single file read. Reports the \
             same fields as get_schematic_pin_locations, including \
             'orientation_degrees', 'length_mm', 'mirror_x' and 'mirror_y'.",
            json!({
                "type": "object",
                "properties": {
                    "schematic": { "type": "string" },
                    "references": {
                        "type": "array",
                        "items": { "type": "string" },
                        "description": "List of reference designators"
                    }
                },
                "required": ["schematic", "references"]
            }),
            |args, ctx| async move { handle_batch_get_pin_locations(args, ctx).await }
        ),
        tool!(
            "add_component_annotation",
            "Add or update a custom property supplied as `key` and `value` across every placed unit of a component. Reference, Value, Footprint and Datasheet are set through edit_schematic_component's own arguments, and a key named DNP is refused: do-not-populate is KiCad's native attribute, set with edit_schematic_component's 'dnp'. On a sheet placed more than once, the change reaches every instance of the sheet.",
            json!({
                "type": "object",
                "properties": {
                    "schematic": { "type": "string", "description": "Path to .kicad_sch file" },
                    "reference": { "type": "string", "description": "Component reference designator (e.g. 'R1')" },
                    "key": { "type": "string", "description": "Property name" },
                    "value": { "type": "string", "description": "Property value" }
                },
                "required": ["schematic", "reference", "key", "value"]
            }),
            |args, ctx| async move { handle_add_component_annotation(args, ctx).await }
        ),
        tool!(
            "group_components",
            "Add or update a group property on every placed unit of multiple components. On a sheet placed more than once, the change reaches every instance of the sheet.",
            json!({
                "type": "object",
                "properties": {
                    "schematic": { "type": "string", "description": "Path to .kicad_sch file" },
                    "references": {
                        "type": "array",
                        "items": { "type": "string" },
                        "description": "List of reference designators to group"
                    },
                    "group_name": { "type": "string", "description": "Group name to assign" }
                },
                "required": ["schematic", "references", "group_name"]
            }),
            |args, ctx| async move { handle_group_components(args, ctx).await }
        ),
        tool!(
            "replace_component",
            "Replace every placed unit of a component with a new library symbol while \
             preserving unit numbers. A unit override is accepted only for a single \
             placement. A replaced placement stops using its sheet-local alias. Repeated \
             library IDs on a selected placement are refused before writing. \
             The old embedded definition is removed after its last use on the \
             sheet, reported as removed_lib_symbol (null if nothing was removed). \
             New definitions use the schematic writer's nested layout. \
             Junction dots are re-judged once for the whole replacement, \
             reported as junctions_pruned_count and junctions_added_count. A no-connect \
             flag follows the same symbol UUID, unit, and pin number when that mapping is \
             one-to-one; removed, renumbered, or duplicated protected pins refuse before \
             writing. On a sheet placed more than once, the change reaches every instance \
             of the sheet.",
            json!({
                "type": "object",
                "properties": {
                    "schematic": { "type": "string", "description": "Path to .kicad_sch file" },
                    "reference": { "type": "string", "description": "Component reference designator (e.g. 'U1')" },
                    "new_lib_id": { "type": "string", "description": "New Library:Symbol identifier (e.g. 'Device:C')" },
                    "unit": { "type": "integer", "minimum": 1, "description": "Optional unit number for a single placed unit; rejected as ambiguous when the reference has multiple placements. When omitted, every existing unit number is preserved and validated against the new symbol." }
                },
                "required": ["schematic", "reference", "new_lib_id"]
            }),
            |args, ctx| async move { handle_replace_component(args, ctx).await }
        ),
        tool!(
            "update_symbols_from_library",
            "Re-embed placed symbols' definitions from their libraries, like KiCad's \
             'Update Symbols from Library'. A schematic carries its own copy of every \
             symbol, so editing one in its library leaves the sheet drawing the old \
             shape — this refreshes it. A placed symbol whose definition the sheet \
             lacks, such as one repointed at another library, has it embedded \
             (reported in added). New and refreshed definitions use the schematic writer's \
             nested layout. A symbol whose pins moved or disappeared in \
             the library is refused (reported in pins_moved) unless allow_pin_moves \
             is set, because wires and labels attach at pin coordinates.",
            json!({
                "type": "object",
                "properties": {
                    "schematic": { "type": "string", "description": "Path to .kicad_sch file" },
                    "references": {
                        "type": "array",
                        "description": "Component references to update (e.g. ['U1']). Omit to update every symbol in the schematic.",
                        "items": { "type": "string" }
                    },
                    "dry_run": { "type": "boolean", "default": false,
                        "description": "Report what would change without writing." },
                    "allow_pin_moves": { "type": "boolean", "default": false,
                        "description": "Update symbols even when the library moved or removed pins. Wires and labels attached at the old pin positions are NOT moved with them — reconnect them afterwards." }
                },
                "required": ["schematic"]
            }),
            |args, ctx| async move { handle_update_symbols_from_library(args, ctx).await }
        ),
        tool!(
            "reset_schematic_field_positions",
            "Move each placed symbol's Reference and Value text back to the position its \
             library definition anchors them at, carried through the symbol's own rotation \
             — KiCad's 'Reset field text positions'. Use it on a sheet whose fields sit at \
             a uniform offset instead of where the library puts them (labels inside a \
             connector body, a rail's name below an up-pointing arrow). Fields a symbol's \
             definition gives no anchor for are left alone.",
            json!({
                "type": "object",
                "properties": {
                    "schematic": { "type": "string", "description": "Path to .kicad_sch file" },
                    "references": {
                        "type": "array",
                        "description": "Component references to reset (e.g. ['U1']). Omit to reset every symbol in the schematic.",
                        "items": { "type": "string" }
                    },
                    "dry_run": { "type": "boolean", "default": false,
                        "description": "Report what would move without writing." }
                },
                "required": ["schematic"]
            }),
            |args, ctx| async move { handle_reset_schematic_field_positions(args, ctx).await }
        ),
        tool!(
            "get_schematic_view",
            "Render a schematic sheet with kicad-cli and return the path to the SVG it wrote.              There is no PNG: KiCad ships no schematic rasteriser — `sch export` offers no bitmap              format and there is no `sch render` — so this is a vector file, not an image that can              be shown inline. It lands in a temporary directory and is overwritten by the next view              of the same sheet; use export_schematic_svg (toolset sch_export) to choose where it              goes. The SVG doubles as a geometry source: kicad-cli writes every string a second              time as an invisible <text opacity=\"0\"> element carrying x, y, textLength, font-size              and text-anchor, so text content, position and width can be checked without rendering              a pixel.",
            json!({
                "type": "object",
                "properties": {
                    "schematic": { "type": "string" }
                },
                "required": ["schematic"]
            }),
            |args, ctx| async move { handle_get_schematic_view(args, ctx).await }
        ),
    ]
}
