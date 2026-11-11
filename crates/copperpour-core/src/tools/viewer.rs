//! Session ownership and OS browser launch for the schematic viewer.
use copperpour_viewer::browser::BrowserViewer;
use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    process::Stdio,
    time::Duration,
};

#[derive(Default)]
pub(super) struct ViewerRegistry(tokio::sync::Mutex<HashMap<PathBuf, BrowserViewer>>);

impl ViewerRegistry {
    pub async fn open(&self, root: &Path, cli: &str) -> Result<(String, bool), String> {
        // Serialize starts as well as lookups: concurrent calls must not create
        // two instances for one root. Cancelled startup is owned by BrowserViewer.
        let mut viewers = self.0.lock().await;
        if let Some(viewer) = viewers.get(root).filter(|viewer| viewer.is_running()) {
            return Ok((viewer.url.clone(), true));
        }
        if let Some(viewer) = viewers.remove(root) {
            viewer.shutdown().await;
        }
        let cli = if cli.is_empty() {
            copperpour_viewer::resolve_kicad_cli(None)
        } else {
            cli.to_owned()
        };
        let viewer = BrowserViewer::start(root, &cli, Duration::from_secs(30)).await?;
        let url = viewer.url.clone();
        viewers.insert(root.to_owned(), viewer);
        Ok((url, false))
    }
}

pub(super) struct BrowserLaunch {
    pub succeeded: bool,
    pub error: Option<String>,
}
impl BrowserLaunch {
    pub fn not_requested() -> Self {
        Self {
            succeeded: false,
            error: None,
        }
    }
}

pub(super) async fn launch_browser(url: &str) -> BrowserLaunch {
    #[cfg(target_os = "windows")]
    let mut command = {
        let mut c = tokio::process::Command::new("rundll32.exe");
        c.arg("url.dll,FileProtocolHandler");
        c
    };
    #[cfg(target_os = "macos")]
    let mut command = tokio::process::Command::new("open");
    #[cfg(not(any(target_os = "windows", target_os = "macos")))]
    let mut command = tokio::process::Command::new("xdg-open");
    command.arg(url);
    observe_browser_launch(command, Duration::from_secs(5)).await
}

async fn observe_browser_launch(
    mut command: tokio::process::Command,
    timeout: Duration,
) -> BrowserLaunch {
    // Never inherit the MCP stdio stream, and never forward a token-bearing URL
    // from opener diagnostics. Exit success proves only that the opener accepted it.
    command
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .kill_on_drop(true);
    let outcome = tokio::time::timeout(timeout, command.status()).await;
    match outcome {
        Ok(Ok(status)) if status.success() => BrowserLaunch { succeeded: true, error: None },
        Ok(Ok(status)) => BrowserLaunch { succeeded: false, error: Some(format!("OS browser opener exited with {status}; use the returned URL manually")) },
        Ok(Err(error)) => BrowserLaunch { succeeded: false, error: Some(format!("Could not start OS browser opener: {error}; use the returned URL manually")) },
        Err(_) => BrowserLaunch { succeeded: false, error: Some("OS browser opener did not finish within five seconds; a browser may still have opened. Use the returned URL if needed".into()) },
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use crate::{mcp::handler::McpHandler, tools::ServerConfig};
    use serde_json::{json, Value};
    use std::os::unix::fs::{symlink, PermissionsExt};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    fn fixture() -> (tempfile::TempDir, PathBuf, String) {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("root.kicad_sch");
        std::fs::copy(
            Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/derived_lib_name.kicad_sch"),
            &root,
        )
        .unwrap();
        let cli = dir.path().join("cli");
        std::fs::write(&cli, r##"#!/bin/sh
mkdir -p "$5"
printf '<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 100 80"><text>saved</text></svg>' > "$5/$(basename "$6" .kicad_sch).svg"
"##).unwrap();
        std::fs::set_permissions(&cli, std::fs::Permissions::from_mode(0o700)).unwrap();
        (dir, root, cli.to_string_lossy().into_owned())
    }
    async fn handler(cli: String) -> McpHandler {
        McpHandler::new(ServerConfig {
            kicad_cli: cli,
            kicad_binary: String::new(),
            ipc_address: String::new(),
            project_dir: None,
            jlcpcb_db_path: None,
            auto_load_toolsets: false,
            eager_toolsets: false,
        })
        .await
        .unwrap()
    }
    async fn call(handler: &McpHandler, args: Value) -> Value {
        let response = handler.handle_message(json!({"jsonrpc":"2.0", "id":242, "method":"tools/call", "params":{"name":"open_schematic_viewer", "arguments":args}})).await.unwrap();
        serde_json::to_value(response).unwrap()
    }
    fn body(response: &Value) -> Value {
        assert_ne!(response["result"]["isError"], true, "{response}");
        serde_json::from_str(response["result"]["content"][0]["text"].as_str().unwrap()).unwrap()
    }
    async fn state(url: &str) -> String {
        let (base, token) = url.split_once("/#token=").unwrap();
        let address = base.strip_prefix("http://").unwrap();
        let mut stream = tokio::net::TcpStream::connect(address).await.unwrap();
        stream.write_all(format!("GET /state HTTP/1.1\r\nHost: {address}\r\nAuthorization: Bearer {token}\r\nConnection: close\r\n\r\n").as_bytes()).await.unwrap();
        let mut response = String::new();
        stream.read_to_string(&mut response).await.unwrap();
        response
    }
    #[tokio::test]
    async fn browser_mcp_reuses_canonical_root_serves_data_and_stops_with_session() {
        let (dir, root, cli) = fixture();
        let handler = handler(cli).await;
        let alias = dir.path().join("alias.kicad_sch");
        symlink(&root, &alias).unwrap();
        let (a, b) = tokio::join!(
            call(&handler, json!({"schematic":root,"open_browser":false})),
            call(&handler, json!({"schematic":alias,"open_browser":false}))
        );
        let a = body(&a);
        let b = body(&b);
        assert_eq!(a["url"], b["url"]);
        assert_ne!(a["reused"], b["reused"]);
        assert_eq!(a["launched"], true);
        assert_eq!(a["viewer"], "browser");
        assert_eq!(a["schematic"], json!(root.canonicalize().unwrap()));
        assert_eq!(a["browser_launch_requested"], false);
        assert_eq!(a["browser_launch_succeeded"], false);
        assert_eq!(a["browser_launch_error"], Value::Null);
        let url = a["url"].as_str().unwrap();
        let response = state(url).await;
        assert!(response.starts_with("HTTP/1.1 200"), "{response}");
        assert!(response.contains("root.kicad_sch"));
        drop(handler);
        let address = url
            .strip_prefix("http://")
            .unwrap()
            .split('/')
            .next()
            .unwrap();
        tokio::time::timeout(Duration::from_secs(3), async {
            while tokio::net::TcpStream::connect(address).await.is_ok() {
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .expect("session drop closes listener");
    }
    #[tokio::test]
    async fn browser_mcp_refuses_bad_options_missing_files_and_render_failure() {
        let (_dir, root, _cli) = fixture();
        let handler = handler("/nonexistent/kicad-cli".into()).await;
        for option in [json!(null), json!("false"), json!(0)] {
            let response = call(&handler, json!({"schematic":root,"open_browser":option})).await;
            assert!(response.to_string().contains("open_browser"));
            assert!(
                response.get("error").is_some() || response["result"]["isError"] == true,
                "{response}"
            );
        }
        for path in [json!(root), json!("/nonexistent/root.kicad_sch")] {
            let response = call(&handler, json!({"schematic":path,"open_browser":false})).await;
            assert_eq!(response["result"]["isError"], true, "{response}");
        }
    }
    #[tokio::test]
    async fn browser_registry_failed_start_can_retry_and_separate_roots_have_separate_urls() {
        let (_dir, root, cli) = fixture();
        let (_other, other_root, _) = fixture();
        let registry = ViewerRegistry::default();
        assert!(registry
            .open(&root, "/nonexistent/kicad-cli")
            .await
            .is_err());
        let (url, reused) = registry.open(&root, &cli).await.unwrap();
        assert!(!reused);
        let (other_url, reused) = registry.open(&other_root, &cli).await.unwrap();
        assert!(!reused);
        assert_ne!(url, other_url);
        // A stopped/failed service must not be returned as a healthy reuse.
        let old = registry.0.lock().await.remove(&root).unwrap();
        old.shutdown().await;
        let (new_url, reused) = registry.open(&root, &cli).await.unwrap();
        assert!(!reused);
        assert_ne!(url, new_url);
    }
    fn command(script: &str) -> tokio::process::Command {
        let mut c = tokio::process::Command::new("sh");
        c.arg("-c").arg(script);
        c
    }
    #[tokio::test]
    async fn browser_opener_reports_exit_failure_spawn_failure_and_timeout_truthfully() {
        let success = observe_browser_launch(command("exit 0"), Duration::from_secs(1)).await;
        assert!(success.succeeded);
        assert!(success.error.is_none());
        for c in [
            command("exit 9"),
            tokio::process::Command::new("/nonexistent/browser"),
        ] {
            let result = observe_browser_launch(c, Duration::from_secs(1)).await;
            assert!(!result.succeeded);
            assert!(result.error.is_some());
        }
        let result =
            observe_browser_launch(command("exec sleep 10"), Duration::from_millis(50)).await;
        assert!(!result.succeeded);
        assert!(result.error.unwrap().contains("did not finish"));
    }
}
