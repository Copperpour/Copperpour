//! Read-only runtime and installation provenance for the serving process.

use crate::config_resolution::{ConfigResolution, SEARCH_POLICY};
use crate::guidance::GuidanceProbe;
use crate::tools::ServerConfig;
use serde_json::{json, Value};
use std::cmp::Ordering;
use std::path::{Path, PathBuf};
use std::time::Duration;
use tokio::process::Command;

const COMMAND_TIMEOUT: Duration = Duration::from_secs(5);
const PCM_IDENTIFIER: &str = "com.github.copperpour.copperpour";

pub(crate) async fn collect(
    config: &ServerConfig,
    resolution: &ConfigResolution,
    guidance: Option<&dyn GuidanceProbe>,
) -> Value {
    let running_version = env!("CARGO_PKG_VERSION");
    let executable_path = installed_executable_path().ok();
    let installation = executable_path
        .as_deref()
        .map(classify_installation)
        .unwrap_or_else(InstallSource::unavailable);

    let binary_probe = match executable_path.as_deref() {
        Some(path) => probe_command_version(path, VersionCommand::Konnect).await,
        None => VersionProbe::unavailable(),
    };
    let newer_than_running = binary_probe
        .version
        .as_deref()
        .and_then(|version| stable_version_cmp(version, running_version))
        .map(|ordering| ordering == Ordering::Greater);

    let kicad_cli_path = crate::kicad_install::find_cli(&config.kicad_cli);
    let kicad_probe = match kicad_cli_path.as_deref() {
        Some(path) => probe_command_version(path, VersionCommand::KiCad).await,
        None => VersionProbe::not_found(),
    };

    let ipc_endpoint = if config.ipc_address.trim().is_empty() {
        None
    } else {
        Some(copperpour_ipc::redact_endpoint(config.ipc_address.trim()))
    };

    json!({
        "build": {
            "version": running_version,
            "commit": option_env!("COPPERPOUR_BUILD_COMMIT"),
            "commit_source": option_env!("COPPERPOUR_BUILD_COMMIT_SOURCE"),
            "working_tree_state": "not_recorded",
            "profile": if cfg!(debug_assertions) { "debug" } else { "release" },
            "target_os": std::env::consts::OS,
            "target_arch": std::env::consts::ARCH,
        },
        "runtime": {
            "executable_path": executable_path.as_deref().map(display_path),
        },
        "installation": {
            "source": installation.name,
            "evidence": installation.evidence,
            "manifest_path": installation.manifest_path.as_deref().map(display_path),
            "binary_on_disk": {
                "probe_status": binary_probe.status,
                "version": binary_probe.version,
                "newer_than_running": newer_than_running,
            },
        },
        "kicad": {
            "cli_path": kicad_cli_path.as_deref().map(display_path),
            "probe_status": kicad_probe.status,
            "version": kicad_probe.version,
        },
        "ipc": {
            "configured": ipc_endpoint.is_some(),
            "source": "resolved_server_config",
            "endpoint": ipc_endpoint,
        },
        "configuration": configuration_block(resolution),
        "guidance": guidance_block(guidance),
        "restart_guidance": restart_guidance(installation.name, newer_than_running),
    })
}

/// Report which configuration file configured this process (#419).
///
/// Derived from the resolution captured at startup, never from a fresh search:
/// a file created after launch must not be reported as the one that configured
/// the running process. Paths only — no configuration values, file contents or
/// IPC credentials.
fn configuration_block(resolution: &ConfigResolution) -> Value {
    json!({
        "source": resolution.source().as_str(),
        "selected_path": resolution.selected_path().map(display_path),
        "search_policy": SEARCH_POLICY,
        "skipped_existing_paths": resolution
            .skipped_existing_paths()
            .iter()
            .map(|path| display_path(path.as_path()))
            .collect::<Vec<_>>(),
    })
}

/// Installed guidance compared with the bundle this binary carries (#728).
/// Read-only: a process without a probe says so rather than guessing.
fn guidance_block(guidance: Option<&dyn GuidanceProbe>) -> Value {
    match guidance {
        Some(probe) => probe.detail(),
        None => json!({
            "probe_status": "not_available",
            "reason": "This process does not carry the guidance bundle (embedded or library use); run `copperpour status` from the standalone binary.",
        }),
    }
}

#[derive(Debug)]
struct InstallSource {
    name: &'static str,
    evidence: &'static str,
    manifest_path: Option<PathBuf>,
}

impl InstallSource {
    fn unavailable() -> Self {
        Self {
            name: "unknown",
            evidence:
                "The serving executable path could not be resolved; no install source was inferred.",
            manifest_path: None,
        }
    }
}

fn classify_installation(executable_path: &Path) -> InstallSource {
    let manifest_path = executable_path
        .parent()
        .and_then(Path::parent)
        .map(|plugin_dir| plugin_dir.join("plugin.json"));

    if let Some(path) = manifest_path.filter(|path| is_konnect_pcm_manifest(path)) {
        return InstallSource {
            name: "kicad_pcm",
            evidence:
                "A sibling KiCad executable-plugin manifest has Copperpour's exact public identifier.",
            manifest_path: Some(path),
        };
    }

    InstallSource {
        name: "unknown",
        evidence: "No verified KiCad PCM manifest was found beside this executable; standalone and source builds are intentionally not guessed from path names.",
        manifest_path: None,
    }
}

fn is_konnect_pcm_manifest(path: &Path) -> bool {
    let Ok(raw) = std::fs::read_to_string(path) else {
        return false;
    };
    let Ok(manifest) = serde_json::from_str::<Value>(&raw) else {
        return false;
    };
    manifest.get("identifier").and_then(Value::as_str) == Some(PCM_IDENTIFIER)
        && manifest
            .get("runtime")
            .and_then(|runtime| runtime.get("type"))
            .and_then(Value::as_str)
            == Some("exec")
}

#[derive(Clone, Copy)]
enum VersionCommand {
    Konnect,
    KiCad,
}

struct VersionProbe {
    status: &'static str,
    version: Option<String>,
}

impl VersionProbe {
    fn unavailable() -> Self {
        Self {
            status: "executable_path_unavailable",
            version: None,
        }
    }

    fn not_found() -> Self {
        Self {
            status: "not_found",
            version: None,
        }
    }
}

async fn probe_command_version(path: &Path, command_kind: VersionCommand) -> VersionProbe {
    let mut command = Command::new(path);
    command.arg("--version").kill_on_drop(true);
    let output = match tokio::time::timeout(COMMAND_TIMEOUT, command.output()).await {
        Ok(Ok(output)) => output,
        Ok(Err(_)) => {
            return VersionProbe {
                status: "launch_failed",
                version: None,
            };
        }
        Err(_) => {
            return VersionProbe {
                status: "timed_out",
                version: None,
            };
        }
    };

    if !output.status.success() {
        return VersionProbe {
            status: "nonzero_exit",
            version: None,
        };
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    let line = stdout
        .lines()
        .chain(stderr.lines())
        .map(str::trim)
        .find(|line| !line.is_empty());

    let version = match command_kind {
        VersionCommand::Konnect => line.and_then(parse_konnect_version).map(str::to_string),
        VersionCommand::KiCad => line.and_then(sanitize_version_line),
    };
    VersionProbe {
        status: if version.is_some() {
            "ok"
        } else {
            "unrecognized_output"
        },
        version,
    }
}

/// The path the serving binary is installed at. On Linux `current_exe()` reads
/// `/proc/self/exe`, which gains a ` (deleted)` suffix once an update replaces
/// the file; the installed path, now holding the update, is the link without it.
pub(crate) fn installed_executable_path() -> std::io::Result<PathBuf> {
    let path = std::env::current_exe()?;
    #[cfg(any(target_os = "linux", target_os = "android"))]
    let path = without_deleted_suffix(path);
    Ok(path)
}

#[cfg(any(target_os = "linux", target_os = "android"))]
fn without_deleted_suffix(path: PathBuf) -> PathBuf {
    use std::os::unix::ffi::OsStrExt;
    match path.as_os_str().as_bytes().strip_suffix(b" (deleted)") {
        // A file really named "... (deleted)" keeps its name.
        Some(installed) if !path.exists() => PathBuf::from(std::ffi::OsStr::from_bytes(installed)),
        _ => path,
    }
}

/// Probe a Konnect executable for the version it reports. Reload validation
/// uses the same parser and timeout as installation diagnostics so the two
/// paths cannot disagree about whether a candidate is runnable.
#[cfg(unix)]
pub(crate) async fn probe_konnect_version(path: &Path) -> Result<String, &'static str> {
    let probe = probe_command_version(path, VersionCommand::Konnect).await;
    probe.version.ok_or(probe.status)
}

#[cfg(any(unix, test))]
pub(crate) fn compare_konnect_versions(candidate: &str, running: &str) -> Option<Ordering> {
    stable_version_cmp(candidate, running)
}

/// `copperpour <version>`, or `konnect <version>` from a build before the rename.
fn parse_konnect_version(line: &str) -> Option<&str> {
    let version = line
        .strip_prefix("copperpour ")
        .or_else(|| line.strip_prefix("konnect "))?
        .trim();
    (!version.is_empty() && !version.chars().any(char::is_whitespace)).then_some(version)
}

fn sanitize_version_line(line: &str) -> Option<String> {
    let value: String = line
        .chars()
        .filter(|ch| !ch.is_control())
        .take(200)
        .collect();
    (!value.is_empty()).then_some(value)
}

fn stable_version_cmp(candidate: &str, running: &str) -> Option<Ordering> {
    fn stable_triplet(version: &str) -> Option<[u64; 3]> {
        if version.contains(['-', '+']) {
            return None;
        }
        let values = version
            .strip_prefix('v')
            .unwrap_or(version)
            .split('.')
            .map(str::parse::<u64>)
            .collect::<Result<Vec<_>, _>>()
            .ok()?;
        (values.len() == 3).then(|| [values[0], values[1], values[2]])
    }

    Some(stable_triplet(candidate)?.cmp(&stable_triplet(running)?))
}

fn restart_guidance(source: &str, newer_than_running: Option<bool>) -> Vec<String> {
    let mut guidance = Vec::new();
    if newer_than_running == Some(true) {
        guidance.push(
            "A newer binary is proven at the serving executable path; restart the process before relying on the new build."
                .to_string(),
        );
    }

    #[cfg(target_os = "windows")]
    guidance.push(
        "Windows: exit every MCP client or KiCad session that launched Copperpour, then reopen the owning application; running executables may remain locked during an update."
            .to_string(),
    );
    #[cfg(target_os = "macos")]
    guidance.push(
        "macOS: restart the MCP client that launched Copperpour; if KiCad launched it, quit and reopen KiCad after the update."
            .to_string(),
    );
    #[cfg(target_os = "linux")]
    guidance.push(
        "Linux: restart the MCP client that launched Copperpour; if KiCad launched it, stop the plugin server or quit and reopen KiCad after the update."
            .to_string(),
    );
    #[cfg(not(any(target_os = "windows", target_os = "macos", target_os = "linux")))]
    guidance.push(
        "Restart the MCP client or KiCad session that launched Copperpour after replacing the binary."
            .to_string(),
    );

    if source == "kicad_pcm" {
        guidance.push(
            "KiCad PCM install detected: complete the Plugin and Content Manager update, then restart KiCad and any separately configured MCP client."
                .to_string(),
        );
    }
    guidance.push(
        "Call get_installation_info again after restart and verify the serving version, commit, and executable path."
            .to_string(),
    );
    guidance
}

fn display_path(path: &Path) -> String {
    path.to_string_lossy().into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    struct DetailProbe;

    impl GuidanceProbe for DetailProbe {
        fn detail(&self) -> Value {
            json!({"probe_status": "ok", "claude": {"state": "out_of_sync"}})
        }

        fn take_notice(&self) -> Option<String> {
            unreachable!("get_installation_info must not consume the initialize notice")
        }
    }

    #[test]
    fn guidance_block_is_the_probe_detail_or_says_it_is_unavailable() {
        assert_eq!(
            guidance_block(Some(&DetailProbe)),
            json!({"probe_status": "ok", "claude": {"state": "out_of_sync"}})
        );
        let absent = guidance_block(None);
        assert_eq!(absent["probe_status"], "not_available");
        assert!(absent["reason"]
            .as_str()
            .unwrap()
            .contains("copperpour status"));
    }

    #[cfg(any(target_os = "linux", target_os = "android"))]
    #[test]
    fn only_a_missing_path_loses_the_deleted_suffix() {
        let temp = tempfile::tempdir().unwrap();
        let real_path = temp.path().join("konnect (deleted)");
        std::fs::write(&real_path, b"").unwrap();
        assert_eq!(without_deleted_suffix(real_path.clone()), real_path);

        let unlinked_path = temp.path().join("gone (deleted)");
        assert_eq!(
            without_deleted_suffix(unlinked_path),
            temp.path().join("gone")
        );

        let missing_path = temp.path().join("missing");
        assert_eq!(without_deleted_suffix(missing_path.clone()), missing_path);
    }

    #[test]
    fn verified_pcm_manifest_is_required_for_pcm_classification() {
        let temp = tempfile::tempdir().unwrap();
        let plugin_dir = temp.path().join("plugins");
        let bin_dir = plugin_dir.join("bin");
        std::fs::create_dir_all(&bin_dir).unwrap();
        let executable = bin_dir.join(if cfg!(windows) {
            "copperpour.exe"
        } else {
            "copperpour"
        });
        std::fs::write(&executable, b"").unwrap();

        assert_eq!(classify_installation(&executable).name, "unknown");
        std::fs::write(
            plugin_dir.join("plugin.json"),
            r#"{"identifier":"someone.else","runtime":{"type":"exec"}}"#,
        )
        .unwrap();
        assert_eq!(classify_installation(&executable).name, "unknown");
        // Konnect's identifier is another project's package now.
        std::fs::write(
            plugin_dir.join("plugin.json"),
            r#"{"identifier":"com.github.mixelpixx.konnect","runtime":{"type":"exec"}}"#,
        )
        .unwrap();
        assert_eq!(classify_installation(&executable).name, "unknown");
        std::fs::write(
            plugin_dir.join("plugin.json"),
            r#"{"identifier":"com.github.copperpour.copperpour","runtime":{"type":"exec"}}"#,
        )
        .unwrap();

        let source = classify_installation(&executable);
        assert_eq!(source.name, "kicad_pcm");
        assert_eq!(source.manifest_path, Some(plugin_dir.join("plugin.json")));
    }

    #[test]
    fn newer_claim_requires_comparable_stable_versions() {
        assert_eq!(
            stable_version_cmp("0.12.0", "0.11.9"),
            Some(Ordering::Greater)
        );
        assert_eq!(
            stable_version_cmp("0.11.0", "0.11.0"),
            Some(Ordering::Equal)
        );
        assert_eq!(stable_version_cmp("0.11.0-beta.1", "0.10.0"), None);
        assert_eq!(stable_version_cmp("not-a-version", "0.11.0"), None);
    }

    #[test]
    fn a_version_line_under_either_name_is_read() {
        assert_eq!(parse_konnect_version("copperpour 0.15.0"), Some("0.15.0"));
        assert_eq!(parse_konnect_version("konnect 0.14.0"), Some("0.14.0"));
        assert_eq!(parse_konnect_version("kicad-cli 10.0.1"), None);
        assert_eq!(parse_konnect_version("copperpour "), None);
    }

    #[test]
    fn embedded_commit_is_hex_when_available() {
        if let Some(commit) = option_env!("COPPERPOUR_BUILD_COMMIT") {
            assert!((7..=64).contains(&commit.len()));
            assert!(commit.bytes().all(|byte| byte.is_ascii_hexdigit()));
        }
    }

    #[test]
    fn restart_guidance_names_the_current_platform() {
        let guidance = restart_guidance("unknown", None).join("\n");
        #[cfg(target_os = "windows")]
        assert!(guidance.contains("Windows:"));
        #[cfg(target_os = "macos")]
        assert!(guidance.contains("macOS:"));
        #[cfg(target_os = "linux")]
        assert!(guidance.contains("Linux:"));
    }
}
