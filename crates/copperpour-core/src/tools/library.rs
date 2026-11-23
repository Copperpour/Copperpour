//! `library` toolset — create and manage footprints, symbols, and KiCAD library tables.
//!
//! Operations are file-based (S-expression manipulation + directory scanning).
//! No IPC or kicad-cli is required for most tools.

mod footprint_ops;
mod lib_tables;
mod search_and_info;
mod symbol_ops;

#[cfg(test)]
#[path = "create_footprint_kicad10_tests.rs"]
mod create_footprint_kicad10_tests;
#[cfg(test)]
#[path = "library_custom_pad_tests.rs"]
mod custom_pad_tests;
#[cfg(test)]
mod footprint_tests;
#[cfg(test)]
mod lib_table_tests;
#[cfg(test)]
#[path = "symbol_info_pins_tests.rs"]
mod symbol_info_pins_tests;
#[cfg(test)]
mod symbol_tests;

pub(crate) use footprint_ops::{handle_create_footprint, skip_sexp_token, PadFabricationProperty};
use footprint_ops::{handle_edit_footprint_pad, PAD_LAYER_NAMES};
pub(crate) use lib_tables::{
    expand_lib_uri, footprint_lib_nickname_for_dir, handle_register_footprint_library,
    handle_register_symbol_library, is_lib_id, resolve_footprint_path, KiCadSymbolSource,
};
use lib_tables::{handle_list_footprint_libraries, handle_list_symbol_libraries};
#[cfg(test)]
pub(crate) use lib_tables::{project_root_for, quote_lib_table_string};
use search_and_info::{
    handle_get_footprint_info, handle_get_symbol_info, handle_list_library_footprints,
    handle_list_symbols_in_library, handle_search_3d_models, handle_search_footprints,
    handle_search_symbols,
};
pub(crate) use symbol_ops::{
    handle_create_symbol, handle_delete_symbol, off_grid_pin_coordinate_warning,
    ALLOWED_PIN_ELECTRICAL_TYPES,
};

#[cfg(test)]
use crate::mcp::protocol::CallToolResult;
#[cfg(test)]
use crate::router::ToolRouter;
#[cfg(test)]
use crate::tools::{ServerConfig, ToolContext};
#[cfg(test)]
use copperpour_sexp::geometry::round6;
#[cfg(test)]
use copperpour_sexp::parser::{parse_sexp, SexpNode};
#[cfg(test)]
use copperpour_sexp::writer::{
    apply_edits, find_balanced_block, find_block_starts, find_direct_child_blocks, SexpEdit,
};
#[cfg(test)]
use footprint_ops::{
    build_footprint_graphics, silk_pieces, FootprintOutlines, SILK_PAD_CLEARANCE, SILK_WIDTH,
};
#[cfg(test)]
use footprint_ops::{
    build_model_sexp, chamfered_rect_points, courtyard_clearance, edit_footprint_pad_block,
    escape_library_string, format_ratio, kicad_pad_layers, pads_bbox, pin1_index, report_observed,
    set_pad_fabrication_property, set_pad_zone_connect, PadGeom, PadZoneConnect,
};
#[cfg(test)]
use lib_table_tests::{result_text, test_ctx};
#[cfg(test)]
use lib_tables::{
    flatten_lib_table, global_fp_lib_table, global_sym_lib_table, lib_table_target,
    parse_lib_table, persist_lib_table_registration, portable_uri, project_relative_uri,
    project_root_for_from, read_lib_table_checked, register_in_lib_table, repair_table_root,
    table_root_element,
};
#[cfg(test)]
use search_and_info::{
    collect_3d_models_in_dir, duplicate_pin_warnings, is_3d_model_file, resolve_symbol_pins,
    search_footprint_entries, search_lib_symbols, search_response, search_symbol_entries,
    stacked_pin_count, top_level_symbol_names, SymbolPin,
};
#[cfg(test)]
use std::path::{Path, PathBuf};
#[cfg(test)]
use std::sync::Arc;
#[cfg(test)]
use symbol_ops::{
    build_symbol_unit, graphics_primitive_keys, grow_to, is_on_symbol_grid, names_span,
    pin_name_width, pin_root, symbol_body_rect, Axis, PinGeom, PIN_NAME_GAP, PIN_NAME_OFFSET,
    PIN_TEXT, SYMBOL_GRID,
};

use crate::tool;
use crate::tools::{solder_margins, ToolDef};
use serde_json::json;

// ─── Tool definitions ─────────────────────────────────────────────────────────

fn footprint_drill_schema() -> serde_json::Value {
    json!({
        "description": "Hole in pad-local millimetres: a circular diameter or an oval width/height. Pad rotation rotates the hole too.",
        "oneOf": [
            {"type": "number", "exclusiveMinimum": 0},
            {
                "type": "object", "additionalProperties": false,
                "properties": {
                    "shape": {"const": "oval"},
                    "width": {"type": "number", "exclusiveMinimum": 0},
                    "height": {"type": "number", "exclusiveMinimum": 0}
                },
                "required": ["shape", "width", "height"]
            }
        ]
    })
}

fn footprint_drill_sexp(
    value: &serde_json::Value,
    pad_type: &str,
    pad_width: f64,
    pad_height: f64,
) -> Result<String, String> {
    if !matches!(pad_type, "thru_hole" | "np_thru_hole") {
        return Err("drill requires a thru_hole or np_thru_hole pad".into());
    }
    let (width, height, oval) = if let Some(diameter) = value.as_f64() {
        (diameter, diameter, false)
    } else if let Some(object) = value.as_object() {
        if object.len() != 3 || object.get("shape").and_then(|s| s.as_str()) != Some("oval") {
            return Err("oval drill requires exactly shape, width, and height".into());
        }
        let width = value["width"]
            .as_f64()
            .ok_or("drill width must be a number")?;
        let height = value["height"]
            .as_f64()
            .ok_or("drill height must be a number")?;
        (width, height, true)
    } else {
        return Err("drill must be a positive diameter or an oval object".into());
    };
    if !width.is_finite() || !height.is_finite() || width <= 0.0 || height <= 0.0 {
        return Err("drill dimensions must be finite and positive".into());
    }
    if width > pad_width || height > pad_height {
        return Err("drill dimensions must fit within the pad size".into());
    }
    Ok(if oval {
        format!("(drill oval {width} {height})")
    } else {
        format!("(drill {width})")
    })
}

/// The pin-item object schema (number/name/type/style/x/y/angle/length) shared
/// by `pins`, `units[].pins`, and `power_pins` in the create_symbol schema
/// below. `type_desc` parameterizes the one wording difference between call
/// sites. `require_xy` is false where a `glyph` may auto-place the pins (so x/y
/// become optional) and true for the always-rectangular `power_pins`.
/// The drawing primitives a symbol unit may carry.
///
/// Deliberately the same vocabulary `set_footprint_graphics` already defines —
/// `line`, `arc`, `rect`, `circle`, `poly`, points as `{x, y}`,
/// `stroke_width_mm` — built from the same code so the two cannot drift (#501).
/// Only `fill` differs: a footprint fills or it does not, while a symbol also
/// has KiCad's pale `background` body fill.
fn symbol_graphics_schema() -> serde_json::Value {
    let mut schema = crate::tools::footprint_graphics::graphics_schema_with_fill(&[
        "none",
        "outline",
        "background",
    ]);
    schema["description"] = json!(
        "Draw this body yourself instead of using a glyph. Same primitive vocabulary as \
         set_footprint_graphics (line, arc, rect, circle, poly; points as {x, y}; \
         stroke_width_mm), in symbol-local millimetres. `fill` is KiCad's symbol \
         vocabulary: 'none', 'outline' (the shape's own colour) or 'background' (the pale \
         body fill). When given, the automatic body rectangle is not drawn, and pin x/y are \
         written exactly as supplied rather than slid out to a computed body edge. At the top \
         level this draws the single-unit `pins` body; with `units` it belongs on the unit, and \
         passing it at the top level alongside `units` is refused rather than dropped."
    );
    schema
}

fn pin_item_schema(type_desc: &str, require_xy: bool) -> serde_json::Value {
    let mut required = vec!["number", "name", "type"];
    if require_xy {
        required.push("x");
        required.push("y");
    }
    json!({
        "type": "object",
        "properties": {
            "number": { "type": "string" },
            "name": { "type": "string" },
            "type": {
                "type": "string",
                "enum": ["input", "output", "bidirectional", "tri_state", "passive", "free", "unspecified", "power_in", "power_out", "open_collector", "open_emitter", "no_connect"],
                "description": type_desc
            },
            "style": {
                "type": "string",
                "enum": ["line", "inverted", "clock", "inverted_clock", "input_low", "clock_low", "output_low", "edge_clock_high", "non_logic"],
                "description": "Pin graphic style (default 'line'). 'inverted' = active-low bubble, 'clock' = clock input, etc. Works with any body shape."
            },
            "x": {
                "type": "number",
                "description": "Starting position, not a fixed one. For a rectangle body the box is sized to fit the pin names and every pin whose angle names an edge is then aligned to that edge, so a pin can end up further out than requested. A `glyph` body ignores x/y entirely. The response reports where each pin actually ended up under `units[].pins`, with `requested` alongside whenever it differs."
            },
            "y": {
                "type": "number",
                "description": "Starting position, not a fixed one — see `x`."
            },
            "angle": { "type": "number", "default": 0 },
            "length": { "type": "number", "default": 2.54 },
            "hide": {
                "type": "boolean",
                "default": false,
                "description": "true writes KiCad's `(hide yes)`: the pin is not drawn but still connects at its position. Stack hidden pins under one visible pin at the same x/y to tie many pads (e.g. a connector's grounds) to a single connection point."
            }
        },
        "required": required
    })
}

pub fn tools() -> Vec<ToolDef> {
    let mut tools = vec![
        tool!(
            "create_footprint",
            "Create a new footprint (.kicad_mod) file from a pad layout description.",
            json!({
                "type": "object",
                "properties": {
                    "output": { "type": "string", "description": "Output .kicad_mod file path" },
                    "name": { "type": "string", "description": "Footprint name" },
                    "description": { "type": "string", "description": "Footprint description (optional)" },
                    "pads": {
                        "type": "array",
                        "description": "Pad definitions",
                        "items": {
                            "type": "object",
                            "properties": {
                                "number": { "type": "string" },
                                "type": { "type": "string", "description": "'smd', 'thru_hole', 'np_thru_hole'" },
                                "shape": { "type": "string", "description": "'rect', 'oval', 'circle', 'roundrect', or 'custom' with custom_shape" },
                                "x": { "type": "number" },
                                "y": { "type": "number" },
                                "width": { "type": "number" },
                                "height": { "type": "number" },
                                "drill": footprint_drill_schema(),
                                "layers": {
                                    "type": "array",
                                    "items": { "type": "string" },
                                    "description": "Explicit canonical KiCAD pad layers. Defaults to F.Cu/F.Paste/F.Mask for SMD and *.Cu/*.Mask otherwise."
                                },
                                "rotation": {
                                    "type": "number",
                                    "description": "Pad rotation in degrees (default 0)"
                                },
                                "roundrect_rratio": {
                                    "type": "number",
                                    "minimum": 0,
                                    "maximum": 0.5,
                                    "description": "Rounded-rectangle corner radius ratio (0..0.5)"
                                },
                                "custom_shape": super::pad_primitives::custom_shape_schema(),
                                "fabrication_property": {
                                    "type": "string",
                                    "enum": PadFabricationProperty::names(),
                                    "description": "KiCad's pad fabrication property (default 'none'), e.g. 'castellated' for a plated half-hole edge pad. A property KiCad flags for the pad type is still written, with KiCad's warning in 'warnings'."
                                }
                            },
                            "required": ["number", "type", "shape", "x", "y", "width", "height"]
                        }
                    },
                    "body_width": { "type": "number", "description": "The component body's size along X in mm (optional, with body_height). With a body, the F.Fab outline is the body, the courtyard goes around the pads and the body, and the silk runs 0.11 mm outside the body, broken near pads. Without one, the outlines follow the pad envelope." },
                    "body_height": { "type": "number", "description": "The component body's size along Y in mm (optional, with body_width), as seen from above; not the 3D height." },
                    "package_type": { "type": "string", "description": "'smd' (0.25mm courtyard), 'through_hole' (0.5mm), 'small' (0.15mm, <0603), or 'bga' (1.0mm). Sets courtyard clearance when courtyard_clearance is not given." },
                    "courtyard_clearance": { "type": "number", "description": "Explicit courtyard clearance in mm (overrides package_type / auto-detection)." },
                    "model": {
                        "type": "object",
                        "description": "Optional 3D model to associate with the footprint.",
                        "properties": {
                            "path": { "type": "string", "description": "Path to the 3D model file (.step/.wrl); absolute or a KiCAD env-var path like ${KICAD9_3DMODEL_DIR}/..." },
                            "offset": { "type": "object", "properties": { "x": { "type": "number" }, "y": { "type": "number" }, "z": { "type": "number" } }, "description": "{x,y,z} in mm (default 0,0,0)" },
                            "scale": { "type": "object", "properties": { "x": { "type": "number" }, "y": { "type": "number" }, "z": { "type": "number" } }, "description": "{x,y,z} (default 1,1,1)" },
                            "rotate": { "type": "object", "properties": { "x": { "type": "number" }, "y": { "type": "number" }, "z": { "type": "number" } }, "description": "{x,y,z} in degrees (default 0,0,0)" }
                        },
                        "required": ["path"]
                    },
                    "replace_existing": {
                        "type": "boolean",
                        "description": "Replace an existing .kicad_mod file at output instead of refusing with conflict",
                        "default": false
                    }
                },
                "required": ["output", "name", "pads"]
            }),
            |args, ctx| async move { handle_create_footprint(args, ctx).await }
        ),
        tool!(
            "edit_footprint_pad",
            "Edit the size, shape, corner ratio, custom shape, position, drill, layers, zone connection, fabrication property, or solder mask and paste margins of a pad in an existing .kicad_mod footprint file. Each margin, the corner ratio and the layers are reported with their previous and current value; a custom shape is reported as read back from the pad as written.",
            json!({
                "type": "object",
                "properties": {
                    "footprint_path": { "type": "string", "description": "Path to .kicad_mod file" },
                    "pad_number": { "type": "string", "description": "Pad number to edit" },
                    "new_number": { "type": "string", "description": "New pad number (optional; duplicate destination numbers are allowed)" },
                    "match_all": { "type": "boolean", "description": "Edit every direct-child pad with pad_number instead of only the first match", "default": false },
                    "x": { "type": "number", "description": "New X position in mm (optional)" },
                    "y": { "type": "number", "description": "New Y position in mm (optional)" },
                    "width": { "type": "number", "description": "New pad width in mm (optional)" },
                    "height": { "type": "number", "description": "New pad height in mm (optional)" },
                    "shape": {
                        "type": "string",
                        "enum": ["circle", "rect", "oval", "roundrect", "custom"],
                        "description": "New pad shape (optional). roundrect gets a valid default corner ratio when needed. custom needs custom_shape. Changing a custom pad to another shape removes its primitives."
                    },
                    "roundrect_rratio": {
                        "type": "number",
                        "minimum": 0,
                        "maximum": 0.5,
                        "description": "Corner radius as a fraction of the pad's smaller side (0..0.5), as KiCad's roundrect_rratio (optional). Only for a roundrect pad, or with shape roundrect in the same call."
                    },
                    "custom_shape": super::pad_primitives::custom_shape_schema(),
                    "drill": footprint_drill_schema(),
                    "layers": {
                        "type": "array",
                        "items": { "type": "string", "enum": PAD_LAYER_NAMES },
                        "minItems": 1,
                        "description": "Replacement pad layers (optional), the names create_footprint accepts, for example [\"F.Cu\", \"F.Mask\", \"F.Paste\"] for an SMD pad with its stencil opening. Written as KiCad writes them: *.Cu for front and back copper, *.Mask for both masks, then the rest in KiCad's layer order."
                    },
                    "zone_connect": {
                        "type": "string",
                        "enum": ["inherited", "solid", "thermal", "none"],
                        "description": "How copper zones connect to this pad (optional; omit it to leave the pad as it is). 'solid', 'thermal' and 'none' write the pad's own override; 'inherited' removes it so the pad follows its footprint and zone settings."
                    },
                    "fabrication_property": {
                        "type": "string",
                        "enum": PadFabricationProperty::names(),
                        "description": "KiCad's pad fabrication property (optional; omit it to leave the pad as it is). KiCad's DRC skips the copper-to-edge check for a 'castellated' pad; 'none' removes the property. A property KiCad flags for this pad type is still written, with KiCad's warning in 'warnings'."
                    },
                    "solder_mask_margin": solder_margins::Margin::Mask.schema("pad"),
                    "solder_paste_margin": solder_margins::Margin::Paste.schema("pad"),
                    "solder_paste_margin_ratio": solder_margins::Margin::PasteRatio.schema("pad")
                },
                "required": ["footprint_path", "pad_number"]
            }),
            |args, ctx| async move { handle_edit_footprint_pad(args, ctx).await }
        ),
        tool!(
            "register_footprint_library",
            "Register a local footprint library directory in the KiCAD global or project library table.",
            json!({
                "type": "object",
                "properties": {
                    "library_path": { "type": "string", "description": "Path to .pretty directory" },
                    "nickname": { "type": "string", "description": "Library nickname" },
                    "scope": {
                        "type": "string",
                        "description": "Scope: 'global' or 'project'",
                        "default": "project"
                    },
                    "project": { "type": "string", "description": "Path to the .kicad_pro file, or to the project directory that holds it (required for project scope)" },
                    "replace_existing": {
                        "type": "boolean",
                        "description": "Replace the URI of an existing nickname instead of leaving it unchanged",
                        "default": false
                    }
                },
                "required": ["library_path", "nickname"]
            }),
            |args, ctx| async move { handle_register_footprint_library(args, ctx).await }
        ),
        tool!(
            "list_footprint_libraries",
            "List all registered footprint libraries (global and optionally project-level).",
            json!({
                "type": "object",
                "properties": {
                    "project": { "type": "string", "description": "Path to .kicad_pro to include project libraries (optional)" },
                    "scope": {
                        "type": "string",
                        "description": "Scope: 'global', 'project', or 'all'",
                        "default": "all"
                    }
                },
                "required": []
            }),
            |args, ctx| async move { handle_list_footprint_libraries(args, ctx).await }
        ),
        tool!(
            "create_symbol",
            "Create a new KiCAD schematic symbol and append it to a .kicad_sym library file. \
             Supports single-unit symbols (via `pins`) and multi-unit parts like dual/quad \
             op-amps or gate banks (via `units` + optional `power_pins`). By default each unit \
             gets a rectangular body sized to its pins. Omitted edge-axis coordinates are \
             distributed at 2.54 mm pitch on the angle-selected edge (default left); supplied \
             coordinates retain the existing body-edge alignment. Pins whose resolved coordinates \
             fall off the 1.27 mm schematic grid are still written, with a warning in `warnings`. \
             Set `glyph` (symbol-level and/or per \
             unit) to draw a conventional op-amp triangle or logic-gate body instead. With a \
             glyph, pins auto-place by their `type` (inputs left in the order listed top-to- \
             bottom, output right, power top/bottom) and their x/y are ignored.",
            super::symbol_fields::with_field_schema(json!({
                "type": "object",
                "properties": {
                    "library_path": { "type": "string", "description": "Path to .kicad_sym library file" },
                    "name": { "type": "string", "description": "Symbol name; refused if the library already defines it" },
                    "reference_prefix": { "type": "string", "description": "Default reference prefix (e.g. 'U')" },
                    "value": { "type": "string", "description": "Default value string" },
                    "datasheet": { "type": "string", "description": "Datasheet URL or path (default empty). A '~' is written as an empty string — carrying it fails ERC's library-match check." },
                    "glyph": {
                        "type": "string",
                        "enum": ["rectangle", "opamp", "buffer", "inverter", "schmitt", "schmitt_inverter", "and", "nand", "or", "nor", "xor", "xnor"],
                        "description": "Symbol-level default body shape. 'rectangle' (default) uses the pin x/y you supply; the others draw a fixed conventional shape and auto-place pins by type (x/y ignored). Op-amp/gate inputs are placed in the order listed, top-to-bottom (KiCAD's op-amp convention is + on top, - on bottom). Inverting glyphs (inverter/schmitt_inverter/nand/nor/xnor) draw the same body as their base and put the inversion bubble on the output pin. If a glyph's pins don't fit (wrong input count, not exactly one output), it falls back to a rectangle and reports a warning."
                    },
                    "pins": {
                        "type": "array",
                        "description": "Pin definitions. x/y size and position the rectangle body rather than fixing the pins: see the per-pin `x`. They are ignored entirely when a `glyph` is set.",
                        "items": pin_item_schema("Pin electrical type — exactly one of KiCAD's 12 values. Note: NC pins are 'no_connect' (not 'not_connected').", false)
                    },
                    "show_pin_names": { "type": "boolean", "description": "Show pin names on the symbol (default true).", "default": true },
                    "show_pin_numbers": { "type": "boolean", "description": "Show pin numbers on the symbol (default true).", "default": true },
                    "graphics": symbol_graphics_schema(),
                    "units": {
                        "type": "array",
                        "description": "For MULTI-UNIT parts (dual/quad op-amps, gate banks, multi-bank connectors). Each element is one unit (becomes Unit A, B, C...) with its own pins and body. When given, `units` replaces `pins` (use `pins` for single-unit symbols instead). Each unit may set its own `glyph`, overriding the symbol-level default.",
                        "items": {
                            "type": "object",
                            "properties": {
                                "glyph": {
                                    "type": "string",
                                    "enum": ["rectangle", "opamp", "buffer", "inverter", "schmitt", "schmitt_inverter", "and", "nand", "or", "nor", "xor", "xnor"],
                                    "description": "Body shape for this unit, overriding the symbol-level `glyph`."
                                },
                                "pins": {
                                    "type": "array",
                                    "description": "Pins for this unit. x/y are ignored when the unit has a `glyph`.",
                                    "items": pin_item_schema("Pin electrical type — exactly one of KiCAD's 12 values. Note: NC pins are 'no_connect' (not 'not_connected').", false)
                                },
                                "graphics": symbol_graphics_schema()
                            },
                            "required": ["pins"]
                        }
                    },
                    "power_pins": {
                        "type": "array",
                        "description": "Shared power pins (V+/V-, VCC/GND). Only meaningful with `units`: they become a dedicated final 'power unit' (e.g. Unit C of a dual op-amp, Unit E of a quad gate) placed once, following KiCAD's own 74xx convention. This avoids drawing the power pins on every unit (which would each need wiring to pass ERC). The power unit is always a rectangle.",
                        "items": pin_item_schema("Pin electrical type — exactly one of KiCAD's 12 values (power pins are usually 'power_in'). Note: NC pins are 'no_connect' (not 'not_connected').", true)
                    }
                },
                "required": ["library_path", "name", "reference_prefix"]
            })),
            |args, ctx| async move { handle_create_symbol(args, ctx).await }
        ),
        tool!(
            "delete_symbol",
            "Delete a symbol definition from a .kicad_sym library file.",
            json!({
                "type": "object",
                "properties": {
                    "library_path": { "type": "string", "description": "Path to .kicad_sym library file" },
                    "symbol_name": { "type": "string", "description": "Name of the symbol to delete" }
                },
                "required": ["library_path", "symbol_name"]
            }),
            |args, ctx| async move { handle_delete_symbol(args, ctx).await }
        ),
        super::symbol_fields::tool(),
        super::symbol_pins::tool(),
        tool!(
            "list_symbols_in_library",
            "List all symbol names defined in a .kicad_sym library file.",
            json!({
                "type": "object",
                "properties": {
                    "library_path": { "type": "string", "description": "Path to .kicad_sym library file" },
                    "limit": { "type": "integer", "minimum": 0, "description": "Maximum symbols to display (0 for count only). count is the full library size; shown/truncated describe the displayed list.", "default": 100 }
                },
                "required": ["library_path"]
            }),
            |args, ctx| async move { handle_list_symbols_in_library(args, ctx).await }
        ),
        tool!(
            "register_symbol_library",
            "Register a .kicad_sym library file in the KiCAD global or project symbol table. \
             Reports whether the entry was inserted, left unchanged, or updated — an existing \
             nickname is kept as-is unless replace_existing is set.",
            json!({
                "type": "object",
                "properties": {
                    "library_path": { "type": "string", "description": "Path to .kicad_sym file" },
                    "nickname": { "type": "string", "description": "Library nickname" },
                    "scope": {
                        "type": "string",
                        "description": "Scope: 'global' or 'project'",
                        "default": "project"
                    },
                    "project": { "type": "string", "description": "Path to the .kicad_pro file, or to the project directory that holds it (required for project scope)" },
                    "replace_existing": {
                        "type": "boolean",
                        "description": "Replace the URI of an existing nickname instead of leaving it unchanged",
                        "default": false
                    }
                },
                "required": ["library_path", "nickname"]
            }),
            |args, ctx| async move { handle_register_symbol_library(args, ctx).await }
        ),
        tool!(
            "list_symbol_libraries",
            "List all registered symbol libraries (global and optionally project-level).",
            json!({
                "type": "object",
                "properties": {
                    "project": { "type": "string", "description": "Path to .kicad_pro to include project libraries (optional)" },
                    "scope": {
                        "type": "string",
                        "description": "Scope: 'global', 'project', or 'all'",
                        "default": "all"
                    }
                },
                "required": []
            }),
            |args, ctx| async move { handle_list_symbol_libraries(args, ctx).await }
        ),
        tool!(
            "search_symbols",
            "Search for symbols across all registered libraries by name or keyword.",
            json!({
                "type": "object",
                "properties": {
                    "query": { "type": "string", "description": "Search string (partial name or keyword match)" },
                    "limit": { "type": "integer", "minimum": 0, "description": "Maximum matches to display (0 for existence only). count/shown are returned matches; truncated means another match was found, not a full catalogue total.", "default": 50 },
                    "project_dir": { "type": "string", "description": "Project directory whose sym-lib-table is also searched. Defaults to the configured project_dir." }
                },
                "required": ["query"]
            }),
            |args, ctx| async move { handle_search_symbols(args, ctx).await }
        ),
        tool!(
            "list_library_footprints",
            "List all footprints in a specific registered footprint library (.pretty directory).",
            json!({
                "type": "object",
                "properties": {
                    "library_path": { "type": "string", "description": "Path to .pretty directory (or nickname to look up)" }
                },
                "required": ["library_path"]
            }),
            |args, ctx| async move { handle_list_library_footprints(args, ctx).await }
        ),
        tool!(
            "get_footprint_info",
            "Return detailed information about a footprint: pad layout, courtyard, description, \
             and optionally its supported graphical primitives.",
            json!({
                "type": "object",
                "properties": {
                    "footprint_path": { "type": "string", "description": "Path to .kicad_mod file, OR 'Library:Footprint' identifier" },
                    "project": { "type": "string", "description": "Path to a .kicad_pro used to resolve project libraries (optional)" },
                    "include_pads": { "type": "boolean", "description": "Include typed pad geometry in footprint-local coordinates. A custom pad also gets custom_shape (anchor, clearance and primitives, the form edit_footprint_pad takes), or custom_shape_unreadable with the reason.", "default": false },
                    "include_graphics": { "type": "boolean", "description": "Include supported top-level footprint graphics in the response", "default": false },
                    "graphics_layer": { "type": "string", "description": "Return graphics only from this canonical KiCad layer; implies include_graphics" }
                },
                "required": ["footprint_path"]
            }),
            |args, ctx| async move { handle_get_footprint_info(args, ctx).await }
        ),
        tool!(
            "search_footprints",
            "Search for footprints across all registered libraries by name or keyword.",
            json!({
                "type": "object",
                "properties": {
                    "query": { "type": "string", "description": "Search string (partial name or keyword)" },
                    "limit": { "type": "integer", "minimum": 0, "description": "Maximum matches to display (0 for existence only). count/shown are returned matches; truncated means another match was found, not a full catalogue total.", "default": 50 }
                },
                "required": ["query"]
            }),
            |args, ctx| async move { handle_search_footprints(args, ctx).await }
        ),
        tool!(
            "search_3d_models",
            "Search for 3D model files (.step, .stp, .stpz, .stp.gz, .step.gz, .igs, .iges, .wrl, .wrz, .x3d, .idf, .emn) in KiCad's bundled 3dmodels directory and/or the project's 3dmodels/ folder.",
            json!({
                "type": "object",
                "properties": {
                    "query": { "type": "string", "description": "Case-insensitive substring to match against the model path or filename (e.g. 'SOIC-8', 'C_0805', 'USB_C')" },
                    "project_dir": { "type": "string", "description": "Project directory containing a 3dmodels/ folder. Defaults to the configured project_dir when omitted." },
                    "scope": { "type": "string", "enum": ["all", "kicad", "project"], "description": "Where to search: 'all' (default), 'kicad' (bundled KiCad 3dmodels), or 'project' (<project_dir>/3dmodels)", "default": "all" },
                    "limit": { "type": "integer", "minimum": 0, "description": "Maximum matches to display (0 for existence only). count/shown are returned matches; truncated means another match was found, not a full catalogue total.", "default": 50 }
                },
                "required": ["query"]
            }),
            |args, ctx| async move { handle_search_3d_models(args, ctx).await }
        ),
        tool!(
            "get_symbol_info",
            "Return detailed information about a schematic symbol: pins, properties, description. \
             Every pin is listed with its unit and body_style, as edit_symbol_pin chooses pins; a \
             derived symbol shows its root's pins. pin_count is KiCad's (every unit, body style \
             1, a stacked number once per pin), and warnings name the duplicate pins KiCad's \
             symbol checker reports. Name it by lib_id through the symbol library tables, or by \
             library_path and symbol_name in a .kicad_sym file, registered or not.",
            json!({
                "type": "object",
                "properties": {
                    "lib_id": { "type": "string", "description": "Library:Symbol identifier (e.g. 'Device:R'), resolved through the project and global symbol library tables. Not with library_path." },
                    "project_dir": { "type": "string", "description": "Project directory to resolve project-scoped libraries for lib_id. Defaults to the configured project_dir. Not with library_path." },
                    "library_path": { "type": "string", "description": "Path to a .kicad_sym library file, read directly whether or not it is registered. Takes symbol_name; not with lib_id." },
                    "symbol_name": { "type": "string", "description": "Name of the symbol in library_path" }
                }
            }),
            |args, ctx| async move { handle_get_symbol_info(args, ctx).await }
        ),
    ];
    // Grouped after `create_footprint` so the footprint-editing tools read as
    // one family in `tools/list`. Anchored on that tool's name rather than a
    // literal index: the list above is edited often, and a positional insert
    // silently reorders the catalogue the first time a tool moves.
    let after_create_footprint = tools
        .iter()
        .position(|t| t.name == "create_footprint")
        .map(|i| i + 1)
        .unwrap_or(tools.len());
    for (offset, tool) in [
        super::footprint_graphics::tool(),
        super::footprint_metadata::tool(),
        super::footprint_models::tool(),
    ]
    .into_iter()
    .enumerate()
    {
        tools.insert(after_create_footprint + offset, tool);
    }
    tools
}
