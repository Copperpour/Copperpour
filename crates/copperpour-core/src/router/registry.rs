//! Static registry mapping toolset names → ToolDef slices.
//!
//! Each toolset module exposes a `tools()` function returning its Vec<ToolDef>.
//! This registry wires them together by name.

use super::ToolsetMeta;
use crate::tools::ToolDef;
use std::collections::HashMap;
use std::sync::OnceLock;

/// Toolsets auto-loaded when the server starts.
///
/// Kept minimal so that baseline `tools/list` context stays small (20 tools
/// including meta-tools ≈ 2K tokens). The LLM expands its toolbelt on demand
/// via `load_toolset(...)`.
///
/// Starter choices:
/// - `project` — needed to open / create / save any project
/// - `config` — user preferences, design rules; call `load_user_config` at session start
pub static STARTER_KIT: &[&str] = &["project", "config"];

pub static ALL_TOOLSETS: &[ToolsetMeta] = &[
    ToolsetMeta {
        name: "project",
        description: "Create, open, save, rename, snapshot KiCAD projects, and launch the live schematic viewer",
        category: "project",
    },
    ToolsetMeta {
        name: "editor_navigation",
        description: "Observe and semantically navigate exact KiCad editor, document, sheet, selection, and cross-probe context",
        category: "project",
    },
    ToolsetMeta {
        name: "sch_components",
        description: "Add, edit, move, rotate, mirror, and delete schematic symbols",
        category: "schematic",
    },
    ToolsetMeta {
        name: "sch_wiring",
        description: "Wires, net labels, power symbols, junctions, no-connects, pin-to-pin connections",
        category: "schematic",
    },
    ToolsetMeta {
        name: "sch_bus",
        description: "Buses, bus entries, and fanning a group of pins out onto a bus",
        category: "schematic",
    },
    ToolsetMeta {
        name: "sch_analysis",
        description: "Net connectivity, pin queries, trace paths, and orphan detection",
        category: "schematic",
    },
    ToolsetMeta {
        name: "sch_readability",
        description: "Check saved schematic sheets for overlaps, touching pins, long or indirect wire runs, and content outside the drawing frame or in the title block; compare layout scores, preview or apply wire-label exchanges, and preview or approve Reference and Value placements",
        category: "schematic",
    },
    ToolsetMeta {
        name: "sch_batch",
        description: "Bulk add, edit, delete, and move schematic elements in one call; add, list, edit and delete free text items; add and list graphic shapes",
        category: "schematic",
    },
    ToolsetMeta {
        name: "sch_export",
        description: "Export schematic to SVG/PDF/PNG/netlist, run ERC and set its checks' severities, and synchronize a live PCB",
        category: "schematic",
    },
    ToolsetMeta {
        name: "sch_hierarchy",
        description: "Sheet files and hierarchical sheets: create a schematic file and set its page size and title block, add/edit/move/delete/duplicate a sheet, hierarchy and page-numbering queries, stale symbol instance record repair, moving circuit objects between sheets, import/add/edit/delete sheet pins, net label renames, pin/label sync validation",
        category: "schematic",
    },
    ToolsetMeta {
        name: "pcb_board",
        description: "Board outline, layers, zones, mounting holes, board text, SVG logo import, custom fields on placed footprints",
        category: "pcb",
    },
    ToolsetMeta {
        name: "pcb_text",
        description: "List and edit placed board text and footprint reference/value fields in place",
        category: "pcb",
    },
    ToolsetMeta {
        name: "pcb_rule_areas",
        description: "Create and manage KiCad rule areas (keep-out zones) on live boards",
        category: "pcb",
    },
    ToolsetMeta {
        name: "pcb_components",
        description: "Place, refresh, move, rotate, flip, align, duplicate and repair PCB footprints; inspect pads; inspect and edit a placed footprint's graphics",
        category: "pcb",
    },
    ToolsetMeta {
        name: "pcb_routing",
        description: "Traces, vias, pad nets, copper pours, net classes, differential pairs, and strict Specctra SES import",
        category: "pcb",
    },
    ToolsetMeta {
        name: "pcb_copper",
        description: "Copper edits on the board open in KiCad: create tracks and vias, batch-move existing vias, move or turn board items and groups as one rigid block, and read or set native pad and via teardrops",
        category: "pcb",
    },
    ToolsetMeta {
        name: "placement",
        description: "Placement quality and automation: score with named deductions, plan decoupling rows and BGA fanouts, first placement, force-directed refinement",
        category: "pcb",
    },
    ToolsetMeta {
        name: "pcb_export",
        description: "Gerber, PDF, SVG, 3D model, BOM, Specctra DSN, pick-and-place, DRC, DXF/GenCAD/IPC-2581/ODB++",
        category: "pcb",
    },
    ToolsetMeta {
        name: "library",
        description: "Search, register, and author symbol and footprint libraries — create symbols and footprints, edit pads, graphics, metadata and 3D models",
        category: "library",
    },
    ToolsetMeta {
        name: "library_maintenance",
        description: "Maintain symbol and footprint libraries as a whole: upgrade a library's file format with kicad-cli, replacing files only when their content is kept, rename library symbols and footprints, change several pins of a library symbol in one write, remove one pin, and edit a library symbol's graphics",
        category: "library",
    },
    ToolsetMeta {
        name: "model_3d",
        description: "Generate approximate STEP package bodies and pin rows without external CAD tools",
        category: "library",
    },
    // A toolset whose tools are all compiled out isn't listed (#210).
    #[cfg(any(feature = "jlcpcb", feature = "freerouting"))]
    ToolsetMeta {
        name: "integration",
        description: INTEGRATION_DESCRIPTION,
        category: "integration",
    },
    ToolsetMeta {
        name: "verification",
        description: "DRC and its checks' severities, design rules, layer constraints, clearance checks, KiCAD UI control (ERC is in sch_export)",
        category: "verification",
    },
    ToolsetMeta {
        name: "config",
        description: "User preferences, project rules, design rules, fab constraints — call load_user_config at session start",
        category: "config",
    },
    ToolsetMeta {
        name: "design_review",
        description: "AI-powered design audits: decoupling, connections, power rails, DFM, BOM health",
        category: "review",
    },
    ToolsetMeta {
        name: "templates",
        description: "Reference circuit library: USB-C, LDO, buck converter, STM32, I2C, LED — verified component values",
        category: "templates",
    },
    ToolsetMeta {
        name: "manufacturing",
        description: "Design-to-fab pipeline: export Gerber+BOM+positions package, validate for fab house, estimate cost",
        category: "manufacturing",
    },
];

/// What this build's `integration` toolset holds.
#[cfg(all(feature = "jlcpcb", feature = "freerouting"))]
const INTEGRATION_DESCRIPTION: &str =
    "JLCPCB parts database, local Freerouting MCP routing, datasheet URLs";
#[cfg(all(feature = "jlcpcb", not(feature = "freerouting")))]
const INTEGRATION_DESCRIPTION: &str = "JLCPCB parts database, datasheet URLs";
#[cfg(all(not(feature = "jlcpcb"), feature = "freerouting"))]
const INTEGRATION_DESCRIPTION: &str = "Local Freerouting MCP routing";

static TOOL_CATALOGUE: OnceLock<HashMap<&'static str, Vec<ToolDef>>> = OnceLock::new();

fn tool_catalogue() -> &'static HashMap<&'static str, Vec<ToolDef>> {
    TOOL_CATALOGUE.get_or_init(|| {
        ALL_TOOLSETS
            .iter()
            .map(|toolset| {
                let tools = build_tools_for(toolset.name)
                    .unwrap_or_else(|| panic!("no tool builder for '{}'", toolset.name));
                (toolset.name, tools)
            })
            .collect()
    })
}

/// Borrow all registered definitions in toolset registration order.
pub(super) fn toolsets_with_tools() -> impl Iterator<Item = (&'static str, &'static [ToolDef])> {
    let catalogue = tool_catalogue();
    ALL_TOOLSETS
        .iter()
        .map(move |toolset| (toolset.name, catalogue[toolset.name].as_slice()))
}

/// Return cached ToolDefs for a given toolset name, or None if unknown.
/// Clones share each definition's compiled input validator through `Arc`.
pub fn tools_for(name: &str) -> Option<Vec<ToolDef>> {
    tool_catalogue().get(name).cloned()
}

/// Number of tools a toolset registers; 0 for an unknown name.
pub fn tool_count(name: &str) -> usize {
    tool_catalogue().get(name).map_or(0, Vec::len)
}

fn build_tools_for(name: &str) -> Option<Vec<ToolDef>> {
    use crate::tools::*;
    match name {
        "project" => Some(project::tools()),
        "editor_navigation" => Some(editor_navigation::tools()),
        "sch_components" => Some(sch_components::tools()),
        "sch_wiring" => Some(sch_wiring::tools()),
        "sch_bus" => Some(sch_bus::tools()),
        "sch_analysis" => Some(sch_analysis::tools()),
        "sch_readability" => Some(sch_readability::tools()),
        "sch_batch" => Some(sch_batch::tools()),
        "sch_export" => Some(sch_export::tools()),
        "sch_hierarchy" => Some(sch_hierarchy::tools()),
        "pcb_board" => Some(pcb_board::tools()),
        "pcb_text" => Some(pcb_text::tools()),
        "pcb_rule_areas" => Some(pcb_board_rule_area::tools()),
        "pcb_components" => Some(pcb_components::tools()),
        "pcb_routing" => Some(pcb_routing::tools()),
        "pcb_copper" => Some(pcb_copper::tools()),
        "placement" => Some(placement::tools()),
        "pcb_export" => Some(pcb_export::tools()),
        "library" => Some(library::tools()),
        "library_maintenance" => Some(
            library_upgrade::tools()
                .into_iter()
                .chain([
                    footprint_rename::tool(),
                    symbol_rename::tool(),
                    symbol_pins::batch_tool(),
                    symbol_pins::delete_tool(),
                    symbol_graphics::tool(),
                ])
                .collect(),
        ),
        "model_3d" => Some(model_3d::tools()),
        #[cfg(any(feature = "jlcpcb", feature = "freerouting"))]
        "integration" => Some(integration::tools()),
        "verification" => Some(verification::tools()),
        "config" => Some(config::tools()),
        "design_review" => Some(design_review::tools()),
        "templates" => Some(templates::tools()),
        "manufacturing" => Some(manufacturing::tools()),
        _ => None,
    }
}
