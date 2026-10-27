//! MCP server session context.

use super::protocol::*;
use crate::router::ToolRouter;
use std::sync::Arc;

pub struct McpServerState {
    pub router: Arc<ToolRouter>,
}

impl McpServerState {
    pub fn new(router: Arc<ToolRouter>) -> Self {
        McpServerState { router }
    }

    pub fn build_initialize_result(instructions: Option<String>) -> InitializeResult {
        InitializeResult {
            protocol_version: "2025-06-18".to_string(),
            capabilities: ServerCapabilities {
                tools: Some(ToolsCapability {
                    list_changed: Some(true),
                }),
                ..Default::default()
            },
            server_info: ServerInfo {
                name: crate::app_dirs::APP_NAME.to_string(),
                version: env!("CARGO_PKG_VERSION").to_string(),
            },
            instructions,
        }
    }
}
