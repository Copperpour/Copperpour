//! Which tools `call_tool` may run.
//!
//! An MCP client asks its user for approval per tool name. `call_tool` runs
//! another tool under its own name, so the client's approval of `call_tool`
//! would otherwise stand in for the approval of every tool behind it. The
//! server therefore decides here, by the target's name and before any part of
//! the target runs, and the decision does not depend on whether the target's
//! toolset is loaded.
//!
//! - A tool in [`READ_ONLY_TOOLS`] or [`READ_ONLY_META_TOOLS`] always runs.
//! - A tool in [`NEVER_THROUGH_CALL_TOOL`] never runs.
//! - Every other tool runs only when the operator set
//!   `call_tool_allow_writes = true` in the server configuration. No tool
//!   argument reaches that setting.
//!
//! # What "read-only" means here
//!
//! A tool is on a read-only list only if no combination of its arguments can
//!
//! - create, change or delete a file that outlives the call: a design,
//!   library or configuration file, an export, a report, a render or a cache
//!   (a temporary file the tool removes itself does not count);
//! - change anything in KiCad over IPC, the selection included;
//! - change server state that a later call observes, the loaded toolsets
//!   included (the call log records every call and is exempt);
//! - send a network request, or start a program that stays running.
//!
//! So a tool that plans by default and applies on request is not read-only,
//! and neither is a check that can write its report to a path the caller
//! names, one that leaves a render behind, or one that runs KiCad's DRC,
//! which puts its report beside the board before removing it. Such a tool is
//! called under its own name.
//!
//! What every tool does as the server's own bookkeeping is not counted, since
//! no argument steers it and no design depends on it: the call log, the empty
//! per-document lock file a consistent read creates in Copperpour's state
//! directory, in-memory result caches, the one-time rename of Copperpour's
//! own legacy directory in the user's home, and the note that a board was
//! seen open in KiCad, which only makes later file fallbacks refuse sooner.
//! A tool that renames a legacy `.konnect` directory inside a project
//! directory the caller names is not read-only.
//!
//! The lists are reviewed by hand. They are not derived from `BoardAccess`,
//! which says how a tool reaches a board and nothing about files,
//! configuration or the network, nor from MCP annotations, which describe a
//! tool to the client and enforce nothing.
//!
//! # Adding a tool
//!
//! `every_registered_tool_is_classified` fails for a tool in neither
//! [`READ_ONLY_TOOLS`] nor [`OTHER_TOOLS`]. Put it in `OTHER_TOOLS` unless you
//! have read its handler against the list above. An unlisted tool is treated
//! as one of `OTHER_TOOLS` at run time, so a missing entry fails closed.

/// Domain tools `call_tool` always runs. See the module documentation for the
/// test a tool has to pass.
pub const READ_ONLY_TOOLS: &[&str] = &[
    // project
    "get_project_info",
    "open_project",
    // editor_navigation
    "get_editor_selection",
    "get_editor_state",
    "resolve_cross_probe_target",
    "resolve_navigation_target",
    // sch_components
    "batch_get_schematic_pin_locations",
    "get_schematic_component",
    "get_schematic_pin_locations",
    "list_schematic_components",
    // sch_analysis
    "check_schematic_overlaps",
    "find_orphan_items",
    "find_shorted_nets",
    "find_single_pin_nets",
    "get_component_nets",
    "get_connected_items",
    "get_net_components",
    "get_net_connections",
    "get_net_connectivity",
    "get_pin_connections",
    "get_pin_net_name",
    "list_schematic_labels",
    "list_schematic_nets",
    "list_schematic_wires",
    "trace_from_point",
    // sch_batch
    "get_schematic_layout",
    "list_schematic_texts",
    "validate_component_connections",
    "validate_wire_connections",
    // sch_export
    "export_netlist_summary",
    // sch_hierarchy
    "get_sheet_hierarchy",
    "validate_sheet_pins",
    // pcb_board
    "get_board_extents",
    "get_board_info",
    "get_board_stackup",
    "get_layer_list",
    "list_zones",
    // pcb_text
    "list_board_texts",
    "list_footprint_fields",
    // pcb_components
    "find_component",
    "get_component_list",
    "get_component_pads",
    "get_pad_position",
    "list_board_footprint_graphics",
    // pcb_routing
    "get_netclasses",
    "get_nets_list",
    "plan_specctra_ses_import",
    "query_traces",
    "query_vias",
    // placement
    "score_placement",
    // library
    "get_footprint_info",
    "get_symbol_info",
    "list_footprint_libraries",
    "list_library_footprints",
    "list_symbol_libraries",
    "list_symbols_in_library",
    "search_footprints",
    "search_symbols",
    // integration
    "get_jlcpcb_database_stats",
    "get_jlcpcb_part",
    "list_jlcpcb_categories",
    "search_jlcpcb_parts",
    "suggest_jlcpcb_alternatives",
    // verification
    "check_clearance",
    "check_kicad_ui",
    "get_design_rules",
    "get_predefined_sizes",
    // design_review
    "audit_connections",
    "audit_decoupling",
    "audit_manufacturing",
    "audit_power_rails",
    "check_bom_health",
    // templates
    "get_template",
    "list_template_categories",
    "search_templates",
    // manufacturing
    "estimate_cost",
];

/// Every other domain tool: `call_tool` runs these only with
/// `call_tool_allow_writes`. Most write; some are here because one of their
/// arguments can, or because nobody has yet read them against the module's
/// list.
pub const OTHER_TOOLS: &[&str] = &[
    "add_board_outline",
    "add_board_text",
    "add_bus",
    "add_bus_entry",
    "add_component_annotation",
    "add_copper_pour",
    "add_design_rule",
    "add_hierarchical_sheet",
    "add_junction",
    "add_layer",
    "add_mounting_hole",
    "add_net",
    "add_no_connect",
    "add_power_symbol",
    "add_schematic_component",
    "add_schematic_connection",
    "add_schematic_net_label",
    "add_schematic_text",
    "add_sheet_pin",
    "add_via",
    "add_wire",
    "add_zone",
    "align_components",
    "annotate_schematic",
    "apply_copper",
    "apply_specctra_ses",
    "apply_template",
    "assign_net_to_class",
    "auto_place_from_schematic",
    "batch_add_bus",
    "batch_add_junction",
    "batch_add_no_connect",
    "batch_add_power_symbol",
    "batch_add_schematic_net_label",
    "batch_add_wire",
    "batch_connect_pins",
    "batch_connect_to_net",
    "batch_delete",
    "batch_delete_no_connect",
    "batch_delete_schematic_components",
    "batch_delete_schematic_wire",
    "batch_edit_schematic_components",
    "batch_place_components",
    "batch_rotate_labels",
    "bulk_move_schematic_components",
    "check_freerouting",
    "compare_visual_baseline",
    "connect_passthrough",
    "connect_pins",
    "connect_pins_to_bus",
    "connect_to_net",
    "copy_routing_pattern",
    "create_footprint",
    "create_netclass",
    "create_project",
    "create_schematic",
    "create_symbol",
    "delete_component",
    "delete_custom_rule",
    "delete_graphics",
    "delete_netclass",
    "delete_no_connect",
    "delete_schematic_component",
    "delete_schematic_net_label",
    "delete_schematic_text",
    "delete_schematic_wire",
    "delete_sheet",
    "delete_sheet_pin",
    "delete_symbol",
    "delete_trace",
    "delete_via",
    "delete_zone",
    "download_jlcpcb_database",
    "duplicate_component",
    "duplicate_sheet",
    "edit_board_footprint_graphic",
    "edit_board_text",
    "edit_component",
    "edit_footprint_field",
    "edit_footprint_pad",
    "edit_schematic_component",
    "edit_schematic_text",
    "edit_sheet",
    "edit_sheet_pin",
    "edit_symbol_pin",
    "enrich_datasheets",
    "export_3d",
    "export_bom",
    "export_dxf",
    "export_gencad",
    "export_gerber",
    "export_ipc2581",
    "export_manufacturing_package",
    "export_netlist",
    "export_odb",
    "export_pdf",
    "export_position_file",
    "export_schematic_pdf",
    "export_schematic_svg",
    "export_specctra_dsn",
    "export_svg",
    "fix_connectivity",
    "flip_component",
    "generate_jlcpcb_order_link",
    "generate_netlist",
    "get_board_2d_view",
    "get_datasheet_url",
    "get_drc_violations",
    "get_effective_config",
    "get_schematic_view",
    "group_components",
    "import_jlcpcb_part",
    "import_sheet_pins",
    "import_svg_logo",
    "launch_kicad_ui",
    "list_design_rules",
    "load_project_config",
    "load_user_config",
    "mirror_schematic_component",
    "modify_trace",
    "move_component",
    "move_connected",
    "move_labels_by_offset",
    "move_region",
    "move_schematic_component",
    "move_sheet",
    "move_via",
    "mutate_editor_selection",
    "open_schematic_viewer",
    "place_component",
    "place_component_array",
    "place_decoupling_caps",
    "plan_bga_fanout",
    "refill_zones",
    "refine_placement_force_directed",
    "register_footprint_library",
    "register_symbol_library",
    "rename_project",
    "rename_schematic_net_label",
    "render_schematic_png",
    "renumber_sheet_pages",
    "repair_corrupted_footprints",
    "repair_instance_metadata",
    "replace_board_outline",
    "replace_component",
    "reset_schematic_field_positions",
    "rotate_component",
    "rotate_schematic_component",
    "rotate_schematic_label",
    "route_differential_pair",
    "route_pad_to_pad",
    "route_specctra_dsn",
    "route_trace",
    "run_design_review",
    "run_drc",
    "run_erc",
    "save_project",
    "save_project_config",
    "save_user_config",
    "set_active_layer",
    "set_board_page",
    "set_board_size",
    "set_board_stackup",
    "set_board_title_block",
    "set_component_placements",
    "set_custom_rule",
    "set_design_rules",
    "set_erc_severity",
    "set_footprint_field",
    "set_footprint_graphics",
    "set_footprint_metadata",
    "set_footprint_models",
    "set_layer_constraints",
    "set_pad_net",
    "set_placed_footprint_models",
    "set_predefined_sizes",
    "set_schematic_page",
    "set_schematic_title_block",
    "set_symbol_properties",
    "set_visual_baseline",
    "snapshot_project",
    "split_wire_at_point",
    "update_footprints_from_library",
    "update_pcb_from_schematic",
    "update_symbols_from_library",
    "validate_for_manufacturing",
];

/// Meta-tools `call_tool` always runs: they report and change nothing.
pub const READ_ONLY_META_TOOLS: &[&str] = &[
    "get_active_toolsets",
    "get_guidance",
    "get_installation_info",
    "get_recent_calls",
    "list_toolboxes",
    "server_stats",
];

/// Meta-tools that change which tools the session lists. They are not
/// read-only, so `call_tool` runs them only with `call_tool_allow_writes`,
/// and then they send `notifications/tools/list_changed` as a direct call
/// does.
pub const SESSION_META_TOOLS: &[&str] = &["load_toolset", "unload_toolset"];

/// Tools `call_tool` never runs, whatever the configuration: itself, since a
/// nested call would only hide its target one level deeper, and
/// `reload_server`, which replaces the serving process and asks for its own
/// explicit confirmation.
pub const NEVER_THROUGH_CALL_TOOL: &[&str] = &["call_tool", "reload_server"];

/// Why `call_tool` does not run a target.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Refusal {
    /// The target is not on a read-only list and the operator has not set
    /// `call_tool_allow_writes`.
    WritesNotEnabled,
    /// The target is in [`NEVER_THROUGH_CALL_TOOL`].
    Never,
}

impl Refusal {
    /// The `reason` of the `tool_not_permitted` error.
    pub fn code(self) -> &'static str {
        match self {
            Self::WritesNotEnabled => "writes_not_enabled",
            Self::Never => "never_through_call_tool",
        }
    }
}

/// Whether `call_tool` refuses `target`, a tool that exists, given the
/// operator's `call_tool_allow_writes`. A name on no list is refused like one
/// of [`OTHER_TOOLS`].
pub fn refusal(target: &str, allow_writes: bool) -> Option<Refusal> {
    if NEVER_THROUGH_CALL_TOOL.contains(&target) {
        return Some(Refusal::Never);
    }
    if allow_writes || READ_ONLY_TOOLS.contains(&target) || READ_ONLY_META_TOOLS.contains(&target) {
        return None;
    }
    Some(Refusal::WritesNotEnabled)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::router::{meta_tools, registry};
    use std::collections::BTreeSet;

    fn registered() -> BTreeSet<&'static str> {
        registry::ALL_TOOLSETS
            .iter()
            .flat_map(|toolset| registry::tools_for(toolset.name).unwrap())
            .map(|tool| tool.name)
            .collect()
    }

    fn set(names: &[&'static str]) -> BTreeSet<&'static str> {
        let unique: BTreeSet<_> = names.iter().copied().collect();
        assert_eq!(unique.len(), names.len(), "a name is listed twice");
        unique
    }

    /// A new tool has to be put on one list by a person before this passes.
    #[test]
    fn every_registered_tool_is_classified() {
        let read_only = set(READ_ONLY_TOOLS);
        let other = set(OTHER_TOOLS);
        let both: Vec<_> = read_only.intersection(&other).collect();
        assert!(both.is_empty(), "on both lists: {both:?}");

        let classified: BTreeSet<_> = read_only.union(&other).copied().collect();
        let unclassified: Vec<_> = registered().difference(&classified).copied().collect();
        assert!(
            unclassified.is_empty(),
            "unclassified tools, see call_tool_policy's module documentation: {unclassified:?}"
        );
    }

    /// A list entry that names no tool would hide a rename. Only a build with
    /// every feature registers every tool.
    #[cfg(all(feature = "jlcpcb", feature = "freerouting"))]
    #[test]
    fn every_classified_name_is_a_registered_tool() {
        let registered = registered();
        let stale: Vec<_> = READ_ONLY_TOOLS
            .iter()
            .chain(OTHER_TOOLS)
            .filter(|name| !registered.contains(*name))
            .collect();
        assert!(stale.is_empty(), "not registered tools: {stale:?}");
    }

    /// Every meta-tool has a policy of its own, `reload_server` included.
    #[test]
    fn every_meta_tool_is_on_exactly_one_list() {
        let lists = [
            set(READ_ONLY_META_TOOLS),
            set(SESSION_META_TOOLS),
            set(NEVER_THROUGH_CALL_TOOL),
        ];
        let listed: Vec<_> = lists.iter().flatten().copied().collect();
        assert_eq!(set(&listed).len(), listed.len());
        let meta: BTreeSet<String> = meta_tools::meta_tool_descriptions_for(true)
            .into_iter()
            .map(|tool| tool.name)
            .collect();
        // `reload_server` is described on Unix only; it is listed everywhere.
        let mut expected: BTreeSet<String> = listed.iter().map(|name| name.to_string()).collect();
        if !cfg!(unix) {
            expected.remove("reload_server");
        }
        assert_eq!(meta, expected);
        let registered = registered();
        assert!(listed.iter().all(|name| !registered.contains(name)));
    }

    #[test]
    fn the_decision_follows_the_lists() {
        for allow_writes in [false, true] {
            assert_eq!(refusal("get_component_nets", allow_writes), None);
            assert_eq!(refusal("server_stats", allow_writes), None);
            assert_eq!(refusal("call_tool", allow_writes), Some(Refusal::Never));
            assert_eq!(refusal("reload_server", allow_writes), Some(Refusal::Never));
        }
        for target in ["add_wire", "load_toolset", "a_tool_added_tomorrow"] {
            assert_eq!(refusal(target, false), Some(Refusal::WritesNotEnabled));
            assert_eq!(refusal(target, true), None);
        }
    }
}
