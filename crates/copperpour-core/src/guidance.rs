//! Embedded MCP guidance and read-only installed-guidance drift reporting.

mod bundle;
pub use bundle::{SkillManifest, LEGACY_OPERATING_RULES_SKILL, OPERATING_RULES_SKILL, SKILLS};

use serde_json::Value;

pub trait GuidanceProbe: Send + Sync {
    /// The `guidance` block of `get_installation_info`.
    fn detail(&self) -> Value;
    /// A notice for the model when installed guidance differs from the bundle
    /// this server carries. The probe hands it out at most once for each
    /// installed version, so a later call or a later start gets `None`.
    fn take_notice(&self) -> Option<String>;
}

/// Small, always-present rules; full workflows are fetched on demand.
pub const SESSION_RULES: &str = "Copperpour operating rules for KiCad work:\n\
Route every design modification through Copperpour MCP tools. Never hand-edit \
.kicad_sch, .kicad_pcb, .kicad_pro, .kicad_sym, .kicad_mod, fp-lib-table or \
sym-lib-table with text/file tools. Discover tools with list_toolboxes and \
load_toolset; if the required MCP capability is unavailable, stop and report it \
instead of editing source files. Prefer tool exports for analysis. PCB IPC \
operations need the exact board open in KiCad with IPC enabled; saved-file \
schematic operations do not require the GUI.\n\
Before mutations, dependent checks, retries or completion claims, read \
copperpour/references/reliability-contract.md through get_guidance. Bind the \
requested document and distinguish live-editor evidence from saved files. Await \
state-changing prerequisites, inspect errors and per-item outcomes, and verify \
results by readback. A successful call alone is not proof that a change applied \
or a check ran. After a partial or uncertain mutation, inspect what happened \
before retrying; do not repeat already-applied work or replace missing evidence \
with zero findings.\n\
Use get_guidance with no arguments to list this build's skills and references, \
then read an exact listed path (start with copperpour/SKILL.md). The same content \
is available as copperpour://guidance/ resources. Server guidance needs no init \
or local skill installation; init remains opt-in for native skills and hooks.";

pub fn session_instructions(notice: Option<&str>) -> String {
    match notice {
        Some(notice) if !notice.is_empty() => format!("{SESSION_RULES}\n\n{notice}"),
        _ => SESSION_RULES.to_owned(),
    }
}

const URI_PREFIX: &str = "copperpour://guidance/";

struct Document {
    path: String,
    text: &'static str,
}

fn documents() -> impl Iterator<Item = Document> {
    SKILLS.iter().flat_map(|skill| {
        std::iter::once(Document {
            path: format!("{}/SKILL.md", skill.name),
            text: skill.content,
        })
        .chain(skill.references.iter().map(|(name, text)| Document {
            path: format!("{}/references/{name}", skill.name),
            text,
        }))
    })
}

fn resource_list(include_paths: bool) -> Value {
    serde_json::json!({"resources": documents().map(|doc| {
        let mut resource = serde_json::json!({
            "uri": format!("{URI_PREFIX}{}", doc.path),
            "name": doc.path,
            "mimeType": "text/markdown"
        });
        if include_paths {
            resource["path"] = serde_json::json!(doc.path);
        }
        resource
    }).collect::<Vec<_>>()})
}

fn resource_content(doc: Document) -> Value {
    serde_json::json!({"contents": [{
        "uri": format!("{URI_PREFIX}{}", doc.path),
        "mimeType": "text/markdown",
        "text": doc.text
    }]})
}

/// Typed only for resource dispatch; unrelated MCP errors retain their contract.
#[derive(Debug, thiserror::Error)]
#[error("{message}")]
pub(crate) struct ResourceRequestError {
    pub code: i32,
    pub message: String,
    pub data: Option<Value>,
}

fn invalid_params(message: &str) -> ResourceRequestError {
    ResourceRequestError {
        code: -32602,
        message: message.into(),
        data: None,
    }
}

pub(crate) fn validate_list_params(params: Option<&Value>) -> Result<(), ResourceRequestError> {
    if let Some(params) = params {
        let object = params
            .as_object()
            .ok_or_else(|| invalid_params("Expected an object for resource-list parameters"))?;
        if object.contains_key("cursor") {
            return Err(invalid_params(
                "This static resource catalog has no continuation cursor",
            ));
        }
    }
    Ok(())
}

pub(crate) fn list_resources(params: Option<&Value>) -> Result<Value, ResourceRequestError> {
    validate_list_params(params)?;
    Ok(resource_list(false))
}

pub(crate) fn read_resource(params: Option<&Value>) -> Result<Value, ResourceRequestError> {
    let uri = params
        .and_then(Value::as_object)
        .and_then(|params| params.get("uri"))
        .and_then(Value::as_str)
        .ok_or_else(|| invalid_params("resources/read requires a string uri"))?;
    // Exact catalog identity only: no URL decoding, path normalization or file IO.
    documents()
        .find(|doc| uri == format!("{URI_PREFIX}{}", doc.path))
        .map(resource_content)
        .ok_or_else(|| ResourceRequestError {
            code: -32002,
            message: "Guidance resource not found; use resources/list".into(),
            data: Some(serde_json::json!({"uri": uri})),
        })
}

pub(crate) fn get_guidance(args: &Value) -> crate::mcp::protocol::CallToolResult {
    use crate::mcp::protocol::CallToolResult;
    let Some(object) = args.as_object() else {
        return crate::tools::invalid_arg("arguments", "expected an object");
    };
    if object.keys().any(|key| key != "path") {
        return crate::tools::invalid_arg("arguments", "only path is supported");
    }
    let result = match object.get("path") {
        None => {
            let mut list = resource_list(true);
            list["count"] = serde_json::json!(documents().count());
            list
        }
        Some(value) => {
            let Some(path) = value.as_str() else {
                return crate::tools::invalid_arg("path", "expected an exact bundle-path string");
            };
            let Some(doc) = documents().find(|doc| doc.path == path) else {
                return crate::tools::invalid_arg("path", "unknown guidance path; call get_guidance with no arguments to list valid paths");
            };
            resource_content(doc)
        }
    };
    CallToolResult::text(result.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mcp::handler::McpHandler;
    use crate::mcp::protocol::JsonRpcResponse;
    use serde_json::json;

    async fn handler() -> McpHandler {
        McpHandler::new(crate::tools::ServerConfig::default())
            .await
            .unwrap()
    }

    async fn request(handler: &McpHandler, method: &str, params: Value) -> JsonRpcResponse {
        handler
            .handle_message(json!({"jsonrpc":"2.0","id":17,"method":method,"params":params}))
            .await
            .unwrap()
    }

    async fn tool(handler: &McpHandler, args: Value) -> Value {
        let reply = request(
            handler,
            "tools/call",
            json!({"name":"get_guidance","arguments":args}),
        )
        .await;
        assert!(reply.error.is_none(), "{reply:?}");
        let result = reply.result.unwrap();
        assert_ne!(result["isError"], true, "{result}");
        serde_json::from_str(result["content"][0]["text"].as_str().unwrap()).unwrap()
    }

    #[tokio::test]
    async fn guidance_is_available_before_loading_any_toolset_and_without_a_probe() {
        let handler = handler().await;
        for _ in 0..2 {
            let init = request(
                &handler,
                "initialize",
                json!({"clientInfo":{"name":"test-client"}}),
            )
            .await
            .result
            .unwrap();
            let rules = init["instructions"].as_str().unwrap();
            assert!(rules.contains("Never hand-edit"));
            assert!(rules.contains("partial or uncertain mutation"));
            assert!(rules.contains("get_guidance"));
            assert_eq!(init["capabilities"]["resources"], json!({}));
        }
        // Even after the starter kit is unloaded, guidance stays discoverable/callable.
        for name in ["project", "config"] {
            request(
                &handler,
                "tools/call",
                json!({"name":"unload_toolset","arguments":{"name":name}}),
            )
            .await;
        }
        let active = request(
            &handler,
            "tools/call",
            json!({"name":"get_active_toolsets","arguments":{}}),
        )
        .await
        .result
        .unwrap();
        let active: Value =
            serde_json::from_str(active["content"][0]["text"].as_str().unwrap()).unwrap();
        assert_eq!(active["active_toolsets"], json!([]));
        let tools = request(&handler, "tools/list", json!({}))
            .await
            .result
            .unwrap();
        assert!(tools["tools"]
            .as_array()
            .unwrap()
            .iter()
            .any(|t| t["name"] == "get_guidance"));
        let listed = tool(&handler, json!({})).await;
        assert_eq!(listed["count"], 16);
        assert_eq!(listed["resources"].as_array().unwrap().len(), 16);
        let contents = tool(&handler, json!({"path":"copperpour/SKILL.md"})).await;
        assert_eq!(
            contents["contents"][0]["text"],
            include_str!("../../copperpour/assets/skills/copperpour/SKILL.md")
        );
    }

    #[tokio::test]
    async fn guidance_resources_and_tool_serve_every_bundled_file_with_identical_bytes() {
        let handler = handler().await;
        let listed = request(&handler, "resources/list", json!({}))
            .await
            .result
            .unwrap();
        let resources = listed["resources"].as_array().unwrap();
        assert_eq!(resources.len(), 16);
        let tool_list = tool(&handler, json!({})).await;
        // Independent on-disk inventory catches an omitted or stale embedded asset.
        let assets =
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../copperpour/assets/skills");
        let mut expected = std::collections::BTreeMap::new();
        for name in [
            "copperpour",
            "kicad-schematic",
            "kicad-pcb",
            "kicad-manufacture",
            "kicad-review",
            "kicad-library",
        ] {
            expected.insert(
                format!("{name}/SKILL.md"),
                std::fs::read_to_string(assets.join(name).join("SKILL.md")).unwrap(),
            );
            let references = assets.join(name).join("references");
            if references.is_dir() {
                for entry in std::fs::read_dir(references).unwrap() {
                    let file = entry.unwrap();
                    expected.insert(
                        format!("{name}/references/{}", file.file_name().to_str().unwrap()),
                        std::fs::read_to_string(file.path()).unwrap(),
                    );
                }
            }
        }
        expected.insert(
            "copperpour/references/reliability-contract.md".into(),
            include_str!("../../../docs/RELIABILITY_CONTRACT.md").into(),
        );
        assert_eq!(expected.len(), resources.len());
        let mut observed = std::collections::BTreeSet::new();
        for resource in resources {
            let uri = resource["uri"].as_str().unwrap();
            let path = resource["name"].as_str().unwrap();
            assert!(observed.insert(path));
            assert_eq!(resource["mimeType"], "text/markdown");
            assert_eq!(uri, format!("copperpour://guidance/{path}"));
            let read = request(&handler, "resources/read", json!({"uri":uri})).await;
            assert!(read.error.is_none(), "{read:?}");
            let content = read.result.unwrap();
            assert_eq!(content["contents"].as_array().unwrap().len(), 1);
            assert_eq!(content["contents"][0]["uri"], uri);
            assert_eq!(content["contents"][0]["mimeType"], "text/markdown");
            assert_eq!(content["contents"][0]["text"], expected[path]);
            assert_eq!(tool(&handler, json!({"path":path})).await, content);
            assert!(tool_list["resources"]
                .as_array()
                .unwrap()
                .iter()
                .any(|r| r["path"] == path && r["uri"] == uri));
        }
        assert!(listed.get("nextCursor").is_none());
        assert_eq!(
            request(&handler, "resources/templates/list", json!({}))
                .await
                .result
                .unwrap(),
            json!({"resourceTemplates":[]})
        );
    }

    #[tokio::test]
    async fn guidance_resource_errors_preserve_request_id_and_never_read_arbitrary_paths() {
        let handler = handler().await;
        let dir = tempfile::tempdir().unwrap();
        let secret = dir.path().join("secret.md");
        std::fs::write(&secret, "PRIVATE LOCAL CONTENT").unwrap();
        for uri in [
            format!("file://{}", secret.display()),
            "copperpour://guidance/copperpour/../kicad-pcb/SKILL.md".into(),
            "copperpour://guidance/copperpour/%2e%2e/SKILL.md".into(),
            "copperpour://guidance/copperpour\\SKILL.md".into(),
            "copperpour://guidance/copperpour/SKILL.md?x=1".into(),
            "copperpour://guidance/copperpour/SKILL.md#fragment".into(),
            "copperpour://guidance/unknown/SKILL.md".into(),
        ] {
            let read = request(&handler, "resources/read", json!({"uri":uri})).await;
            assert_eq!(read.id, 17);
            assert!(read.result.is_none());
            let error = read.error.unwrap();
            assert_eq!(error.code, -32002);
            assert_eq!(error.data.unwrap()["uri"], uri);
        }
        assert_eq!(
            std::fs::read_to_string(secret).unwrap(),
            "PRIVATE LOCAL CONTENT"
        );
        for args in [json!({}), json!({"uri":null}), json!({"uri":42}), json!([])] {
            let read = request(&handler, "resources/read", args).await;
            assert_eq!(read.error.unwrap().code, -32602);
        }
        for method in ["resources/list", "resources/templates/list"] {
            for args in [
                json!([]),
                json!("bad"),
                json!({"cursor":"bogus"}),
                json!({"cursor":null}),
            ] {
                assert_eq!(
                    request(&handler, method, args).await.error.unwrap().code,
                    -32602
                );
            }
        }
    }

    #[test]
    fn guidance_resource_read_checks_required_uri_type() {
        for params in [
            None,
            Some(json!({})),
            Some(json!({"uri":null})),
            Some(json!({"uri":1})),
            Some(json!([])),
        ] {
            assert_eq!(read_resource(params.as_ref()).unwrap_err().code, -32602);
        }
    }

    #[test]
    fn guidance_list_checks_object_shape_and_rejects_continuation_cursors() {
        assert!(validate_list_params(None).is_ok());
        assert!(validate_list_params(Some(&json!({}))).is_ok());
        for params in [json!([]), json!({"cursor":"bad"}), json!({"cursor":null})] {
            assert_eq!(
                validate_list_params(Some(&params)).unwrap_err().code,
                -32602
            );
        }
    }

    #[test]
    fn guidance_direct_tool_checks_argument_shape_and_path_type() {
        for args in [
            json!(null),
            json!([]),
            json!({"unknown":true}),
            json!({"path":null}),
            json!({"path":1}),
        ] {
            assert_eq!(
                crate::mcp::error::extract_error_kind(&get_guidance(&args)).as_deref(),
                Some("invalid_argument")
            );
        }
    }

    #[test]
    fn guidance_direct_tool_refuses_unlisted_paths() {
        for path in [
            "unknown",
            "../secret.md",
            "copperpour://guidance/copperpour/SKILL.md",
            "copperpour/%53KILL.md",
        ] {
            assert_eq!(
                crate::mcp::error::extract_error_kind(&get_guidance(&json!({"path":path})))
                    .as_deref(),
                Some("invalid_argument")
            );
        }
    }

    #[tokio::test]
    async fn guidance_tool_refuses_bad_paths_and_malformed_present_arguments() {
        let handler = handler().await;
        for args in [
            json!({"path":null}),
            json!({"path":1}),
            json!({"path":"../secret.md"}),
            json!({"path":"copperpour/%53KILL.md"}),
            json!({"path":"copperpour/SKILL.md","extra":true}),
            json!([]),
        ] {
            let reply = request(
                &handler,
                "tools/call",
                json!({"name":"get_guidance","arguments":args}),
            )
            .await;
            let result: crate::mcp::protocol::CallToolResult =
                serde_json::from_value(reply.result.unwrap()).unwrap();
            assert_eq!(
                crate::mcp::error::extract_error_kind(&result).as_deref(),
                Some("invalid_argument")
            );
            assert_eq!(
                crate::mcp::error::extract_error_kind(&get_guidance(&args)).as_deref(),
                Some("invalid_argument")
            );
        }
    }
}
