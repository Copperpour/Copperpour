//! Freerouting: finding the JAR and its Java runtime, and routing a Specctra
//! DSN through the JAR's headless MCP server ([`mcp`]).
//!
//! The `check_freerouting` and `route_specctra_dsn` tools stay registered in
//! the `integration` toolset (#209). Their handlers there delegate to
//! [`check_freerouting`] and [`route_specctra_dsn`].

pub(crate) mod mcp;

use crate::mcp::{error::ToolErrorKind, protocol::CallToolResult};
use crate::tools::{get_path, ToolContext};
use serde_json::json;
use std::path::{Path, PathBuf};

fn is_freerouting_jar(path: &Path) -> bool {
    path.is_file()
        && path
            .extension()
            .is_some_and(|extension| extension.eq_ignore_ascii_case("jar"))
        && path
            .file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| name.to_ascii_lowercase().contains("freerouting"))
}

fn find_freerouting_jar_below(root: &Path, remaining_depth: usize) -> Option<PathBuf> {
    if is_freerouting_jar(root) {
        return Some(root.to_path_buf());
    }
    if remaining_depth == 0 || !root.is_dir() {
        return None;
    }

    let mut entries = std::fs::read_dir(root)
        .ok()?
        .flatten()
        .map(|entry| entry.path())
        .collect::<Vec<_>>();
    entries.sort();

    entries
        .iter()
        .find(|path| is_freerouting_jar(path))
        .cloned()
        .or_else(|| {
            entries.into_iter().find_map(|path| {
                let file_type = std::fs::symlink_metadata(&path).ok()?.file_type();
                file_type
                    .is_dir()
                    .then(|| find_freerouting_jar_below(&path, remaining_depth - 1))
                    .flatten()
            })
        })
}

/// Where Freerouting discovery looks when the caller passes no `jar_path`, in
/// the order it looks. The concrete places come from the tables below, and
/// both the search and the `jar_path` description are built from those tables,
/// so what the schema says and what the search does cannot drift apart. There
/// is no configuration setting: the schema once said "uses config default" for
/// one that never existed (#787).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum FreeroutingLocation {
    KicadThirdPartyVariable,
    KicadPluginFolder,
    WorkingDirectory,
    SystemDirectory,
    PathDirectory,
}

/// The variables naming a KiCad third-party directory, newest KiCad first.
const FREEROUTING_THIRD_PARTY_VARIABLES: [&str; 3] =
    ["KICAD10_3RD_PARTY", "KICAD9_3RD_PARTY", "KICAD8_3RD_PARTY"];

/// The KiCad versions whose plugin folders are searched, newest first.
const FREEROUTING_KICAD_VERSIONS: [&str; 3] = ["10.0", "9.0", "8.0"];

/// Plugin folders below a home directory: the variable naming the home, the
/// folder below it (`<version>` stands for each version), and whether it is
/// searched on Windows only. A home that is not absolute is skipped.
const FREEROUTING_PLUGIN_FOLDERS: [(&str, &str, bool); 3] = [
    ("HOME", "Documents/KiCad/<version>/3rdparty/plugins", false),
    (
        "HOME",
        ".local/share/kicad/<version>/3rdparty/plugins",
        false,
    ),
    (
        "USERPROFILE",
        "Documents/KiCad/<version>/3rdparty/plugins",
        true,
    ),
];

/// The JAR looked for in the server's working directory.
const FREEROUTING_WORKING_DIRECTORY_JAR: &str = "freerouting.jar";

/// System install directories.
const FREEROUTING_SYSTEM_DIRECTORIES: [&str; 2] =
    ["/usr/local/lib/freerouting", "/opt/freerouting"];

/// The variable whose directories are searched last.
const FREEROUTING_PATH_VARIABLE: &str = "PATH";

impl FreeroutingLocation {
    /// Every location, in search order.
    const ALL: [Self; 5] = [
        Self::KicadThirdPartyVariable,
        Self::KicadPluginFolder,
        Self::WorkingDirectory,
        Self::SystemDirectory,
        Self::PathDirectory,
    ];

    fn kind(self) -> &'static str {
        match self {
            Self::KicadThirdPartyVariable => "kicad_3rd_party_variable",
            Self::KicadPluginFolder => "kicad_plugin_folder",
            Self::WorkingDirectory => "working_directory",
            Self::SystemDirectory => "system_directory",
            Self::PathDirectory => "path_directory",
        }
    }

    /// How many directory levels below one of these roots are searched.
    fn depth(self) -> usize {
        match self {
            Self::KicadThirdPartyVariable | Self::KicadPluginFolder => 5,
            Self::WorkingDirectory => 0,
            Self::SystemDirectory => 3,
            Self::PathDirectory => 1,
        }
    }

    fn description(self) -> String {
        match self {
            Self::KicadThirdPartyVariable => format!(
                "the directories named by {}",
                english_list(
                    FREEROUTING_THIRD_PARTY_VARIABLES
                        .iter()
                        .map(|v| v.to_string())
                )
            ),
            Self::KicadPluginFolder => format!(
                "the KiCad {} plugin folders {}",
                english_list(FREEROUTING_KICAD_VERSIONS.iter().map(|v| v.to_string())),
                english_list(FREEROUTING_PLUGIN_FOLDERS.iter().map(
                    |(home, folder, windows_only)| {
                        let place = format!("${home}/{folder}");
                        if *windows_only {
                            format!("{place} (Windows only)")
                        } else {
                            place
                        }
                    }
                ))
            ),
            Self::WorkingDirectory => {
                format!("{FREEROUTING_WORKING_DIRECTORY_JAR} in the server's working directory")
            }
            Self::SystemDirectory => {
                english_list(FREEROUTING_SYSTEM_DIRECTORIES.iter().map(|d| d.to_string()))
            }
            Self::PathDirectory => format!("every directory on {FREEROUTING_PATH_VARIABLE}"),
        }
    }
}

/// `a`, `a and b`, or `a, b and c`.
fn english_list(items: impl Iterator<Item = String>) -> String {
    let items: Vec<String> = items.collect();
    match items.as_slice() {
        [] => String::new(),
        [only] => only.clone(),
        [rest @ .., last] => format!("{} and {last}", rest.join(", ")),
    }
}

pub(crate) fn freerouting_jar_path_description() -> String {
    let locations = FreeroutingLocation::ALL
        .iter()
        .map(|location| location.description())
        .collect::<Vec<_>>()
        .join("; ");
    format!(
        "Path to a Freerouting JAR (optional). When omitted, Copperpour searches, in order: \
         {locations}. Relative paths are taken from the server's working directory. \
         There is no configuration setting for it."
    )
}

/// One place discovery looked, and how many directory levels below it.
#[derive(Debug, Clone, PartialEq)]
struct FreeroutingSearchRoot {
    location: FreeroutingLocation,
    /// Always absolute: a relative `KICAD*_3RD_PARTY` or `PATH` entry is taken
    /// from the server's working directory, which is where the search reads
    /// it. A relative home is skipped instead (#179).
    path: PathBuf,
    depth: usize,
}

impl FreeroutingSearchRoot {
    fn evidence(&self) -> serde_json::Value {
        json!({
            "kind": self.location.kind(),
            "location_path": self.path,
            "max_depth": self.depth,
            "exists": self.path.exists(),
        })
    }
}

fn freerouting_search_roots() -> Vec<FreeroutingSearchRoot> {
    freerouting_search_roots_from(
        // On Unix, `HOME` as everywhere else: the account's home when unset.
        |name| match name {
            "HOME" if cfg!(unix) => crate::app_dirs::home_dir().map(Into::into),
            _ => std::env::var_os(name),
        },
        &std::env::current_dir().unwrap_or_default(),
    )
}

/// The search roots for a given environment, so a test can supply one without
/// touching the process's own.
fn freerouting_search_roots_from(
    var: impl Fn(&str) -> Option<std::ffi::OsString>,
    working_directory: &Path,
) -> Vec<FreeroutingSearchRoot> {
    let mut roots: Vec<FreeroutingSearchRoot> = Vec::new();
    let mut push = |location: FreeroutingLocation, path: PathBuf| {
        let path = if path.is_absolute() {
            path
        } else {
            working_directory.join(path)
        };
        // `HOME` and `USERPROFILE` are often the same directory (Git Bash on
        // Windows sets both), and searching a root twice only repeats it in
        // the evidence.
        if !roots.iter().any(|root| root.path == path) {
            roots.push(FreeroutingSearchRoot {
                location,
                depth: location.depth(),
                path,
            })
        }
    };
    let below = |base: PathBuf, folder: &str| folder.split('/').fold(base, |p, part| p.join(part));

    for variable in FREEROUTING_THIRD_PARTY_VARIABLES {
        if let Some(path) = var(variable) {
            push(
                FreeroutingLocation::KicadThirdPartyVariable,
                PathBuf::from(path),
            );
        }
    }

    let mut homes: Vec<&str> = FREEROUTING_PLUGIN_FOLDERS
        .iter()
        .map(|(home, _, _)| *home)
        .collect();
    homes.dedup();
    for home_variable in homes {
        // A relative home is no home, not a folder below the cwd (#179).
        let Some(home) = var(home_variable)
            .map(PathBuf::from)
            .filter(|home| home.is_absolute())
        else {
            continue;
        };
        for version in FREEROUTING_KICAD_VERSIONS {
            for (variable, folder, windows_only) in FREEROUTING_PLUGIN_FOLDERS {
                if variable != home_variable || (windows_only && !cfg!(target_os = "windows")) {
                    continue;
                }
                push(
                    FreeroutingLocation::KicadPluginFolder,
                    below(home.clone(), &folder.replace("<version>", version)),
                );
            }
        }
    }

    push(
        FreeroutingLocation::WorkingDirectory,
        PathBuf::from(FREEROUTING_WORKING_DIRECTORY_JAR),
    );
    for directory in FREEROUTING_SYSTEM_DIRECTORIES {
        push(
            FreeroutingLocation::SystemDirectory,
            PathBuf::from(directory),
        );
    }
    if let Some(path) = var(FREEROUTING_PATH_VARIABLE) {
        for directory in std::env::split_paths(&path) {
            push(FreeroutingLocation::PathDirectory, directory);
        }
    }
    roots
}

/// What looking for the Freerouting JAR found.
#[derive(Debug, PartialEq)]
enum FreeroutingJar {
    Found(PathBuf),
    /// The caller passed `jar_path`, and it is not a file.
    SuppliedPathIsNotAFile(PathBuf),
    /// Discovery searched every root and found no JAR.
    NotFound(Vec<FreeroutingSearchRoot>),
}

fn resolve_freerouting_jar(args: &serde_json::Value) -> FreeroutingJar {
    if let Some(path) = args["jar_path"].as_str() {
        let path = PathBuf::from(path);
        return if path.is_file() {
            FreeroutingJar::Found(path)
        } else {
            FreeroutingJar::SuppliedPathIsNotAFile(path)
        };
    }

    let roots = freerouting_search_roots();
    match roots
        .iter()
        .find_map(|root| find_freerouting_jar_below(&root.path, root.depth))
    {
        Some(jar) => FreeroutingJar::Found(jar),
        None => FreeroutingJar::NotFound(roots),
    }
}

fn command_output(output: &std::process::Output) -> String {
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    [stdout.trim(), stderr.trim()]
        .into_iter()
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>()
        .join("\n")
}

async fn run_java_command(
    command: &mut tokio::process::Command,
) -> Result<std::process::Output, String> {
    command.kill_on_drop(true);
    match tokio::time::timeout(std::time::Duration::from_secs(10), command.output()).await {
        Ok(Ok(output)) => Ok(output),
        Ok(Err(error)) => Err(error.to_string()),
        Err(_) => Err("Java command timed out after 10 seconds".to_string()),
    }
}

/// The `check_freerouting` tool, which `integration`'s handler delegates to.
pub(crate) async fn check_freerouting(
    args: &serde_json::Value,
    _ctx: &ToolContext,
) -> anyhow::Result<CallToolResult> {
    match resolve_freerouting_jar(args) {
        FreeroutingJar::SuppliedPathIsNotAFile(path) => Ok(CallToolResult::json(&json!({
            "available": false,
            "engine_found": false,
            "native_mcp_available": false,
            "bridge_available": false,
            "checked_path": path,
            "note": format!("jar_path is not a file: {}", path.display())
        }))),
        FreeroutingJar::NotFound(roots) => Ok(CallToolResult::json(&json!({
            "available": false,
            "engine_found": false,
            "native_mcp_available": false,
            "bridge_available": false,
            "searched_locations": roots.iter().map(FreeroutingSearchRoot::evidence).collect::<Vec<_>>(),
            "note": "No Freerouting JAR was found in any searched location. Install Freerouting \
                     (KiCad's Plugin and Content Manager, or \
                     https://github.com/freerouting/freerouting/releases), or pass jar_path."
        }))),
        FreeroutingJar::Found(jar_path) => {
            let java_path = crate::java_runtime::java_executable();
            let mut java_command = tokio::process::Command::new(&java_path);
            java_command.arg("-version");
            let java = match run_java_command(&mut java_command).await {
                Ok(output) => output,
                Err(error) => {
                    return Ok(CallToolResult::json(&json!({
                        "available": false,
                        "engine_found": true,
                        "native_mcp_available": false,
                        "bridge_available": false,
                        "jar_path": jar_path,
                        "java_path": java_path,
                        "java_available": false,
                        "note": error
                    })));
                }
            };
            if !java.status.success() {
                return Ok(CallToolResult::json(&json!({
                    "available": false,
                    "engine_found": true,
                    "native_mcp_available": false,
                    "bridge_available": false,
                    "jar_path": jar_path,
                    "java_path": java_path,
                    "java_available": false,
                    "java_output": command_output(&java),
                    "note": "Freerouting was found, but Java did not start successfully"
                })));
            }

            // An older Java only fails once the JAR is loaded, with
            // UnsupportedClassVersionError: say so before anything else runs.
            let java_release = crate::java_runtime::java_release(&command_output(&java));
            let required_release = crate::java_runtime::required_java_release(&jar_path);
            if let (Some(found), Some(required)) = (java_release, required_release) {
                if found < required {
                    return Ok(CallToolResult::json(&json!({
                        "available": false,
                        "engine_found": true,
                        "native_mcp_available": false,
                        "bridge_available": false,
                        "jar_path": jar_path,
                        "java_path": java_path,
                        "java_available": true,
                        "java_release": found,
                        "required_java_release": required,
                        "note": format!(
                            "This Freerouting JAR needs Java {required} or later, but {} is Java {found}. \
                             Install a newer JDK, or point JAVA_HOME at one.",
                            java_path.display()
                        )
                    })));
                }
            }

            let mut freerouting_command = tokio::process::Command::new(&java_path);
            freerouting_command.args([
                mcp::JAVA_HEADLESS_ARG,
                "-jar",
                jar_path.to_str().unwrap_or(""),
                "--version",
            ]);
            let freerouting = run_java_command(&mut freerouting_command).await;
            let (version_checked, version_output) = match freerouting {
                Ok(output) => (output.status.success(), command_output(&output)),
                Err(error) => (false, error),
            };

            let bridge = mcp::probe_local(&jar_path).await;
            // Available means the JAR actually ran here, not merely that a JAR
            // and some `java` exist.
            let note = (!version_checked).then(|| {
                format!(
                    "Java started, but the Freerouting JAR did not run with {} (`--version` \
                     failed): see version_output",
                    java_path.display()
                )
            });
            let mut body = json!({
                "available": version_checked,
                "engine_found": true,
                "native_mcp_available": bridge.native_mcp_available,
                "bridge_available": bridge.bridge_available,
                "jar_path": jar_path,
                "java_path": java_path,
                "java_available": true,
                "java_release": java_release,
                "required_java_release": required_release,
                "java_output": command_output(&java),
                "version_checked": version_checked,
                "version_output": version_output,
                "server_protocol_version": bridge.server_protocol_version,
                "native_mcp_tool_count": bridge.tool_count,
                "native_mcp_diagnostics": bridge.diagnostics,
                "bridge_error": bridge.error
            });
            // The key stays absent when the JAR ran, as before this change.
            if let Some(note) = note {
                body["note"] = json!(note);
            }
            Ok(CallToolResult::json(&body))
        }
    }
}

/// The `route_specctra_dsn` tool, which `integration`'s handler delegates to.
pub(crate) async fn route_specctra_dsn(
    args: &serde_json::Value,
    _ctx: &ToolContext,
) -> anyhow::Result<CallToolResult> {
    let dsn = get_path(args, "dsn_path")?;
    let ses_output = get_path(args, "ses_output_path")?;
    // The schema bounds the threshold to 0..=100 but cannot express this gap,
    // so it is refused here, before the JAR is even looked up. The threshold
    // checked is the one sent: the named argument wins over router_settings.
    let threshold_sent = match args["optimizer_improvement_threshold"].as_f64() {
        Some(threshold) => Some(("optimizer_improvement_threshold", threshold)),
        None => args["router_settings"]
            .as_object()
            .and_then(mcp::router_settings_threshold)
            .map(|threshold| (mcp::ROUTER_SETTINGS_THRESHOLD, threshold)),
    };
    if let Some((field, threshold)) =
        threshold_sent.filter(|(_, threshold)| mcp::threshold_read_as_fraction(*threshold))
    {
        return Ok(CallToolResult::error_kind(
            ToolErrorKind::InvalidArgument {
                field: field.to_string(),
                reason: mcp::THRESHOLD_READ_AS_FRACTION.to_string(),
            },
            format!(
                "{field} is {threshold}: a value {}. Nothing was routed.",
                mcp::THRESHOLD_READ_AS_FRACTION
            ),
        ));
    }
    let max_passes = args["max_passes"]
        .as_u64()
        .map(u32::try_from)
        .transpose()
        .map_err(|_| anyhow::anyhow!("max_passes is too large"))?;
    let job_timeout_seconds = args["job_timeout_seconds"].as_u64();
    let overall_timeout_seconds = args["overall_timeout_seconds"].as_u64().unwrap_or(900);
    let settings = mcp::RouteSettings {
        max_passes,
        optimizer_enabled: args["optimizer_enabled"].as_bool(),
        job_timeout_seconds,
        optimize_seconds: args["optimize_seconds"].as_u64(),
        optimizer_max_passes: args["optimizer_max_passes"]
            .as_u64()
            .map(u32::try_from)
            .transpose()
            .map_err(|_| anyhow::anyhow!("optimizer_max_passes is too large"))?,
        optimizer_improvement_threshold: args["optimizer_improvement_threshold"].as_f64(),
        plane_nets: args["plane_nets"].as_array().map(|nets| {
            nets.iter()
                .filter_map(|net| net.as_str())
                .map(str::to_string)
                .collect()
        }),
        plane_as_obstacle: args["plane_as_obstacle"].as_bool(),
        router_settings: args["router_settings"].as_object().cloned(),
        poll_interval: std::time::Duration::from_secs(3),
        overall_timeout: std::time::Duration::from_secs(overall_timeout_seconds),
    };
    // Like the threshold above, refused before the JAR lookup; the values
    // checked are the ones sent, so a named argument that replaces a negative
    // router_settings value clears it.
    let negative = mcp::negative_settings_sent(&settings);
    if let Some((field, _)) = negative.first() {
        return Ok(CallToolResult::error_kind(
            ToolErrorKind::InvalidArgument {
                field: field.clone(),
                reason: mcp::NEGATIVE_SETTING.to_string(),
            },
            mcp::negative_settings_message(&negative),
        ));
    }
    let jar = match resolve_freerouting_jar(args) {
        FreeroutingJar::Found(jar) => jar,
        FreeroutingJar::SuppliedPathIsNotAFile(path) => {
            return Ok(CallToolResult::error_kind(
                ToolErrorKind::FileNotFound {
                    path: path.display().to_string(),
                },
                format!("jar_path is not a file: {}", path.display()),
            ));
        }
        FreeroutingJar::NotFound(roots) => {
            return Ok(CallToolResult::error(format!(
                "Freerouting JAR not found in {} searched locations; install Freerouting or \
                 pass jar_path. check_freerouting lists every location searched.",
                roots.len()
            )));
        }
    };
    let evidence = mcp::route_local(&jar, &dsn, &ses_output, &settings)
        .await
        .map_err(|error| anyhow::anyhow!("Freerouting MCP routing failed: {error:#}"))?;
    Ok(CallToolResult::json(&route_response(
        &evidence,
        &jar,
        &dsn,
        &ses_output,
    )))
}

/// The `route_specctra_dsn` answer for a finished job.
///
/// A board with unrouted connections is not a routed board, even though
/// Freerouting ends the job COMPLETED (max_passes reached, or no progress):
/// `success` is false, `message` says how many are left, and the SES is still
/// written, for inspection. An unknown count (log not readable) is not turned
/// into a failure, but `message` says it was not checked: "routed" and "not
/// counted" must not give the same answer.
fn route_response(
    evidence: &mcp::RouteEvidence,
    jar: &Path,
    dsn: &Path,
    ses_output: &Path,
) -> serde_json::Value {
    let fully_routed = evidence.unrouted.is_none_or(|unrouted| unrouted == 0);
    let message = match evidence.unrouted {
        Some(0) => None,
        Some(unrouted) => Some(format!(
            "Freerouting left {unrouted} connection(s) unrouted; the SES at {} is incomplete",
            ses_output.display()
        )),
        None => Some(format!(
            "the unrouted count could not be read from Freerouting's log ({}); the SES at {} \
             was not checked for completeness",
            evidence.freerouting_log.as_deref().unwrap_or("no log"),
            ses_output.display()
        )),
    };
    json!({
        "success": fully_routed,
        "message": message,
        "unrouted_connections": evidence.unrouted,
        "method": "local_freerouting_native_mcp",
        "engine": {
            "name": "Freerouting",
            "jar_path": jar,
            "execution": "local"
        },
        "native_mcp": {
            "used": true,
            "server_protocol_version": evidence.server_protocol_version
        },
        "bridge": {
            "mode": "dsn_ses_file_round_trip",
            "cloud_used": false
        },
        "artifacts": {
            "dsn_path": dsn,
            "ses_output_path": ses_output,
            "diagnostics_path": evidence.diagnostics_path
        },
        "session_id": evidence.session_id,
        "job_id": evidence.job_id,
        "final_state": evidence.final_state,
        "poll_count": evidence.poll_count,
        "elapsed_seconds": evidence.elapsed_seconds,
        "ses_bytes": evidence.ses_bytes,
        "last_activity": evidence.last_activity,
        "settings_checked": evidence.settings_checked,
        // Kept for callers that read it: a setting Freerouting did not apply
        // now fails the call before routing, so a routed board has none.
        "settings_not_applied": [],
        "settings_warnings": evidence.settings_warnings
    })
}

#[cfg(test)]
mod freerouting_tests {
    use super::*;
    use crate::router::ToolRouter;
    use crate::tools::ServerConfig;
    use std::collections::HashMap;
    use std::sync::Arc;

    /// What `route_local` hands back for a job that ended COMPLETED, with
    /// `unrouted` connections left according to Freerouting's log.
    fn completed_job(unrouted: Option<u64>) -> mcp::RouteEvidence {
        mcp::RouteEvidence {
            session_id: "session".into(),
            job_id: "a3aa32d0".into(),
            final_state: "COMPLETED".into(),
            poll_count: 12,
            elapsed_seconds: 300.0,
            ses_bytes: 4096,
            server_protocol_version: "2025-06-18".into(),
            diagnostics_path: None,
            last_activity: Some("pass 30: 3 unrouted, 218 violations".into()),
            unrouted,
            freerouting_log: Some("/state/freerouting/logs/freerouting.log".into()),
            settings_checked: false,
            settings_warnings: Vec::new(),
        }
    }

    fn respond(unrouted: Option<u64>) -> serde_json::Value {
        route_response(
            &completed_job(unrouted),
            Path::new("/opt/freerouting.jar"),
            Path::new("/work/board.dsn"),
            Path::new("/work/board.ses"),
        )
    }

    /// Freerouting ends a job COMPLETED when max_passes runs out or no pass
    /// makes progress, whatever is left unrouted: that is not success.
    #[test]
    fn a_completed_job_with_unrouted_connections_is_not_a_success() {
        let body = respond(Some(3));
        assert_eq!(body["success"], false, "{body}");
        assert_eq!(body["final_state"], "COMPLETED", "{body}");
        assert_eq!(body["unrouted_connections"], 3, "{body}");
        let message = body["message"].as_str().expect("a message says why");
        assert!(message.contains("3 connection(s) unrouted"), "{message}");
        assert!(message.contains("/work/board.ses"), "{message}");
        // The SES is still reported, for inspection.
        assert_eq!(body["artifacts"]["ses_output_path"], "/work/board.ses");
        assert_eq!(body["ses_bytes"], 4096);
    }

    #[test]
    fn a_completed_job_with_every_connection_routed_is_a_success() {
        let body = respond(Some(0));
        assert_eq!(body["success"], true, "{body}");
        assert_eq!(body["unrouted_connections"], 0, "{body}");
        assert!(body["message"].is_null(), "{body}");
    }

    /// Settings Freerouting did not apply fail the call before routing, so a
    /// routed board has none; the key stays for callers that read it.
    /// Whether they were read back at all reaches the caller as is.
    #[test]
    fn a_routed_board_keeps_an_empty_settings_not_applied() {
        let body = respond(Some(0));
        assert_eq!(body["settings_not_applied"], json!([]), "{body}");
        assert_eq!(body["settings_checked"], false, "{body}");
        let mut evidence = completed_job(Some(0));
        evidence.settings_checked = true;
        evidence.settings_warnings = vec![json!("max_threads above the CPU count")];
        let body = route_response(
            &evidence,
            Path::new("/opt/freerouting.jar"),
            Path::new("/work/board.dsn"),
            Path::new("/work/board.ses"),
        );
        assert_eq!(body["settings_checked"], true, "{body}");
        assert_eq!(body["settings_not_applied"], json!([]), "{body}");
        assert_eq!(
            body["settings_warnings"],
            json!(["max_threads above the CPU count"]),
            "{body}"
        );
    }

    /// A count that could not be read from the log is unknown, not a failure,
    /// and the message says so: "routed" and "not counted" differ.
    #[test]
    fn an_unknown_unrouted_count_leaves_success_true() {
        let body = respond(None);
        assert_eq!(body["success"], true, "{body}");
        assert!(body["unrouted_connections"].is_null(), "{body}");
        let message = body["message"]
            .as_str()
            .expect("a message says it was not counted");
        assert!(message.contains("could not be read"), "{message}");
        assert!(
            message.contains("/state/freerouting/logs/freerouting.log"),
            "{message}"
        );
        assert!(message.contains("/work/board.ses"), "{message}");

        let mut evidence = completed_job(None);
        evidence.freerouting_log = None;
        let body = route_response(
            &evidence,
            Path::new("/opt/freerouting.jar"),
            Path::new("/work/board.dsn"),
            Path::new("/work/board.ses"),
        );
        let message = body["message"].as_str().expect("a message without a log");
        assert!(message.contains("(no log)"), "{message}");
    }

    fn test_ctx() -> ToolContext {
        ToolContext::new(
            ServerConfig {
                kicad_cli: String::new(),
                kicad_binary: String::new(),
                ipc_address: String::new(),
                project_dir: None,
                jlcpcb_db_path: None,
                auto_load_toolsets: false,
                eager_toolsets: false,
            },
            Arc::new(ToolRouter::new()),
        )
    }

    fn response_json(result: &CallToolResult) -> serde_json::Value {
        match &result.content[0] {
            crate::mcp::protocol::ToolContent::Text { text } => serde_json::from_str(text).unwrap(),
            _ => panic!("expected text content"),
        }
    }

    #[test]
    fn finds_versioned_jar_inside_pcm_plugin_tree() {
        let temp = tempfile::tempdir().unwrap();
        let plugin = temp.path().join("app_freerouting_kicad-plugin").join("lib");
        std::fs::create_dir_all(&plugin).unwrap();
        let jar = plugin.join("freerouting-2.3.0.jar");
        std::fs::write(&jar, b"fixture").unwrap();

        assert_eq!(find_freerouting_jar_below(temp.path(), 5), Some(jar));
    }

    #[tokio::test]
    async fn explicit_missing_jar_reports_each_readiness_boundary() {
        let temp = tempfile::tempdir().unwrap();
        let missing = temp.path().join("missing.jar");
        assert_eq!(
            resolve_freerouting_jar(&json!({ "jar_path": missing })),
            FreeroutingJar::SuppliedPathIsNotAFile(missing.clone())
        );

        let result = check_freerouting(&json!({ "jar_path": missing }), &test_ctx())
            .await
            .unwrap();
        let body = response_json(&result);
        assert_eq!(body["available"], false);
        assert_eq!(body["engine_found"], false);
        assert_eq!(body["native_mcp_available"], false);
        assert_eq!(body["bridge_available"], false);
        // The caller's path is what was checked; discovery never ran, so
        // there is no search to report and no download advice to give.
        assert_eq!(body["checked_path"], json!(missing));
        assert_eq!(
            body["note"],
            json!(format!("jar_path is not a file: {}", missing.display()))
        );
        assert!(body.get("searched_locations").is_none(), "{body}");
    }

    /// A full environment, so every location the search can visit is present.
    fn every_location_environment(root: &Path) -> HashMap<&'static str, std::ffi::OsString> {
        let path = std::env::join_paths([root.join("bin-a"), root.join("bin-b")]).unwrap();
        HashMap::from([
            ("KICAD10_3RD_PARTY", root.join("k10").into_os_string()),
            ("KICAD9_3RD_PARTY", root.join("k9").into_os_string()),
            ("KICAD8_3RD_PARTY", root.join("k8").into_os_string()),
            ("HOME", root.join("home").into_os_string()),
            ("USERPROFILE", root.join("profile").into_os_string()),
            ("PATH", path),
        ])
    }

    /// #787: the `jar_path` description is the documentation of the search.
    /// The search visits locations in the order the description lists them,
    /// visits every location it lists, and the description promises no
    /// configuration setting.
    #[test]
    fn the_search_visits_the_documented_locations_in_the_documented_order() {
        let temp = tempfile::tempdir().unwrap();
        let env = every_location_environment(temp.path());
        let roots =
            freerouting_search_roots_from(|name| env.get(name).cloned(), &temp.path().join("cwd"));

        let visited: Vec<FreeroutingLocation> = roots.iter().map(|root| root.location).collect();
        assert!(
            visited.windows(2).all(|pair| pair[0] <= pair[1]),
            "search order differs from the documented order: {visited:?}"
        );
        let mut distinct = visited.clone();
        distinct.dedup();
        assert_eq!(distinct, FreeroutingLocation::ALL);

        let description = freerouting_jar_path_description();
        let positions: Vec<usize> = FreeroutingLocation::ALL
            .iter()
            .map(|location| {
                description
                    .find(location.description().as_str())
                    .unwrap_or_else(|| panic!("{location:?} is not documented: {description}"))
            })
            .collect();
        assert!(
            positions.windows(2).all(|pair| pair[0] < pair[1]),
            "{description}"
        );
        assert!(!description.contains("config default"), "{description}");
        assert!(description.contains("There is no configuration setting"));

        // The working-directory root is absolute, so `searched` names the
        // file that was actually checked.
        assert!(roots.contains(&FreeroutingSearchRoot {
            location: FreeroutingLocation::WorkingDirectory,
            path: temp.path().join("cwd").join("freerouting.jar"),
            depth: 0,
        }));
        assert_eq!(
            roots
                .iter()
                .filter(|root| root.location == FreeroutingLocation::PathDirectory)
                .map(|root| root.path.clone())
                .collect::<Vec<_>>(),
            [temp.path().join("bin-a"), temp.path().join("bin-b")]
        );
    }

    /// The tables name concrete places, not only their categories: every
    /// variable, KiCad version, plugin folder, file and directory the search
    /// visits is named in the description, and the search visits each one.
    #[test]
    fn the_description_names_every_concrete_place_the_search_visits() {
        let temp = tempfile::tempdir().unwrap();
        let env = every_location_environment(temp.path());
        let cwd = temp.path().join("cwd");
        let roots = freerouting_search_roots_from(|name| env.get(name).cloned(), &cwd);
        let searched = |path: PathBuf| roots.iter().any(|root| root.path == path);
        let description = freerouting_jar_path_description();
        let named = |text: &str| {
            assert!(
                description.contains(text),
                "{text} is not named: {description}"
            )
        };

        for variable in FREEROUTING_THIRD_PARTY_VARIABLES {
            named(variable);
            assert!(
                searched(PathBuf::from(&env[variable])),
                "{variable}: {roots:?}"
            );
        }
        for version in FREEROUTING_KICAD_VERSIONS {
            named(version);
        }
        for (home, folder, windows_only) in FREEROUTING_PLUGIN_FOLDERS {
            named(&format!("${home}/{folder}"));
            if windows_only && !cfg!(target_os = "windows") {
                continue;
            }
            for version in FREEROUTING_KICAD_VERSIONS {
                let place = folder
                    .replace("<version>", version)
                    .split('/')
                    .fold(PathBuf::from(&env[home]), |p, part| p.join(part));
                assert!(searched(place.clone()), "{place:?}: {roots:?}");
            }
        }
        named(FREEROUTING_WORKING_DIRECTORY_JAR);
        assert!(searched(cwd.join(FREEROUTING_WORKING_DIRECTORY_JAR)));
        for directory in FREEROUTING_SYSTEM_DIRECTORIES {
            named(directory);
            assert!(searched(cwd.join(directory)), "{directory}: {roots:?}");
        }
        named(FREEROUTING_PATH_VARIABLE);
        for directory in std::env::split_paths(&env[FREEROUTING_PATH_VARIABLE]) {
            assert!(searched(directory.clone()), "{directory:?}: {roots:?}");
        }
    }

    /// With `HOME` unset, the plugin folders below the account's home are
    /// still searched, as every other per-user path is (#179). Run in a child
    /// process, since `HOME` is process-wide.
    #[cfg(unix)]
    #[test]
    fn an_unset_home_searches_the_account_home() {
        const CHILD: &str = "COPPERPOUR_TEST_FREEROUTING_HOME_CHILD";
        if std::env::var_os(CHILD).is_none() {
            // The test's path below the crate, so the child runs this test
            // wherever the module lives.
            let module = module_path!().split_once("::").expect("crate::module").1;
            let test = format!("{module}::an_unset_home_searches_the_account_home");
            let output = std::process::Command::new(std::env::current_exe().unwrap())
                .args(["--exact", test.as_str(), "--nocapture"])
                .env(CHILD, "1")
                .env_remove("HOME")
                .output()
                .unwrap();
            let stdout = String::from_utf8_lossy(&output.stdout);
            assert!(
                output.status.success() && stdout.contains("1 passed"),
                "{stdout}\n{}",
                String::from_utf8_lossy(&output.stderr)
            );
            return;
        }
        let home = crate::app_dirs::home_dir().expect("the account has a home directory");
        let folder = home.join(".local/share/kicad/10.0/3rdparty/plugins");
        assert!(
            freerouting_search_roots()
                .iter()
                .any(|root| root.path == folder),
            "{folder:?} was not searched"
        );
    }

    /// Every searched location is reported as an absolute path. A relative
    /// `KICAD*_3RD_PARTY` or `PATH` entry is taken from the server's working
    /// directory, which is where the search reads it; a relative `HOME` or
    /// `USERPROFILE` is no home directory, so nothing below it is searched
    /// (#179).
    #[test]
    fn every_searched_location_is_absolute() {
        let temp = tempfile::tempdir().unwrap();
        let cwd = temp.path().join("cwd");
        let env = HashMap::from([
            ("KICAD10_3RD_PARTY", std::ffi::OsString::from("third-party")),
            ("HOME", std::ffi::OsString::from("home")),
            ("USERPROFILE", std::ffi::OsString::from("profile")),
            (
                "PATH",
                std::env::join_paths([PathBuf::from("bin"), PathBuf::from(".")]).unwrap(),
            ),
        ]);
        let roots = freerouting_search_roots_from(|name| env.get(name).cloned(), &cwd);
        for root in &roots {
            assert!(root.path.is_absolute(), "{root:?}");
        }
        assert!(roots
            .iter()
            .any(|root| root.path == cwd.join("third-party")));
        assert!(roots.iter().any(|root| root.path == cwd.join("bin")));
        for home in ["home", "profile"] {
            assert!(
                !roots
                    .iter()
                    .any(|root| root.path.starts_with(cwd.join(home))),
                "{home}: {roots:?}"
            );
        }
        let evidence = roots[0].evidence();
        assert_eq!(evidence["location_path"], json!(cwd.join("third-party")));
        assert!(evidence.get("path").is_none(), "{evidence}");
    }

    /// `check_freerouting` with a `jar_path` that is not a file, through the
    /// served `tools/call`: the path checked is named, and no search ran.
    #[tokio::test]
    async fn served_check_freerouting_reports_a_jar_path_that_is_not_a_file() {
        let temp = tempfile::tempdir().unwrap();
        let missing = temp.path().join("missing.jar");
        let handler = crate::mcp::handler::McpHandler::new(ServerConfig {
            kicad_cli: String::new(),
            kicad_binary: String::new(),
            ipc_address: String::new(),
            project_dir: None,
            jlcpcb_db_path: None,
            auto_load_toolsets: true,
            eager_toolsets: true,
        })
        .await
        .expect("handler builds");
        let response = handler
            .handle_message(json!({
                "jsonrpc": "2.0",
                "id": 1,
                "method": "tools/call",
                "params": {
                    "name": "check_freerouting",
                    "arguments": { "jar_path": missing.display().to_string() }
                }
            }))
            .await
            .expect("tools/call receives a response");
        let result = response.result.expect("result");
        assert_ne!(result["isError"], json!(true), "{result}");
        let body: serde_json::Value =
            serde_json::from_str(result["content"][0]["text"].as_str().unwrap()).unwrap();
        assert_eq!(body["engine_found"], false, "{body}");
        assert_eq!(body["available"], false, "{body}");
        assert_eq!(body["checked_path"], json!(missing), "{body}");
        assert_eq!(
            body["note"],
            json!(format!("jar_path is not a file: {}", missing.display()))
        );
        assert!(body.get("searched_locations").is_none(), "{body}");
    }

    /// A JAR whose main class is built for Java 25 (class-file major 69), as
    /// Freerouting 2.5.0's is.
    #[cfg(unix)]
    fn java_25_jar(dir: &Path) -> PathBuf {
        use std::io::Write;
        let jar = dir.join("freerouting-2.5.0.jar");
        let mut zip = zip::ZipWriter::new(std::fs::File::create(&jar).unwrap());
        let options = zip::write::SimpleFileOptions::default();
        zip.start_file("META-INF/MANIFEST.MF", options).unwrap();
        zip.write_all(b"Manifest-Version: 1.0\r\nMain-Class: app.freerouting.Freerouting\r\n")
            .unwrap();
        zip.start_file("app/freerouting/Freerouting.class", options)
            .unwrap();
        zip.write_all(&[0xCA, 0xFE, 0xBA, 0xBE, 0, 0, 0, 69])
            .unwrap();
        zip.finish().unwrap();
        jar
    }

    /// Runs `future` to completion on a runtime of its own, so a test can hold
    /// the synchronous `JAVA_ENV_LOCK` without keeping it across an await.
    #[cfg(unix)]
    fn block_on<F: std::future::Future>(future: F) -> F::Output {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
            .block_on(future)
    }

    /// Served `check_freerouting` with `JAVA_HOME` pointing at a fake `java`
    /// whose script is `body`. The caller holds `JAVA_ENV_LOCK`.
    #[cfg(unix)]
    async fn served_check_with_fake_java(body: &str) -> serde_json::Value {
        use std::os::unix::fs::PermissionsExt;
        let temp = tempfile::tempdir().unwrap();
        let jar = java_25_jar(temp.path());
        let bin = temp.path().join("jdk").join("bin");
        std::fs::create_dir_all(&bin).unwrap();
        let java = bin.join("java");
        std::fs::write(&java, body).unwrap();
        std::fs::set_permissions(&java, std::fs::Permissions::from_mode(0o755)).unwrap();

        let previous = std::env::var_os("JAVA_HOME");
        std::env::set_var("JAVA_HOME", temp.path().join("jdk"));
        let handler = crate::mcp::handler::McpHandler::new(ServerConfig {
            kicad_cli: String::new(),
            kicad_binary: String::new(),
            ipc_address: String::new(),
            project_dir: None,
            jlcpcb_db_path: None,
            auto_load_toolsets: true,
            eager_toolsets: true,
        })
        .await
        .expect("handler builds");
        let response = handler
            .handle_message(json!({
                "jsonrpc": "2.0",
                "id": 1,
                "method": "tools/call",
                "params": {
                    "name": "check_freerouting",
                    "arguments": { "jar_path": jar.display().to_string() }
                }
            }))
            .await
            .expect("tools/call receives a response");
        match previous {
            Some(value) => std::env::set_var("JAVA_HOME", value),
            None => std::env::remove_var("JAVA_HOME"),
        }
        let result = response.result.expect("result");
        assert_ne!(result["isError"], json!(true), "{result}");
        let body: serde_json::Value =
            serde_json::from_str(result["content"][0]["text"].as_str().unwrap()).unwrap();
        assert_eq!(body["java_path"], json!(java), "{body}");
        body
    }

    /// A Java older than the JAR needs is refused up front, with both
    /// releases named, and `JAVA_HOME` is the `java` that was asked.
    #[cfg(unix)]
    #[test]
    fn served_check_freerouting_refuses_a_java_older_than_the_jar() {
        let _env = crate::java_runtime::JAVA_ENV_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let body = block_on(served_check_with_fake_java(
            "#!/bin/sh\n\
             if [ \"$1\" = -version ]; then echo 'openjdk version \"17.0.2\" 2022-01-18' >&2; exit 0; fi\n\
             exit 1\n",
        ));
        assert_eq!(body["available"], false, "{body}");
        assert_eq!(body["engine_found"], true, "{body}");
        assert_eq!(body["java_available"], true, "{body}");
        assert_eq!(body["java_release"], 17, "{body}");
        assert_eq!(body["required_java_release"], 25, "{body}");
        assert!(
            body["note"].as_str().unwrap().contains("needs Java 25"),
            "{body}"
        );
        // Refused before the JAR was run.
        assert!(body.get("version_checked").is_none(), "{body}");
    }

    /// A recent enough Java whose JAR does not run is not `available`, and
    /// the note says why.
    #[cfg(unix)]
    #[test]
    fn served_check_freerouting_is_unavailable_when_the_jar_does_not_run() {
        let _env = crate::java_runtime::JAVA_ENV_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let body = block_on(served_check_with_fake_java(
            "#!/bin/sh\n\
             if [ \"$1\" = -version ]; then echo 'openjdk version \"25.0.1\" 2025-10-21' >&2; exit 0; fi\n\
             echo 'Error: could not start' >&2; exit 1\n",
        ));
        assert_eq!(body["available"], false, "{body}");
        assert_eq!(body["java_available"], true, "{body}");
        assert_eq!(body["java_release"], 25, "{body}");
        assert_eq!(body["required_java_release"], 25, "{body}");
        assert_eq!(body["version_checked"], false, "{body}");
        assert!(
            body["note"].as_str().unwrap().contains("did not run"),
            "{body}"
        );
    }

    /// A recent enough Java that runs the JAR is `available`, and the
    /// response has no `note` key, as before the Java check was added.
    #[cfg(unix)]
    #[test]
    fn served_check_freerouting_is_available_without_a_note_when_the_jar_runs() {
        let _env = crate::java_runtime::JAVA_ENV_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let body = block_on(served_check_with_fake_java(
            "#!/bin/sh\n\
             if [ \"$1\" = -version ]; then echo 'openjdk version \"25.0.1\" 2025-10-21' >&2; exit 0; fi\n\
             echo 'Freerouting 2.5.0'; exit 0\n",
        ));
        assert_eq!(body["available"], true, "{body}");
        assert_eq!(body["version_checked"], true, "{body}");
        assert!(body.get("note").is_none(), "{body}");
    }

    /// A directory reached twice is searched, and reported, once, under the
    /// location that reached it first.
    #[test]
    fn a_root_reached_twice_is_searched_once() {
        let temp = tempfile::tempdir().unwrap();
        let shared = temp.path().join("shared").into_os_string();
        let path = std::env::join_paths([
            temp.path().join("bin"),
            temp.path().join("bin"),
            PathBuf::from(&shared),
        ])
        .unwrap();
        let env = HashMap::from([
            ("KICAD10_3RD_PARTY", shared.clone()),
            ("KICAD9_3RD_PARTY", shared.clone()),
            ("PATH", path),
        ]);
        let roots = freerouting_search_roots_from(|name| env.get(name).cloned(), temp.path());

        let mut paths: Vec<&PathBuf> = roots.iter().map(|root| &root.path).collect();
        let total = paths.len();
        paths.sort();
        paths.dedup();
        assert_eq!(paths.len(), total, "{roots:?}");
        assert_eq!(
            roots
                .iter()
                .find(|root| root.path.as_os_str() == shared)
                .map(|root| root.location),
            Some(FreeroutingLocation::KicadThirdPartyVariable)
        );
        assert_eq!(
            roots
                .iter()
                .filter(|root| root.location == FreeroutingLocation::PathDirectory)
                .count(),
            1
        );
    }

    /// Both tools that take `jar_path` describe it the same way, through the
    /// served `tools/list`.
    #[tokio::test]
    async fn served_jar_path_descriptions_state_the_search() {
        let handler = crate::mcp::handler::McpHandler::new(ServerConfig {
            kicad_cli: String::new(),
            kicad_binary: String::new(),
            ipc_address: String::new(),
            project_dir: None,
            jlcpcb_db_path: None,
            auto_load_toolsets: false,
            eager_toolsets: true,
        })
        .await
        .expect("handler builds");
        let response = handler
            .handle_message(json!({"jsonrpc": "2.0", "id": 1, "method": "tools/list"}))
            .await
            .expect("tools/list receives a response");
        let tools = response.result.expect("result")["tools"].clone();
        for name in ["check_freerouting", "route_specctra_dsn"] {
            let tool = tools
                .as_array()
                .unwrap()
                .iter()
                .find(|tool| tool["name"] == name)
                .unwrap_or_else(|| panic!("{name} is not listed"));
            assert_eq!(
                tool["inputSchema"]["properties"]["jar_path"]["description"],
                json!(freerouting_jar_path_description()),
                "{name}"
            );
        }
    }

    /// `route_specctra_dsn` names the missing file instead of telling a caller
    /// who passed `jar_path` to pass `jar_path`.
    #[tokio::test]
    async fn served_route_refuses_a_jar_path_that_is_not_a_file() {
        let temp = tempfile::tempdir().unwrap();
        let missing = temp.path().join("missing.jar");
        let handler = crate::mcp::handler::McpHandler::new(ServerConfig {
            kicad_cli: String::new(),
            kicad_binary: String::new(),
            ipc_address: String::new(),
            project_dir: None,
            jlcpcb_db_path: None,
            auto_load_toolsets: true,
            eager_toolsets: true,
        })
        .await
        .expect("handler builds");
        let response = handler
            .handle_message(json!({
                "jsonrpc": "2.0",
                "id": 1,
                "method": "tools/call",
                "params": {
                    "name": "route_specctra_dsn",
                    "arguments": {
                        "dsn_path": temp.path().join("board.dsn").display().to_string(),
                        "ses_output_path": temp.path().join("board.ses").display().to_string(),
                        "jar_path": missing.display().to_string()
                    }
                }
            }))
            .await
            .expect("tools/call receives a response");
        let result = response.result.expect("result");
        assert_eq!(result["isError"], json!(true), "{result}");
        let body: serde_json::Value =
            serde_json::from_str(result["content"][0]["text"].as_str().unwrap()).unwrap();
        assert_eq!(body["error"]["kind"], "file_not_found", "{body}");
        assert_eq!(body["error"]["path"], json!(missing.display().to_string()));
        assert_eq!(
            body["message"],
            json!(format!("jar_path is not a file: {}", missing.display()))
        );
    }

    /// A threshold Freerouting would multiply by 100 is refused as
    /// `invalid_argument` before the JAR is looked up, so no JAR is needed here.
    #[tokio::test]
    async fn served_route_refuses_a_threshold_freerouting_reads_as_a_fraction() {
        for (threshold_args, field) in [
            (
                json!({ "optimizer_improvement_threshold": 0.05 }),
                "optimizer_improvement_threshold",
            ),
            (
                json!({ "router_settings": { "optimizer": { "improvement_threshold": 0.05 } } }),
                mcp::ROUTER_SETTINGS_THRESHOLD,
            ),
        ] {
            let (temp, body) = served_route(threshold_args).await;
            assert_eq!(body["error"]["kind"], "invalid_argument", "{body}");
            assert_eq!(body["error"]["field"], field, "{body}");
            assert!(
                body["message"].as_str().unwrap().contains("as a fraction"),
                "{body}"
            );
            assert!(!temp.path().join("board.ses").exists());
        }
    }

    #[tokio::test]
    async fn served_route_checks_the_threshold_it_sends() {
        // The named argument wins over router_settings, so only its value is
        // sent and checked: the call goes on to look up the (missing) JAR.
        let (_temp, body) = served_route(json!({
            "optimizer_improvement_threshold": 1.0,
            "router_settings": { "optimizer": { "improvement_threshold": 0.05 } }
        }))
        .await;
        assert_eq!(body["error"]["kind"], "file_not_found", "{body}");
    }

    #[tokio::test]
    async fn served_route_refuses_negative_router_settings_before_the_jar_lookup() {
        // The JAR path does not exist: an invalid_argument rather than
        // file_not_found shows the refusal comes before the JAR is looked up.
        let (temp, body) = served_route(json!({
            "router_settings": {
                "scoring": { "via_costs": -5, "plane_via_costs": 5 },
                "fanout": { "min_escape_length_mm": -0.5 }
            }
        }))
        .await;
        assert_eq!(body["error"]["kind"], "invalid_argument", "{body}");
        assert_eq!(
            body["error"]["field"], "router_settings.fanout.min_escape_length_mm",
            "{body}"
        );
        let message = body["message"].as_str().unwrap();
        assert!(
            message.contains("router_settings.fanout.min_escape_length_mm is -0.5"),
            "{body}"
        );
        assert!(
            message.contains("router_settings.scoring.via_costs is -5"),
            "{body}"
        );
        assert!(!message.contains("plane_via_costs"), "{body}");
        assert!(!temp.path().join("board.ses").exists());
    }

    #[tokio::test]
    async fn served_route_checks_the_negative_values_it_sends() {
        // max_passes replaces autorouter.max_passes, so -1 is never sent and
        // the call goes on to look up the (missing) JAR.
        let (_temp, body) = served_route(json!({
            "max_passes": 3,
            "router_settings": { "autorouter": { "max_passes": -1 } }
        }))
        .await;
        assert_eq!(body["error"]["kind"], "file_not_found", "{body}");
    }

    /// Calls route_specctra_dsn through the MCP handler, with a JAR path that
    /// does not exist, and returns the structured error body.
    async fn served_route(extra: serde_json::Value) -> (tempfile::TempDir, serde_json::Value) {
        let temp = tempfile::tempdir().unwrap();
        let handler = crate::mcp::handler::McpHandler::new(ServerConfig {
            kicad_cli: String::new(),
            kicad_binary: String::new(),
            ipc_address: String::new(),
            project_dir: None,
            jlcpcb_db_path: None,
            auto_load_toolsets: true,
            eager_toolsets: true,
        })
        .await
        .expect("handler builds");
        let mut arguments = json!({
            "dsn_path": temp.path().join("board.dsn").display().to_string(),
            "ses_output_path": temp.path().join("board.ses").display().to_string(),
            "jar_path": temp.path().join("missing.jar").display().to_string(),
        });
        arguments
            .as_object_mut()
            .unwrap()
            .extend(extra.as_object().unwrap().clone());
        let response = handler
            .handle_message(json!({
                "jsonrpc": "2.0",
                "id": 1,
                "method": "tools/call",
                "params": { "name": "route_specctra_dsn", "arguments": arguments }
            }))
            .await
            .expect("tools/call receives a response");
        let result = response.result.expect("result");
        assert_eq!(result["isError"], json!(true), "{result}");
        let body = serde_json::from_str(result["content"][0]["text"].as_str().unwrap()).unwrap();
        (temp, body)
    }

    #[test]
    fn ignores_unrelated_jars() {
        let temp = tempfile::tempdir().unwrap();
        std::fs::write(temp.path().join("other.jar"), b"fixture").unwrap();
        assert_eq!(find_freerouting_jar_below(temp.path(), 5), None);
    }

    #[tokio::test]
    #[ignore = "requires Java and FREEROUTING_JAR"]
    async fn installed_engine_reports_each_observed_readiness_boundary() {
        let jar = PathBuf::from(std::env::var_os("FREEROUTING_JAR").expect("set FREEROUTING_JAR"));
        let result = check_freerouting(&json!({ "jar_path": jar }), &test_ctx())
            .await
            .unwrap();
        let body = response_json(&result);
        assert_eq!(body["available"], true);
        assert_eq!(body["engine_found"], true);
        assert_eq!(body["native_mcp_available"], true);
        assert_eq!(body["bridge_available"], true);
        assert!(body["native_mcp_tool_count"].as_u64().unwrap() >= 6);
        assert!(body["server_protocol_version"].as_str().is_some());
    }
}
