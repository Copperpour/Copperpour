//! Drawing and layout checks and preview-first edits of saved schematic sheets.

mod connector_pin_signals;

use super::{
    sch_analysis::handle_check_overlaps, sch_layout_score::handle_compare_schematic_layout,
    sch_page_fit, sch_pin_contacts, sch_placement_check, sch_wire_runs, ToolDef,
};
use crate::tool;
use serde_json::json;

pub fn tools() -> Vec<ToolDef> {
    vec![
        connector_pin_signals::tool(),
        overlap_tool_def(),
        sch_page_fit::tool_def(),
        sch_wire_runs::tool_def(),
        compare_tool_def(),
        sch_wire_runs::label_edit_tool_def(),
        sch_pin_contacts::definition(),
        super::sch_cleanup::definition(),
        super::sch_block_placement::definition(),
        sch_placement_check::definition(),
        super::sch_split_plan::tool_def(),
        super::sch_field_autoplace::definition(),
    ]
}

fn overlap_tool_def() -> ToolDef {
    tool!(
        "check_schematic_overlaps",
        "Find overlapping symbols from their transformed drawings and pins (excluding free \
             text): shared body area, a pin running into another symbol's body, or pins \
             meeting anywhere but tip to tip. Pin-tip contact and touching edges are not \
             reported. Also finds \
             conflicting labels at the same location, colliding label and field text \
             (`text_overlap`), wires drawn through a symbol's body (`wire_overlap`), \
             wires crossing label text or visible power symbol Value text (`wire_label_overlap`), \
             wires crossing visible Reference or Value text on ordinary symbols \
             (`wire_field_overlap`), and wires crossing a drawn pin name or number \
             (`wire_pin_text_overlap`). \
             Reports any symbol whose embedded \
             geometry could not be resolved and used the origin fallback instead, and in \
             `pin_text_unresolved` any symbol whose pin text could not be measured.",
        json!({ "type": "object",
                "properties": {
                    "schematic": { "type": "string" },
                    "tolerance": {
                        "type": "number",
                        "description": "Coordinate tolerance in mm for label collisions and the origin fallback when library geometry is unavailable",
                        "default": 0.5
                    }
                },
                "required": ["schematic"] }),
        |args, ctx| async move { handle_check_overlaps(args, ctx).await }
    )
}

pub(super) fn compare_tool_def() -> ToolDef {
    tool!(
        "compare_schematic_layout",
        "Score a saved schematic sheet, optionally comparing it with a saved baseline. \
         Counts wires crossing visible Reference or Value text on ordinary symbols \
         and wires crossing drawn pin names or numbers. \
         Before ranking layout changes, check pin connectivity, primary net names, \
         scoped aliases, symbol identity, saved PCB/BOM fields and flags (including \
         variants and child-sheet instances), and embedded library identity, jumpers, \
         fields, units, and pins. Embedded Description, ki_keywords, and ki_fp_filters \
         are not compared (https://github.com/Copperpour/Copperpour/issues/938). \
         Refuse different schematic format versions when symbols are retained. \
         Compare saved rule-area flags, Component Classes, and inputs to exported text \
         expressions. Refuse changes that could affect unresolved exports or Netclass assignments. \
         Use verdict for decisions; reasons are human-readable diagnostics. \
         No writes, editor access, or ERC.",
        json!({ "type": "object",
            "properties": {
                "schematic": { "type": "string", "description": "Path to the saved .kicad_sch sheet" },
                "baseline": { "type": ["string", "null"], "description": "Path to an earlier saved copy of the same sheet" },
                "removed_power_symbol_uuids": {
                    "type": ["array", "null"],
                    "items": { "type": "string" },
                    "description": "UUIDs of power symbols intentionally removed from the baseline"
                }
            },
            "required": ["schematic"] }),
        |args, ctx| async move { handle_compare_schematic_layout(args, ctx).await }
    )
}

#[cfg(test)]
#[path = "sch_overlap_tests.rs"]
mod overlap_tests;

#[cfg(test)]
#[path = "wire_pin_text_tests.rs"]
mod wire_pin_text_tests;

#[cfg(test)]
async fn request(
    handler: &crate::mcp::handler::McpHandler,
    method: &str,
    params: serde_json::Value,
) -> serde_json::Value {
    handler
        .handle_message(json!({
            "jsonrpc": "2.0", "id": 1, "method": method, "params": params
        }))
        .await
        .unwrap()
        .result
        .unwrap()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{mcp::handler::McpHandler, tools::ServerConfig};

    async fn assert_readability_route(name: &str) {
        let handler = McpHandler::new(ServerConfig::default()).await.unwrap();
        request(
            &handler,
            "tools/call",
            json!({
                "name": "load_toolset", "arguments": {"name": "sch_analysis"}
            }),
        )
        .await;
        let call = json!({"name": name, "arguments": {}});
        let result = request(&handler, "tools/call", call.clone()).await;
        assert_eq!(result["isError"], true);
        let body: serde_json::Value =
            serde_json::from_str(result["content"][0]["text"].as_str().unwrap()).unwrap();
        assert_eq!(body["error"]["kind"], "toolset_not_loaded", "{body}");
        assert_eq!(body["error"]["toolset"], "sch_readability", "{body}");
        request(
            &handler,
            "tools/call",
            json!({
                "name": "load_toolset", "arguments": {"name": "sch_readability"}
            }),
        )
        .await;
        let loaded = request(&handler, "tools/list", json!({})).await;
        assert!(loaded["tools"]
            .as_array()
            .unwrap()
            .iter()
            .any(|tool| tool["name"] == name));
    }

    #[tokio::test]
    async fn overlap_recovery_names_the_new_toolset() {
        assert!(crate::tools::sch_analysis::tools()
            .iter()
            .all(|tool| tool.name != "check_schematic_overlaps"));
        assert_readability_route("check_schematic_overlaps").await;
    }

    #[tokio::test]
    async fn connector_signals_belong_to_readability() {
        assert_readability_route("check_connector_pin_signals").await;
    }

    #[tokio::test]
    async fn comparison_belongs_to_readability() {
        assert_readability_route("compare_schematic_layout").await;
    }

    #[tokio::test]
    async fn pin_contacts_belong_to_readability() {
        assert_readability_route("find_pin_contacts").await;
    }

    #[tokio::test]
    async fn wire_runs_belong_to_readability() {
        assert_readability_route("check_schematic_wire_runs").await;
    }

    #[tokio::test]
    async fn cleanup_belongs_to_readability() {
        assert!(crate::tools::sch_analysis::tools()
            .iter()
            .all(|tool| tool.name != "plan_schematic_cleanup"));
        assert_readability_route("plan_schematic_cleanup").await;
    }

    #[tokio::test]
    async fn page_fit_belongs_to_readability() {
        assert_readability_route("check_schematic_page_fit").await;
    }

    #[tokio::test]
    async fn block_placement_belongs_only_to_readability() {
        assert!(crate::tools::sch_analysis::tools()
            .iter()
            .all(|tool| tool.name != "preview_block_placement"));
        assert_readability_route("preview_block_placement").await;
    }

    #[tokio::test]
    async fn split_plan_belongs_to_readability() {
        assert_readability_route("plan_schematic_split").await;
    }

    #[tokio::test]
    async fn placement_check_belongs_to_readability() {
        assert_readability_route("check_placement").await;
    }

    #[tokio::test]
    async fn wire_label_exchange_belongs_to_readability() {
        assert_readability_route("exchange_schematic_wires_and_labels").await;
    }

    #[tokio::test]
    async fn field_placement_belongs_to_readability() {
        assert!(crate::tools::sch_components::tools()
            .iter()
            .all(|tool| tool.name != "autoplace_schematic_fields"));
        assert_readability_route("autoplace_schematic_fields").await;
    }
}
