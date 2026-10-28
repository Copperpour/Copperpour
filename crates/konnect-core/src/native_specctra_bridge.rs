//! Optional KiCad 10 native Specctra export through Konnect's legacy ActionPlugin.
//!
//! The bridge is deliberately capability-scoped. It may export the active
//! board into its own private temporary directory; it cannot execute arbitrary
//! Python or write a caller-selected path. KiCad 11 and installations without
//! the legacy plugin continue through Konnect's Rust exporter.

use anyhow::{bail, Context, Result};
use reqwest::Url;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::time::Duration;

const PROTOCOL_VERSION: u32 = 1;
const MAX_DSN_BYTES: u64 = 512 * 1024 * 1024;

#[derive(Debug, Clone)]
pub(crate) struct NativeExport {
    pub dsn: String,
    pub plugin_pid: u32,
    pub protocol_version: u32,
}

#[derive(Debug, Default)]
pub(crate) struct NativeExportAttempt {
    pub export: Option<NativeExport>,
    pub diagnostics: Vec<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Registration {
    protocol_version: u32,
    pid: u32,
    address: String,
    token: String,
    started_at_unix: f64,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct StatusResponse {
    success: bool,
    protocol_version: u32,
    pid: u32,
    native_specctra_export: bool,
}

#[derive(Debug, Serialize)]
struct ExportRequest<'a> {
    expected_board: &'a str,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ExportResponse {
    success: bool,
    protocol_version: u32,
    pid: u32,
    board_path: String,
    dsn_path: String,
    dsn_bytes: u64,
}

pub(crate) async fn try_export(board: &Path) -> NativeExportAttempt {
    let mut attempt = NativeExportAttempt::default();
    let Some(root) = registration_root() else {
        attempt
            .diagnostics
            .push("native bridge registration directory is unavailable".to_string());
        return attempt;
    };
    let entries = match registrations(&root) {
        Ok(entries) => entries,
        Err(error) => {
            attempt
                .diagnostics
                .push(format!("native bridge discovery failed: {error:#}"));
            return attempt;
        }
    };
    if entries.is_empty() {
        attempt
            .diagnostics
            .push("no enabled KiCad 10 native bridge registration was found".to_string());
        return attempt;
    }
    let board = match board.canonicalize() {
        Ok(board) => board,
        Err(error) => {
            attempt
                .diagnostics
                .push(format!("resolve requested board failed: {error}"));
            return attempt;
        }
    };
    for registration_path in entries {
        match try_registration(&root, &registration_path, &board).await {
            Ok(export) => {
                attempt.export = Some(export);
                return attempt;
            }
            Err(error) => attempt.diagnostics.push(format!(
                "{}: {error:#}",
                registration_path
                    .file_name()
                    .and_then(|name| name.to_str())
                    .unwrap_or("native bridge registration")
            )),
        }
    }
    attempt
}

pub(crate) fn registration_root() -> Option<PathBuf> {
    if let Some(path) = crate::app_dirs::env_var("BRIDGE_DIR") {
        return Some(PathBuf::from(path));
    }
    // The KiCad plugin writes here; it ships with this binary, so no move.
    crate::app_dirs::local_data_dir()
        .map(|path| path.join(crate::app_dirs::APP_NAME).join("native-bridge"))
}

fn registrations(root: &Path) -> Result<Vec<PathBuf>> {
    if !root.is_dir() {
        return Ok(Vec::new());
    }
    let mut entries = std::fs::read_dir(root)
        .with_context(|| format!("read native bridge directory {}", root.display()))?
        .filter_map(|entry| entry.ok().map(|entry| entry.path()))
        .filter(|path| {
            path.file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.starts_with("bridge-") && name.ends_with(".json"))
        })
        .collect::<Vec<_>>();
    entries.sort();
    Ok(entries)
}

async fn try_registration(
    root: &Path,
    registration_path: &Path,
    board: &Path,
) -> Result<NativeExport> {
    let registration_source = std::fs::read_to_string(registration_path)
        .with_context(|| format!("read {}", registration_path.display()))?;
    let registration: Registration =
        serde_json::from_str(&registration_source).context("parse bridge registration")?;
    validate_registration(&registration)?;

    let client = reqwest::Client::builder()
        .connect_timeout(Duration::from_secs(2))
        .timeout(Duration::from_secs(35))
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .context("build native bridge HTTP client")?;
    let status: StatusResponse = client
        .get(format!("{}/v1/status", registration.address))
        .bearer_auth(&registration.token)
        .send()
        .await
        .context("query native bridge status")?
        .error_for_status()
        .context("native bridge status refused")?
        .json()
        .await
        .context("parse native bridge status")?;
    if !status.success
        || !status.native_specctra_export
        || status.protocol_version != PROTOCOL_VERSION
        || status.pid != registration.pid
    {
        bail!("native bridge status does not match its registration");
    }

    let board_text = board
        .to_str()
        .with_context(|| format!("board path is not UTF-8: {}", board.display()))?;
    let response: ExportResponse = client
        .post(format!("{}/v1/export-specctra-dsn", registration.address))
        .bearer_auth(&registration.token)
        .json(&ExportRequest {
            expected_board: board_text,
        })
        .send()
        .await
        .context("request native Specctra export")?
        .error_for_status()
        .context("native Specctra export refused")?
        .json()
        .await
        .context("parse native Specctra export response")?;
    validate_response(&registration, &response, board)?;

    let dsn_path = PathBuf::from(&response.dsn_path)
        .canonicalize()
        .with_context(|| format!("resolve native DSN {}", response.dsn_path))?;
    let canonical_root = root
        .canonicalize()
        .with_context(|| format!("resolve bridge root {}", root.display()))?;
    if !dsn_path.starts_with(&canonical_root) {
        bail!("native bridge returned a DSN outside its private directory");
    }
    let metadata = std::fs::metadata(&dsn_path)
        .with_context(|| format!("inspect native DSN {}", dsn_path.display()))?;
    if metadata.len() == 0 || metadata.len() > MAX_DSN_BYTES || metadata.len() != response.dsn_bytes
    {
        bail!("native bridge DSN size does not match its response");
    }
    let dsn = std::fs::read_to_string(&dsn_path)
        .with_context(|| format!("read native DSN {}", dsn_path.display()));
    let cleanup = std::fs::remove_file(&dsn_path)
        .with_context(|| format!("remove consumed native DSN {}", dsn_path.display()));
    let dsn = dsn?;
    cleanup?;
    if !dsn.trim_start().starts_with("(pcb ") {
        bail!("KiCad native export is not a Specctra PCB");
    }
    Ok(NativeExport {
        dsn,
        plugin_pid: response.pid,
        protocol_version: response.protocol_version,
    })
}

fn validate_registration(registration: &Registration) -> Result<()> {
    if registration.protocol_version != PROTOCOL_VERSION
        || registration.pid == 0
        || registration.token.len() < 32
        || !registration.started_at_unix.is_finite()
        || registration.started_at_unix <= 0.0
    {
        bail!("native bridge registration is invalid or incompatible");
    }
    let url = Url::parse(&registration.address).context("parse native bridge address")?;
    if url.scheme() != "http"
        || url.host_str() != Some("127.0.0.1")
        || url.port().is_none()
        || url.path() != "/"
        || url.query().is_some()
        || url.fragment().is_some()
        || !url.username().is_empty()
        || url.password().is_some()
    {
        bail!("native bridge address is not a plain IPv4 loopback endpoint");
    }
    Ok(())
}

fn validate_response(
    registration: &Registration,
    response: &ExportResponse,
    board: &Path,
) -> Result<()> {
    if !response.success
        || response.protocol_version != registration.protocol_version
        || response.pid != registration.pid
        || !Path::new(&response.dsn_path)
            .extension()
            .and_then(|extension| extension.to_str())
            .is_some_and(|extension| extension.eq_ignore_ascii_case("dsn"))
    {
        bail!("native bridge export response is invalid");
    }
    let active_board = Path::new(&response.board_path)
        .canonicalize()
        .with_context(|| format!("resolve active bridge board {}", response.board_path))?;
    let requested_board = board
        .canonicalize()
        .with_context(|| format!("resolve requested bridge board {}", board.display()))?;
    if active_board != requested_board {
        bail!(
            "native bridge board '{}' does not match requested board '{}'",
            active_board.display(),
            requested_board.display()
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    #[test]
    fn registration_requires_exact_loopback_shape() {
        let valid = Registration {
            protocol_version: 1,
            pid: 42,
            address: "http://127.0.0.1:32123".to_string(),
            token: "x".repeat(32),
            started_at_unix: 1.0,
        };
        validate_registration(&valid).unwrap();
        for address in [
            "https://127.0.0.1:32123",
            "http://localhost:32123",
            "http://127.0.0.1:32123/path",
            "http://example.com:32123",
        ] {
            let invalid = Registration {
                address: address.to_string(),
                ..Registration {
                    protocol_version: valid.protocol_version,
                    pid: valid.pid,
                    address: valid.address.clone(),
                    token: valid.token.clone(),
                    started_at_unix: valid.started_at_unix,
                }
            };
            assert!(validate_registration(&invalid).is_err(), "{address}");
        }
    }

    #[test]
    fn discovery_is_empty_for_a_missing_directory() {
        let temp = tempfile::tempdir().unwrap();
        assert!(registrations(&temp.path().join("missing"))
            .unwrap()
            .is_empty());
    }

    #[tokio::test]
    async fn authenticated_registration_returns_and_consumes_private_dsn() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("native-bridge");
        let session = root.join("session-42-test");
        std::fs::create_dir_all(&session).unwrap();
        let board = temp.path().join("board.kicad_pcb");
        std::fs::write(&board, "(kicad_pcb)\n").unwrap();
        let dsn = session.join("native-test.dsn");
        let dsn_source = "(pcb native-test)\n";
        std::fs::write(&dsn, dsn_source).unwrap();

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let token = "t".repeat(32);
        let board_text = board.canonicalize().unwrap().display().to_string();
        let dsn_text = dsn.canonicalize().unwrap().display().to_string();
        let server = tokio::spawn(async move {
            for response in [
                serde_json::json!({
                    "success": true,
                    "protocol_version": 1,
                    "pid": 42,
                    "native_specctra_export": true
                }),
                serde_json::json!({
                    "success": true,
                    "protocol_version": 1,
                    "pid": 42,
                    "board_path": board_text,
                    "dsn_path": dsn_text,
                    "dsn_bytes": dsn_source.len()
                }),
            ] {
                let (mut stream, _) = listener.accept().await.unwrap();
                let mut request = vec![0u8; 16 * 1024];
                let count = stream.read(&mut request).await.unwrap();
                let request = String::from_utf8_lossy(&request[..count]);
                assert!(request
                    .to_ascii_lowercase()
                    .contains("authorization: bearer tttttttttttttttttttttttttttttttt"));
                let body = response.to_string();
                let reply = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                    body.len(), body
                );
                stream.write_all(reply.as_bytes()).await.unwrap();
            }
        });
        let registration_path = root.join("bridge-42.json");
        std::fs::write(
            &registration_path,
            serde_json::json!({
                "protocol_version": 1,
                "pid": 42,
                "address": format!("http://127.0.0.1:{port}"),
                "token": token,
                "started_at_unix": 1.0
            })
            .to_string(),
        )
        .unwrap();

        let export = try_registration(&root, &registration_path, &board)
            .await
            .unwrap();
        server.await.unwrap();
        assert_eq!(export.dsn, dsn_source);
        assert_eq!(export.plugin_pid, 42);
        assert!(
            !dsn.exists(),
            "Rust must consume the private bridge artifact"
        );
    }
}
