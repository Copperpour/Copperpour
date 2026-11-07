//! Local Freerouting MCP orchestration.
//!
//! Copperpour deliberately does not reimplement the router. It starts the local
//! Freerouting JAR in its documented headless stdio MCP mode, follows the
//! server's routing state machine, and returns only compact job evidence.

use anyhow::{bail, Context, Result};
use serde::Serialize;
use serde_json::{json, Map, Number, Value};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader, Lines};
use tokio::process::{Child, ChildStdin, ChildStdout, Command};
use tokio::task::JoinHandle;
use tokio::time::{timeout, Instant};

#[cfg(test)]
use std::sync::atomic::{AtomicU32, Ordering};

#[cfg(test)]
static LAST_CHILD_PID: AtomicU32 = AtomicU32::new(0);

const MCP_PROTOCOL_VERSION: &str = "2024-11-05";
const STARTUP_TIMEOUT: Duration = Duration::from_secs(30);
const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);
const SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(3);
const MAX_STDERR_BYTES: usize = 64 * 1024;
const MAX_SES_BYTES: u64 = 512 * 1024 * 1024;

/// JVM flag keeping Freerouting off the macOS window server (no Dock icon).
/// `--gui.enabled=false` alone does not prevent AWT from registering.
pub(crate) const JAVA_HEADLESS_ARG: &str = "-Djava.awt.headless=true";

fn server_arguments() -> &'static [&'static str] {
    &[
        "--api_server.enabled=true",
        "--api_server.authentication.enabled=false",
        "--api_server-endpoints=http://127.0.0.1:37864",
        "--mcp_server.enabled=true",
        "--mcp_server.authentication.enabled=false",
        "--mcp_server-endpoints=http://127.0.0.1:37964",
        "--mcp_server.stdio=true",
        "--gui.enabled=false",
    ]
}

const REQUIRED_TOOLS: &[&str] = &[
    "create_session",
    "enqueue_job",
    "upload_job_input_from_local_file",
    "start_job",
    "get_job_details",
    "download_job_output_to_local_file",
];

#[derive(Debug, Clone)]
pub(crate) struct RouteSettings {
    pub max_passes: Option<u32>,
    pub optimizer_enabled: Option<bool>,
    pub job_timeout_seconds: Option<u64>,
    /// Once routing is complete and the optimizer has started, how long to let
    /// it run before taking the best board so far and stopping the job.
    pub optimize_seconds: Option<u64>,
    /// Freerouting's `optimizer.max_passes` (its default is 100).
    pub optimizer_max_passes: Option<u32>,
    /// Freerouting's `optimizer.improvement_threshold`, in percent: the
    /// optimizer stops after a pass that improves less (its default is 2.5).
    pub optimizer_improvement_threshold: Option<f64>,
    /// Freerouting's `plane_nets`: nets to mark as power-plane nets (plane
    /// routing mode, cheaper vias to the plane). A `(plane ...)` declared in
    /// the DSN already sets the same flag on its net.
    pub plane_nets: Option<Vec<String>>,
    /// Freerouting's `plane_as_obstacle`: whether copper pours block foreign
    /// traces (its default is false).
    pub plane_as_obstacle: Option<bool>,
    /// Any of Freerouting's router settings, in its JSON shape
    /// (`get_effective_settings`), passed through as given. The named fields
    /// above are merged on top and win.
    pub router_settings: Option<Map<String, Value>>,
    /// Settings taken from the KiCad project's design rules
    /// ([`project_route_rules`]), sent under everything above. One the JAR
    /// does not apply is reported, not refused: the caller did not ask for it.
    pub project_rules: Map<String, Value>,
    pub poll_interval: Duration,
    pub overall_timeout: Duration,
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct RouteEvidence {
    pub session_id: String,
    pub job_id: String,
    pub final_state: String,
    pub poll_count: u32,
    pub elapsed_seconds: f64,
    pub ses_bytes: u64,
    pub server_protocol_version: String,
    pub diagnostics_path: Option<String>,
    /// Last routing activity read from Freerouting's log, e.g.
    /// "pass 17: 0 unrouted, 218 violations".
    pub last_activity: Option<String>,
    /// Whether the sent settings were read back from
    /// `get_effective_settings`. When false, nothing was compared, so a
    /// setting that did not take went unnoticed.
    pub settings_checked: bool,
    /// Connections still unrouted when the job ended, from the last count in
    /// Freerouting's log; None when the log could not be read.
    pub unrouted: Option<u64>,
    /// Freerouting's log file the count was looked for in; None when no
    /// location could be worked out.
    pub freerouting_log: Option<String>,
    /// Project design rules sent but not applied, e.g. a key an older JAR
    /// does not have (`hole_to_hole_um` in 2.5.0), as "path: reason".
    pub project_rules_not_applied: Vec<String>,
    /// Freerouting's own `validation_warnings` for the effective settings.
    pub settings_warnings: Vec<Value>,
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct BridgeProbe {
    pub native_mcp_available: bool,
    pub bridge_available: bool,
    pub server_protocol_version: Option<String>,
    pub tool_count: usize,
    pub diagnostics: Option<String>,
    pub error: Option<String>,
}

pub(crate) async fn probe_local(jar: &Path) -> BridgeProbe {
    let mut client = match LocalMcpClient::start(jar).await {
        Ok(client) => client,
        Err(error) => {
            return BridgeProbe {
                native_mcp_available: false,
                bridge_available: false,
                server_protocol_version: None,
                tool_count: 0,
                diagnostics: None,
                error: Some(format!("{error:#}")),
            };
        }
    };
    let server_protocol_version = Some(client.server_protocol_version.clone());
    let contract = async {
        let tools = client.list_tools().await?;
        validate_tool_contracts(&tools)?;
        let missing = REQUIRED_TOOLS
            .iter()
            .filter(|name| !tools.contains_key(**name))
            .copied()
            .collect::<Vec<_>>();
        if !missing.is_empty() {
            bail!(
                "Freerouting MCP is missing required tool(s): {}",
                missing.join(", ")
            );
        }
        Ok::<usize, anyhow::Error>(tools.len())
    }
    .await;
    let diagnostics = client.close().await;
    match contract {
        Ok(tool_count) => BridgeProbe {
            native_mcp_available: true,
            bridge_available: true,
            server_protocol_version,
            tool_count,
            diagnostics: (!diagnostics.is_empty()).then_some(diagnostics),
            error: None,
        },
        Err(error) => BridgeProbe {
            native_mcp_available: true,
            bridge_available: false,
            server_protocol_version,
            tool_count: 0,
            diagnostics: (!diagnostics.is_empty()).then_some(diagnostics),
            error: Some(format!("{error:#}")),
        },
    }
}

pub(crate) async fn route_local(
    jar: &Path,
    dsn: &Path,
    ses_output: &Path,
    settings: &RouteSettings,
) -> Result<RouteEvidence> {
    validate_inputs(jar, dsn, ses_output, settings)?;
    if let Some(parent) = ses_output
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
    {
        tokio::fs::create_dir_all(parent)
            .await
            .with_context(|| format!("create SES output directory {}", parent.display()))?;
    }
    let temporary = temporary_output_path(ses_output)?;
    if temporary.exists() {
        bail!(
            "temporary SES output already exists: {}",
            temporary.display()
        );
    }

    let mut client = LocalMcpClient::start(jar).await?;
    let started = Instant::now();
    let route_result = match timeout(
        settings.overall_timeout,
        run_state_machine(&mut client, dsn, &temporary, settings, started),
    )
    .await
    {
        Ok(result) => result,
        Err(_) => Err(anyhow::anyhow!(
            "Freerouting MCP job exceeded the overall timeout of {} seconds",
            settings.overall_timeout.as_secs()
        )),
    };
    let diagnostics = client.close().await;
    let diagnostics_path = write_diagnostics(ses_output, &diagnostics).await?;

    let result = route_result
        .map_err(|error| with_stderr_tail(error, &diagnostics, diagnostics_path.as_deref()));
    let evidence = match result {
        Ok(evidence) => evidence,
        Err(error) => {
            let _ = tokio::fs::remove_file(&temporary).await;
            return Err(error);
        }
    };

    let metadata = tokio::fs::metadata(&temporary)
        .await
        .with_context(|| format!("Freerouting did not create {}", temporary.display()))?;
    if metadata.len() == 0 || metadata.len() > MAX_SES_BYTES {
        let _ = tokio::fs::remove_file(&temporary).await;
        bail!(
            "Freerouting SES size {} is outside the supported range 1..={MAX_SES_BYTES}",
            metadata.len()
        );
    }
    let ses_source = tokio::fs::read_to_string(&temporary)
        .await
        .with_context(|| format!("read Freerouting SES {}", temporary.display()))?;
    let tree = copperpour_sexp::parse_sexp(&ses_source).context("parse Freerouting SES output")?;
    if tree.head() != Some("session") {
        let _ = tokio::fs::remove_file(&temporary).await;
        bail!("Freerouting output is not a Specctra SES session");
    }
    copperpour_sexp::write_new_atomic(ses_output, &ses_source)
        .with_context(|| format!("create SES output {}", ses_output.display()))?;
    tokio::fs::remove_file(&temporary)
        .await
        .with_context(|| format!("remove temporary SES {}", temporary.display()))?;
    Ok(RouteEvidence {
        ses_bytes: metadata.len(),
        diagnostics_path,
        ..evidence
    })
}

/// `error` first, then the end of Freerouting's stderr: its banner and
/// warnings run to thousands of characters, with the cause at the end. `full`
/// names the file that holds all of it.
fn with_stderr_tail(error: anyhow::Error, diagnostics: &str, full: Option<&str>) -> anyhow::Error {
    if diagnostics.is_empty() {
        return error;
    }
    let full = full
        .map(|path| format!(", all of it in {path}"))
        .unwrap_or_default();
    anyhow::anyhow!(
        "{error:#}\nFreerouting stderr, last lines{full}:\n{}",
        stderr_tail(diagnostics)
    )
}

/// The last 12 lines of `diagnostics`, cut to their last 2000 characters.
fn stderr_tail(diagnostics: &str) -> String {
    const LINES: usize = 12;
    const CHARS: usize = 2000;
    let lines: Vec<&str> = diagnostics.lines().collect();
    let tail = lines[lines.len().saturating_sub(LINES)..].join("\n");
    match tail.char_indices().rev().nth(CHARS - 1) {
        Some((start, _)) => format!("...{}", &tail[start..]),
        None => tail,
    }
}

fn diagnostics_path(ses_output: &Path) -> PathBuf {
    ses_output.with_extension("freerouting.log")
}

async fn write_diagnostics(output: &Path, diagnostics: &str) -> Result<Option<String>> {
    if diagnostics.is_empty() {
        return Ok(None);
    }
    let path = diagnostics_path(output);
    if path.exists() {
        bail!(
            "Freerouting diagnostics output already exists: {}",
            path.display()
        );
    }
    copperpour_sexp::write_new_atomic(&path, diagnostics)
        .with_context(|| format!("create Freerouting diagnostics {}", path.display()))?;
    Ok(Some(path.display().to_string()))
}

fn validate_inputs(
    jar: &Path,
    dsn: &Path,
    ses_output: &Path,
    settings: &RouteSettings,
) -> Result<()> {
    if !jar.is_file() {
        bail!("Freerouting JAR does not exist: {}", jar.display());
    }
    if !dsn.is_file() || !extension_is(dsn, "dsn") {
        bail!("DSN input must be an existing .dsn file: {}", dsn.display());
    }
    if !extension_is(ses_output, "ses") {
        bail!("SES output must have the .ses extension");
    }
    if ses_output.exists() {
        bail!("SES output already exists: {}", ses_output.display());
    }
    // write_diagnostics refuses to replace this file, but only once routing is
    // over: refuse now rather than after a run that can take many minutes.
    let diagnostics = diagnostics_path(ses_output);
    if diagnostics.exists() {
        bail!(
            "Freerouting diagnostics output already exists: {}",
            diagnostics.display()
        );
    }
    if let Some(max_passes) = settings.max_passes {
        if !(1..=100).contains(&max_passes) {
            bail!("max_passes must be between 1 and 100");
        }
    }
    if settings.poll_interval < Duration::from_secs(2)
        || settings.poll_interval > Duration::from_secs(5)
    {
        bail!("poll interval must be between 2 and 5 seconds");
    }
    let minimum_overall_timeout = if cfg!(test) {
        Duration::from_millis(1)
    } else {
        Duration::from_secs(10)
    };
    if settings.overall_timeout < minimum_overall_timeout
        || settings.overall_timeout > Duration::from_secs(86_400)
    {
        bail!("overall timeout must be between 10 and 86400 seconds");
    }
    if settings
        .job_timeout_seconds
        .is_some_and(|seconds| seconds == 0 || seconds > 86_400)
    {
        bail!("job_timeout_seconds must be between 1 and 86400");
    }
    if settings
        .optimize_seconds
        .is_some_and(|seconds| seconds > 86_400)
    {
        bail!("optimize_seconds must be between 0 and 86400");
    }
    if settings
        .optimizer_max_passes
        .is_some_and(|passes| !(1..=100).contains(&passes))
    {
        bail!("optimizer_max_passes must be between 1 and 100");
    }
    if settings
        .optimizer_improvement_threshold
        .is_some_and(|percent| !(0.0..=100.0).contains(&percent))
    {
        bail!("optimizer_improvement_threshold must be between 0 and 100 (percent)");
    }
    if settings
        .optimizer_improvement_threshold
        .is_some_and(threshold_read_as_fraction)
    {
        bail!("optimizer_improvement_threshold {THRESHOLD_READ_AS_FRACTION}");
    }
    if settings.optimizer_improvement_threshold.is_none()
        && settings
            .router_settings
            .as_ref()
            .and_then(router_settings_threshold)
            .is_some_and(threshold_read_as_fraction)
    {
        bail!("{ROUTER_SETTINGS_THRESHOLD} {THRESHOLD_READ_AS_FRACTION}");
    }
    let negative = negative_settings_sent(settings);
    if !negative.is_empty() {
        bail!("{}", negative_settings_message(&negative));
    }
    Ok(())
}

/// Why a negative `router_settings` number is refused.
pub(crate) const NEGATIVE_SETTING: &str =
    "must not be negative: Freerouting takes a negative cost, count or distance without a \
     warning and routes with it, and none of its settings has a meaning below zero";

/// Every number below zero that `router_settings` would send, as its path
/// (`router_settings.scoring.via_costs`) and value. A value that a named
/// argument replaces is not sent, so it is not reported.
pub(crate) fn negative_settings_sent(settings: &RouteSettings) -> Vec<(String, Number)> {
    // Mirrors merge_into: an object over an object merges key by key, and
    // anything else in the named settings replaces the whole router_settings
    // value at that path.
    fn walk(value: &Value, named: Option<&Value>, path: &str, out: &mut Vec<(String, Number)>) {
        if let Some(named) = named {
            if !(named.is_object() && value.is_object()) {
                return;
            }
        }
        match value {
            Value::Object(map) => {
                for (key, value) in map {
                    let named = named.and_then(|named| named.get(key));
                    walk(value, named, &format!("{path}.{key}"), out);
                }
            }
            Value::Array(items) => {
                for (index, value) in items.iter().enumerate() {
                    walk(value, None, &format!("{path}[{index}]"), out);
                }
            }
            Value::Number(number) if number.as_f64().is_some_and(|n| n < 0.0) => {
                out.push((path.to_string(), number.clone()))
            }
            _ => {}
        }
    }
    let mut out = Vec::new();
    if let Some(router_settings) = &settings.router_settings {
        let named = Value::Object(named_settings(settings));
        walk(
            &Value::Object(router_settings.clone()),
            Some(&named),
            "router_settings",
            &mut out,
        );
    }
    out
}

/// The refusal for what [`negative_settings_sent`] found, naming each path.
pub(crate) fn negative_settings_message(negative: &[(String, Number)]) -> String {
    let listed: Vec<String> = negative
        .iter()
        .map(|(path, value)| format!("{path} is {value}"))
        .collect();
    format!(
        "{}. A router setting {NEGATIVE_SETTING}. Nothing was routed.",
        listed.join("; ")
    )
}

/// Where `router_settings` carries the optimizer's improvement threshold.
pub(crate) const ROUTER_SETTINGS_THRESHOLD: &str =
    "router_settings.optimizer.improvement_threshold";

/// The improvement threshold `router_settings` would send, if it sets one.
/// It is sent only when `optimizer_improvement_threshold` is not given.
pub(crate) fn router_settings_threshold(router_settings: &Map<String, Value>) -> Option<f64> {
    router_settings
        .get("optimizer")?
        .get("improvement_threshold")?
        .as_f64()
}

/// Freerouting 2.5's `BatchOptimizer` reads an improvement threshold above 0
/// and below 0.1 as a fraction and multiplies it by 100, so such a value never
/// runs as given. 0 runs as given.
pub(crate) fn threshold_read_as_fraction(percent: f64) -> bool {
    percent > 0.0 && percent < 0.1
}

/// Why a threshold for which [`threshold_read_as_fraction`] holds is refused.
pub(crate) const THRESHOLD_READ_AS_FRACTION: &str =
    "above 0 and below 0.1 is refused: Freerouting reads such a value as a fraction and \
     multiplies it by 100, so 0.05 would run as 5%. Pass 0, or a percentage of at least 0.1";

async fn run_state_machine(
    client: &mut LocalMcpClient,
    dsn: &Path,
    temporary: &Path,
    settings: &RouteSettings,
    started: Instant,
) -> Result<RouteEvidence> {
    let tools = client.list_tools().await?;
    validate_tool_contracts(&tools)?;
    let missing = REQUIRED_TOOLS
        .iter()
        .filter(|name| !tools.contains_key(**name))
        .copied()
        .collect::<Vec<_>>();
    if !missing.is_empty() {
        bail!(
            "Freerouting MCP is missing required tool(s): {}",
            missing.join(", ")
        );
    }

    let session = client.call_tool("create_session", json!({})).await?;
    let session_id = required_string(&session, &["sessionId", "session_id", "id"], "session id")?;
    let job = client
        .call_tool(
            "enqueue_job",
            json!({ "body": { "session_id": session_id } }),
        )
        .await?;
    let job_id = required_string(&job, &["jobId", "job_id", "id"], "job id")?;
    client
        .call_tool(
            "upload_job_input_from_local_file",
            json!({ "jobId": job_id, "filePath": absolute_utf8(dsn)? }),
        )
        .await?;

    let job_settings = job_settings_body(settings);
    let mut settings_checked = false;
    let requested = requested_settings(settings);
    let project_only = project_only_settings(settings, &requested);
    let mut settings_warnings = Vec::new();
    let mut project_rules_not_applied = Vec::new();
    if !job_settings.is_empty() && !tools.contains_key("update_job_settings") {
        if !requested.is_empty() {
            bail!("Freerouting MCP does not expose update_job_settings");
        }
        // Only project rules to send: a JAR that cannot take them still
        // routes, as it did before they were sent, and says what it skipped.
        project_rules_not_applied = project_only
            .keys()
            .map(|key| format!("{key}: not sent, this Freerouting has no update_job_settings"))
            .collect();
    } else if !job_settings.is_empty() {
        client
            .call_tool(
                "update_job_settings",
                json!({ "path": { "jobId": job_id }, "body": job_settings }),
            )
            .await?;
        // Freerouting accepts unknown keys and drops them without a word:
        // read the settings back, and refuse before start_job a run that
        // would ignore part of what was asked.
        if tools.contains_key("get_effective_settings") {
            let effective = client
                .call_tool(
                    "get_effective_settings",
                    json!({ "path": { "jobId": job_id } }),
                )
                .await?;
            if let Some(effective) = effective_settings(&effective) {
                settings_checked = true;
                // What the caller asked for is checked strictly; a project
                // rule it did not override is only reported.
                project_rules_not_applied = settings_mismatches(&project_only, effective);
                let not_applied = settings_mismatches(&requested, effective);
                if !not_applied.is_empty() {
                    bail!(
                        "Freerouting did not apply these settings, nothing was routed: {}",
                        not_applied.join("; ")
                    );
                }
                settings_warnings = effective
                    .get("validation_warnings")
                    .and_then(Value::as_array)
                    .cloned()
                    .unwrap_or_default();
            }
        }
    }
    client
        .call_tool("start_job", json!({ "path": { "jobId": job_id } }))
        .await?;

    let mut poll_count = 0u32;
    let mut progress = JobProgress::new(&job_id);
    let mut optimizer_bound = OptimizerBound::new(settings.optimize_seconds);
    let mut output_downloaded = false;
    let final_state = loop {
        let details = client
            .call_tool("get_job_details", json!({ "path": { "jobId": job_id } }))
            .await?;
        poll_count = poll_count.saturating_add(1);
        let state = required_string(&details, &["state", "status"], "job state")?;
        progress.report(&state, &details, started);
        match JobState::of(&state) {
            JobState::Completed => break "COMPLETED".to_string(),
            JobState::Ended => bail!("Freerouting job {job_id} ended in state {state}"),
            JobState::EndedWithoutOutput => bail!(
                "Freerouting job {job_id} ended in state {state}, which has no downloadable \
                 output; to bound the optimizer, use optimize_seconds rather than \
                 job_timeout_seconds"
            ),
            JobState::Running => {}
        }

        let optimizing = progress.is_optimizing() || stage_is_optimizing(&details);
        // Until Freerouting has published an output, keep polling and retry.
        if optimizer_bound.reached(optimizing, Instant::now())
            && download_running_output(client, &job_id, temporary).await?
        {
            // The snapshot is in hand, so stopping the job is a courtesy:
            // close() reaps the child whatever cancel_job answers.
            let _ = client
                .call_tool("cancel_job", json!({ "path": { "jobId": job_id } }))
                .await;
            output_downloaded = true;
            break "OPTIMIZATION_STOPPED".to_string();
        }
        tokio::time::sleep(settings.poll_interval).await;
    };

    if !output_downloaded {
        client
            .call_tool(
                "download_job_output_to_local_file",
                json!({ "jobId": job_id, "filePath": absolute_utf8(temporary)? }),
            )
            .await?;
    }
    Ok(RouteEvidence {
        session_id,
        job_id,
        final_state,
        poll_count,
        elapsed_seconds: started.elapsed().as_secs_f64(),
        ses_bytes: 0,
        server_protocol_version: client.server_protocol_version.clone(),
        diagnostics_path: None,
        unrouted: progress.read_unrouted(),
        freerouting_log: progress.log.as_deref().map(|log| log.display().to_string()),
        last_activity: progress.activity,
        settings_checked,
        project_rules_not_applied,
        settings_warnings,
    })
}

/// What a `get_job_details` state means for the poll loop.
#[derive(Debug, PartialEq, Eq)]
enum JobState {
    Completed,
    /// Failed or cancelled.
    Ended,
    /// Terminal too, and Freerouting refuses the output of these states:
    /// polling on would only wait for the overall timeout.
    EndedWithoutOutput,
    /// Anything else: a job still queued or running, or a state this code does not
    /// know yet. The overall timeout still bounds it.
    Running,
}

impl JobState {
    fn of(state: &str) -> Self {
        match state.to_ascii_uppercase().as_str() {
            "COMPLETED" => Self::Completed,
            "FAILED" | "CANCELLED" | "CANCELED" => Self::Ended,
            "TIMED_OUT" | "TERMINATED" | "INVALID" => Self::EndedWithoutOutput,
            _ => Self::Running,
        }
    }
}

/// `optimize_seconds`, counted from the first poll that sees the optimizer.
struct OptimizerBound {
    limit: Option<Duration>,
    since: Option<Instant>,
}

impl OptimizerBound {
    fn new(optimize_seconds: Option<u64>) -> Self {
        Self {
            limit: optimize_seconds.map(Duration::from_secs),
            since: None,
        }
    }

    /// Whether the optimizer has now run for the whole limit. `optimizing` is
    /// what this poll saw; once seen, the clock keeps running even if a later
    /// poll cannot read the log. Without a limit, never.
    fn reached(&mut self, optimizing: bool, now: Instant) -> bool {
        let Some(limit) = self.limit else {
            return false;
        };
        if self.since.is_none() && optimizing {
            self.since = Some(now);
        }
        self.since
            .is_some_and(|since| now.duration_since(since) >= limit)
    }
}

/// Whether `get_job_details` places the job in its optimization stage.
fn stage_is_optimizing(details: &Value) -> bool {
    details
        .get("stage")
        .and_then(Value::as_str)
        .is_some_and(|stage| stage.to_ascii_uppercase().contains("OPTIMI"))
}

/// Save the output of a job that is still running.
///
/// Freerouting publishes the incumbent board, the best one so far, at the
/// start and end of every optimizer pass; candidates are worked on copies.
/// Taken during optimization, the snapshot is therefore the best board so
/// far. It is complete only if the router finished; `last_activity` carries
/// its unrouted count. Returns false while no output has been published yet.
async fn download_running_output(
    client: &mut LocalMcpClient,
    job_id: &str,
    temporary: &Path,
) -> Result<bool> {
    let _ = tokio::fs::remove_file(temporary).await;
    client
        .call_tool(
            "download_job_output_to_local_file",
            json!({ "jobId": job_id, "filePath": absolute_utf8(temporary)? }),
        )
        .await?;
    Ok(tokio::fs::metadata(temporary)
        .await
        .is_ok_and(|metadata| metadata.len() > 0))
}

/// How often an unchanged job still sends a progress notification, so the
/// client's idle timeout keeps being reset during long optimizer stretches.
const PROGRESS_HEARTBEAT: Duration = Duration::from_secs(60);
/// Only the end of Freerouting's log is read on each poll; the file grows to
/// tens of megabytes over a day of routing.
const LOG_TAIL_BYTES: u64 = 256 * 1024;

/// Turns job polls into `notifications/progress` for the current tool call.
///
/// `get_job_details` only says which pass is running. How far the routing
/// actually got (unrouted connections, violations, optimizer stage) is in
/// Freerouting's own log, where each line of a job carries the first six hex
/// digits of its id, upper-cased: `[A3AA32] Auto-routing pass #8 ... (4
/// unrouted and 218 violations)`.
struct JobProgress {
    tag: String,
    log: Option<PathBuf>,
    activity: Option<String>,
    last_message: String,
    last_sent: Option<Instant>,
    sent: u32,
}

impl JobProgress {
    fn new(job_id: &str) -> Self {
        Self {
            tag: format!(
                "[{}]",
                job_id.get(..6).unwrap_or(job_id).to_ascii_uppercase()
            ),
            log: freerouting_log_path(),
            activity: None,
            last_message: String::new(),
            last_sent: None,
            sent: 0,
        }
    }

    fn report(&mut self, state: &str, details: &Value, started: Instant) {
        if let Some(activity) = self.read_activity() {
            self.activity = Some(activity);
        }
        if let Some(message) = self.next_message(state, details, started, Instant::now()) {
            crate::progress::report(f64::from(self.sent), None, &message);
        }
    }

    /// The notification to send for this poll, or `None` when the job looks
    /// the same as last time and the heartbeat is not due. `now` is a
    /// parameter so that deduplication and the heartbeat can be tested.
    fn next_message(
        &mut self,
        state: &str,
        details: &Value,
        started: Instant,
        now: Instant,
    ) -> Option<String> {
        let mut message = state.to_ascii_lowercase();
        match &self.activity {
            Some(activity) => message = format!("{message}, {activity}"),
            None => {
                if let Some(pass) = details.get("current_pass").and_then(Value::as_u64) {
                    message = format!("{message}, pass {pass}");
                }
            }
        }
        let due = self
            .last_sent
            .is_none_or(|sent| now.duration_since(sent) >= PROGRESS_HEARTBEAT);
        if message == self.last_message && !due {
            return None;
        }
        self.sent += 1;
        self.last_sent = Some(now);
        let elapsed = now.duration_since(started).as_secs();
        let text = format!(
            "Freerouting {message} ({}m{:02}s)",
            elapsed / 60,
            elapsed % 60
        );
        self.last_message = message;
        Some(text)
    }

    /// Whether the job's log has reached the optimizer stage.
    fn is_optimizing(&self) -> bool {
        self.activity
            .as_deref()
            .is_some_and(|activity| activity.starts_with("optimiz"))
    }

    fn read_activity(&self) -> Option<String> {
        let tail = read_tail(self.log.as_deref()?, LOG_TAIL_BYTES)?;
        latest_activity(&tail, &self.tag)
    }

    fn read_unrouted(&self) -> Option<u64> {
        let tail = read_tail(self.log.as_deref()?, LOG_TAIL_BYTES)?;
        latest_unrouted(&tail, &self.tag)
    }
}

/// Freerouting's log file: `FREEROUTING_LOG`, else its default under the XDG
/// state directory (Linux).
fn freerouting_log_path() -> Option<PathBuf> {
    if let Some(path) = std::env::var_os("FREEROUTING_LOG") {
        return Some(PathBuf::from(path));
    }
    Some(freerouting_state_dir()?.join("logs/freerouting.log"))
}

/// `$XDG_STATE_HOME/freerouting`, else `~/.local/state/freerouting`: where
/// Freerouting already keeps its logs.
fn freerouting_state_dir() -> Option<PathBuf> {
    let state = std::env::var_os("XDG_STATE_HOME")
        .map(PathBuf::from)
        .or_else(|| {
            std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".local/state"))
        })?;
    Some(state.join("freerouting"))
}

/// HotSpot writes a crash report (`hs_err_pid*.log`) and possibly a core dump
/// into its working directory, which it inherits from Copperpour and is
/// usually the KiCad project: a G1 crash once left its report at the root of a
/// project repository. Send the report next to Freerouting's logs (the
/// temporary directory when there is no usable state directory) and never dump
/// core. A relative state directory would resolve against that same working
/// directory, so it is not used.
fn jvm_crash_arguments(state_dir: Option<&Path>) -> Vec<std::ffi::OsString> {
    let directory = state_dir
        .filter(|directory| directory.is_absolute())
        .filter(|directory| std::fs::create_dir_all(directory).is_ok())
        .map(Path::to_path_buf)
        .unwrap_or_else(std::env::temp_dir);
    let mut error_file = std::ffi::OsString::from("-XX:ErrorFile=");
    error_file.push(directory.join("hs_err_pid%p.log"));
    vec![error_file, "-XX:-CreateCoredumpOnCrash".into()]
}

fn read_tail(path: &Path, bytes: u64) -> Option<String> {
    use std::io::{Read, Seek, SeekFrom};
    let mut file = std::fs::File::open(path).ok()?;
    let len = file.metadata().ok()?.len();
    file.seek(SeekFrom::Start(len.saturating_sub(bytes))).ok()?;
    let mut buffer = Vec::new();
    file.read_to_end(&mut buffer).ok()?;
    Some(String::from_utf8_lossy(&buffer).into_owned())
}

/// The last unrouted-connection count logged for the job tagged `tag`: a pass
/// line's "(N unrouted and ...)" or an optimizer line's "incomplete
/// connections: N".
fn latest_unrouted(log: &str, tag: &str) -> Option<u64> {
    log.lines()
        .rev()
        .filter(|line| line.contains(tag))
        .find_map(|line| {
            let count = |marker: &str, before: bool| -> Option<u64> {
                if before {
                    let head = line.split_once(marker)?.0;
                    head.rsplit(['(', ' ']).next()?.parse().ok()
                } else {
                    let tail = line.split_once(marker)?.1;
                    tail.split([',', '.', ' ']).next()?.parse().ok()
                }
            };
            count(" unrouted and ", true).or_else(|| count("incomplete connections: ", false))
        })
}

/// The most recent routing-pass or optimizer line of the job tagged `tag`.
fn latest_activity(log: &str, tag: &str) -> Option<String> {
    log.lines()
        .rev()
        .filter(|line| line.contains(tag))
        .find_map(|line| {
            let text = &line[line.find(tag)? + tag.len()..];
            if let Some(rest) = text.trim_start().strip_prefix("Auto-routing pass #") {
                let pass = rest.split_whitespace().next()?;
                let counts = rest.rsplit_once(" (")?.1;
                let (unrouted, rest) = counts.split_once(" unrouted and ")?;
                let violations = rest.split_once(" violations")?.0;
                Some(format!(
                    "pass {pass}: {unrouted} unrouted, {violations} violations"
                ))
            } else if let Some(rest) = text.trim_start().strip_prefix("Optimizer pass #") {
                // "Optimizer pass #2: optimizer score 73.70 -> 84.26 (IMPROVED, 14.3%), ..."
                // The other "Optimizer pass #2 on board ..." line carries no score delta.
                let (pass, rest) = rest.split_once(": optimizer score ")?;
                let (scores, rest) = rest.split_once(" (")?;
                let verdict = rest.split([',', ')']).next()?.to_ascii_lowercase();
                Some(format!("optimizer pass {pass}: score {scores} ({verdict})"))
            } else if text.contains("Optimization stage started") {
                let field = |name: &str| {
                    text.split_once(name)
                        .and_then(|(_, rest)| rest.split([',', '.']).next())
                };
                Some(
                    match (
                        field("incomplete connections: "),
                        field("clearance violations: "),
                    ) {
                        (Some(unrouted), Some(violations)) => {
                            format!("optimizing, {unrouted} unrouted, {violations} violations")
                        }
                        (None, Some(violations)) => format!("optimizing, {violations} violations"),
                        _ => "optimizing".to_string(),
                    },
                )
            } else {
                None
            }
        })
}

fn validate_tool_contracts(tools: &BTreeMap<String, Value>) -> Result<()> {
    for (tool, paths) in [
        ("enqueue_job", &[(&["body", "session_id"][..], false)][..]),
        (
            "upload_job_input_from_local_file",
            &[(&["jobId"][..], true), (&["filePath"][..], true)][..],
        ),
        ("start_job", &[(&["path", "jobId"][..], true)][..]),
        ("get_job_details", &[(&["path", "jobId"][..], true)][..]),
        (
            "download_job_output_to_local_file",
            &[(&["jobId"][..], true), (&["filePath"][..], true)][..],
        ),
    ] {
        let schema = tools
            .get(tool)
            .with_context(|| format!("Freerouting MCP is missing required tool '{tool}'"))?;
        for (path, leaf_must_be_required) in paths {
            require_schema_path(tool, schema, path, *leaf_must_be_required)?;
        }
    }
    Ok(())
}

fn require_schema_path(
    tool: &str,
    schema: &Value,
    path: &[&str],
    leaf_must_be_required: bool,
) -> Result<()> {
    let mut current = schema;
    for (index, key) in path.iter().enumerate() {
        if current.get("type").and_then(Value::as_str) != Some("object") {
            bail!(
                "Freerouting MCP tool '{tool}' has an incompatible input schema at '{}'",
                path.join(".")
            );
        }
        let required = current
            .get("required")
            .and_then(Value::as_array)
            .is_some_and(|values| values.iter().any(|value| value.as_str() == Some(key)));
        if !required && (index + 1 < path.len() || leaf_must_be_required) {
            bail!(
                "Freerouting MCP tool '{tool}' does not require argument '{}'",
                path.join(".")
            );
        }
        current = current
            .get("properties")
            .and_then(|properties| properties.get(*key))
            .with_context(|| {
                format!(
                    "Freerouting MCP tool '{tool}' has no argument '{}'",
                    path.join(".")
                )
            })?;
    }
    Ok(())
}

struct LocalMcpClient {
    child: Option<Child>,
    stdin: ChildStdin,
    stdout: Lines<BufReader<ChildStdout>>,
    stderr_task: Option<JoinHandle<String>>,
    next_id: u64,
    server_protocol_version: String,
}

impl LocalMcpClient {
    async fn start(jar: &Path) -> Result<Self> {
        let mut client = Self::spawn(jar)?;
        if let Err(error) = client.initialize().await {
            let diagnostics = client.close().await;
            return Err(with_stderr_tail(error, &diagnostics, None));
        }
        Ok(client)
    }

    fn server_command(jar: &Path) -> Command {
        let mut command = Command::new(crate::java_runtime::java_executable());
        command
            .args(jvm_crash_arguments(freerouting_state_dir().as_deref()))
            .arg(JAVA_HEADLESS_ARG)
            .arg("-jar")
            .arg(jar)
            .args(server_arguments())
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        command
    }

    fn spawn(jar: &Path) -> Result<Self> {
        Self::spawn_command(Self::server_command(jar))
    }

    /// Starts `command` as the MCP server; it must have piped stdin, stdout
    /// and stderr. Split from [`Self::spawn`] so tests can drive `request()`
    /// against a scripted server instead of the Freerouting JAR.
    fn spawn_command(mut command: Command) -> Result<Self> {
        let mut child = command
            .spawn()
            .context("start local Freerouting MCP server")?;
        #[cfg(test)]
        if let Some(pid) = child.id() {
            LAST_CHILD_PID.store(pid, Ordering::SeqCst);
        }
        let stdin = child.stdin.take().context("Freerouting MCP has no stdin")?;
        let stdout = child
            .stdout
            .take()
            .context("Freerouting MCP has no stdout")?;
        let stderr = child
            .stderr
            .take()
            .context("Freerouting MCP has no stderr")?;
        let stderr_task = tokio::spawn(async move {
            let reader = BufReader::new(stderr);
            let mut bytes = Vec::new();
            let _ = reader
                .take(MAX_STDERR_BYTES as u64)
                .read_to_end(&mut bytes)
                .await;
            String::from_utf8_lossy(&bytes).trim().to_string()
        });
        Ok(Self {
            child: Some(child),
            stdin,
            stdout: BufReader::new(stdout).lines(),
            stderr_task: Some(stderr_task),
            next_id: 1,
            server_protocol_version: String::new(),
        })
    }

    async fn initialize(&mut self) -> Result<()> {
        let initialize = timeout(
            STARTUP_TIMEOUT,
            self.request(
                "initialize",
                json!({
                    "protocolVersion": MCP_PROTOCOL_VERSION,
                    "capabilities": {},
                    "clientInfo": { "name": "Copperpour", "version": env!("CARGO_PKG_VERSION") }
                }),
                STARTUP_TIMEOUT,
            ),
        )
        .await
        .map_err(|_| anyhow::anyhow!("Freerouting MCP initialization timed out"))??;
        self.server_protocol_version = find_string(&initialize, &["protocolVersion"])
            .unwrap_or_else(|| MCP_PROTOCOL_VERSION.to_string());
        self.notify("notifications/initialized", json!({})).await?;
        Ok(())
    }

    async fn list_tools(&mut self) -> Result<BTreeMap<String, Value>> {
        let result = self
            .request("tools/list", json!({}), REQUEST_TIMEOUT)
            .await?;
        let tools = result
            .get("tools")
            .and_then(Value::as_array)
            .context("Freerouting tools/list returned no tools array")?;
        tools
            .iter()
            .map(|tool| {
                let name = tool
                    .get("name")
                    .and_then(Value::as_str)
                    .map(str::to_string)
                    .context("Freerouting tools/list contains an unnamed tool")?;
                let schema = tool
                    .get("inputSchema")
                    .cloned()
                    .context("Freerouting tools/list contains a tool without inputSchema")?;
                Ok((name, schema))
            })
            .collect()
    }

    async fn call_tool(&mut self, name: &str, arguments: Value) -> Result<Value> {
        let result = self
            .request(
                "tools/call",
                json!({ "name": name, "arguments": arguments }),
                REQUEST_TIMEOUT,
            )
            .await
            .with_context(|| format!("call Freerouting MCP tool '{name}'"))?;
        if result.get("isError").and_then(Value::as_bool) == Some(true) {
            bail!(
                "Freerouting MCP tool '{name}' returned an error: {}",
                compact(&result)
            );
        }
        Ok(expand_text_content(result))
    }

    async fn request(&mut self, method: &str, params: Value, limit: Duration) -> Result<Value> {
        let id = self.next_id;
        self.next_id = self.next_id.saturating_add(1);
        self.write_json(&json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": method,
            "params": params
        }))
        .await?;
        let deadline = Instant::now() + limit;
        loop {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                bail!("Freerouting MCP request '{method}' timed out");
            }
            let line = timeout(remaining, self.stdout.next_line())
                .await
                .map_err(|_| anyhow::anyhow!("Freerouting MCP request '{method}' timed out"))??
                .context("Freerouting MCP closed stdout before responding")?;
            let Some(response) = json_rpc_message(&line) else {
                if !line.trim().is_empty() {
                    tracing::debug!(
                        method,
                        line = %line,
                        "skipped a Freerouting MCP stdout line that is not a JSON-RPC message"
                    );
                }
                continue;
            };
            if response.get("id") != Some(&json!(id)) {
                if response.get("method").is_some() {
                    continue;
                }
                bail!("Freerouting MCP returned an unexpected response id");
            }
            if let Some(error) = response.get("error") {
                bail!(
                    "Freerouting MCP request '{method}' failed: {}",
                    compact(error)
                );
            }
            return response
                .get("result")
                .cloned()
                .context("Freerouting MCP response has no result");
        }
    }

    async fn notify(&mut self, method: &str, params: Value) -> Result<()> {
        self.write_json(&json!({ "jsonrpc": "2.0", "method": method, "params": params }))
            .await
    }

    async fn write_json(&mut self, value: &Value) -> Result<()> {
        let mut bytes = serde_json::to_vec(value)?;
        bytes.push(b'\n');
        self.stdin
            .write_all(&bytes)
            .await
            .context("write Freerouting MCP stdin")?;
        self.stdin
            .flush()
            .await
            .context("flush Freerouting MCP stdin")
    }

    async fn close(mut self) -> String {
        let _ = self.stdin.shutdown().await;
        if let Some(mut child) = self.child.take() {
            if timeout(SHUTDOWN_TIMEOUT, child.wait()).await.is_err() {
                let _ = child.kill().await;
                let _ = child.wait().await;
            }
        }
        match self.stderr_task.take() {
            Some(task) => task.await.unwrap_or_default(),
            None => String::new(),
        }
    }
}

impl Drop for LocalMcpClient {
    fn drop(&mut self) {
        if let Some(mut child) = self.child.take() {
            let _ = child.start_kill();
            if let Ok(runtime) = tokio::runtime::Handle::try_current() {
                runtime.spawn(async move {
                    let _ = child.wait().await;
                });
            }
        }
        if let Some(task) = self.stderr_task.take() {
            task.abort();
        }
    }
}

/// A stdout line that is a JSON-RPC message, or `None` for anything else.
///
/// Freerouting 2.5 prints its routing progress bar (`#`, one line per tick)
/// on the same stdout as the MCP stdio transport. Failing on such a line used
/// to abort every routing job long enough to show progress, so non-message
/// lines are skipped; a server that never answers still hits the request
/// timeout.
fn json_rpc_message(line: &str) -> Option<Value> {
    let line = line.trim();
    if !line.starts_with('{') {
        return None;
    }
    // A JSON value that starts with `{` can only be an object.
    serde_json::from_str::<Value>(line).ok()
}

fn expand_text_content(mut result: Value) -> Value {
    let parsed = result
        .get("content")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|item| item.get("text").and_then(Value::as_str))
        .filter_map(|text| serde_json::from_str::<Value>(text).ok())
        .collect::<Vec<_>>();
    if let Some(object) = result.as_object_mut() {
        object.insert("parsedTextContent".to_string(), Value::Array(parsed));
    }
    result
}

fn required_string(value: &Value, keys: &[&str], label: &str) -> Result<String> {
    find_string(value, keys).with_context(|| {
        format!(
            "Freerouting MCP response has no {label}: {}",
            compact(value)
        )
    })
}

fn find_string(value: &Value, keys: &[&str]) -> Option<String> {
    match value {
        Value::Object(object) => {
            for (key, value) in object {
                if keys
                    .iter()
                    .any(|candidate| key.eq_ignore_ascii_case(candidate))
                {
                    if let Some(value) = value.as_str() {
                        return Some(value.to_string());
                    }
                }
            }
            object.values().find_map(|value| find_string(value, keys))
        }
        Value::Array(values) => values.iter().find_map(|value| find_string(value, keys)),
        _ => None,
    }
}

fn absolute_utf8(path: &Path) -> Result<String> {
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()?.join(path)
    };
    absolute
        .to_str()
        .map(str::to_string)
        .with_context(|| format!("path is not UTF-8: {}", absolute.display()))
}

fn temporary_output_path(output: &Path) -> Result<PathBuf> {
    let parent = output.parent().unwrap_or_else(|| Path::new("."));
    let name = output
        .file_name()
        .and_then(|name| name.to_str())
        .context("SES output has no UTF-8 filename")?;
    Ok(parent.join(format!(
        ".{name}.copperpour-{}.tmp.ses",
        uuid::Uuid::new_v4()
    )))
}

/// The `update_job_settings` body, in the keys Freerouting 2.5 deserializes
/// (its `RouterSettings` JSON names, read back through
/// `get_effective_settings`). The camelCase `maxPasses` and `jobTimeoutString`
/// sent before were accepted and silently ignored: routing passes stayed
/// unlimited and the job limit at 12 hours.
///
/// The project's design rules go under what the caller asked for, which wins.
fn job_settings_body(settings: &RouteSettings) -> Map<String, Value> {
    let mut body = settings.project_rules.clone();
    merge_into(&mut body, requested_settings(settings));
    body
}

/// What the caller asked for: `router_settings` with the named arguments on
/// top. Every one of these must be applied, or the run is refused.
fn requested_settings(settings: &RouteSettings) -> Map<String, Value> {
    let mut body = settings.router_settings.clone().unwrap_or_default();
    merge_into(&mut body, named_settings(settings));
    body
}

/// The project rules the caller did not override, so the ones sent as read
/// from the project. Project rules are top-level numbers, so a top-level key
/// in `requested` overrides one.
fn project_only_settings(
    settings: &RouteSettings,
    requested: &Map<String, Value>,
) -> Map<String, Value> {
    settings
        .project_rules
        .iter()
        .filter(|(key, _)| !requested.contains_key(*key))
        .map(|(key, value)| (key.clone(), value.clone()))
        .collect()
}

/// KiCad design rules (`board.design_settings.rules` in a `.kicad_pro`, in
/// mm) and the Freerouting setting (in um) each one is sent as.
pub(crate) const PROJECT_RULES: [(&str, &str); 3] = [
    ("min_copper_edge_clearance", "copper_to_edge_clearance_um"),
    ("min_hole_to_hole", "hole_to_hole_um"),
    ("min_hole_clearance", "hole_clearance_um"),
];

/// Freerouting settings from a KiCad project's design rules. Freerouting does
/// not get these from the DSN: KiCad leaves the board edge clearance out of
/// it, and the hole rules have no Specctra form. Without them, its job API
/// routes with none at all. A rule that is absent, not a number, or not above
/// 0 is not sent, so Freerouting keeps its own default for it.
pub(crate) fn project_route_rules(project: &Value) -> Map<String, Value> {
    let rules = &project["board"]["design_settings"]["rules"];
    PROJECT_RULES
        .iter()
        .filter_map(|(kicad, freerouting)| {
            let mm = rules[*kicad].as_f64().filter(|mm| *mm > 0.0)?;
            Some((freerouting.to_string(), json!((mm * 1000.0).round())))
        })
        .collect()
}

/// The settings that have their own route_specctra_dsn argument.
fn named_settings(settings: &RouteSettings) -> Map<String, Value> {
    let mut body = Map::new();
    if let Some(max_passes) = settings.max_passes {
        body.insert(
            "autorouter".to_string(),
            json!({ "max_passes": max_passes }),
        );
    }
    let mut optimizer = Map::new();
    if let Some(enabled) = settings.optimizer_enabled {
        optimizer.insert("enabled".to_string(), json!(enabled));
    }
    if let Some(passes) = settings.optimizer_max_passes {
        optimizer.insert("max_passes".to_string(), json!(passes));
    }
    if let Some(percent) = settings.optimizer_improvement_threshold {
        optimizer.insert("improvement_threshold".to_string(), json!(percent));
    }
    if !optimizer.is_empty() {
        body.insert("optimizer".to_string(), Value::Object(optimizer));
    }
    if let Some(nets) = &settings.plane_nets {
        body.insert("plane_nets".to_string(), json!(nets));
    }
    if let Some(obstacle) = settings.plane_as_obstacle {
        body.insert("plane_as_obstacle".to_string(), json!(obstacle));
    }
    if let Some(seconds) = settings.job_timeout_seconds {
        body.insert(
            "job_timeout".to_string(),
            Value::String(duration_string(seconds)),
        );
    }
    body
}

/// Deep-merge `overlay` into `base`: objects merge key by key, anything else
/// in `overlay` replaces what `base` had.
fn merge_into(base: &mut Map<String, Value>, overlay: Map<String, Value>) {
    for (key, value) in overlay {
        match (base.get_mut(&key), value) {
            (Some(Value::Object(existing)), Value::Object(incoming)) => {
                merge_into(existing, incoming)
            }
            (_, value) => {
                base.insert(key, value);
            }
        }
    }
}

/// The settings object inside a `get_effective_settings` result, which wraps
/// it as `{status, contentType, body}` in its text content.
fn effective_settings(result: &Value) -> Option<&Map<String, Value>> {
    let parsed = result.get("parsedTextContent")?.as_array()?.first()?;
    parsed.get("body").unwrap_or(parsed).as_object()
}

/// Every leaf of `sent` whose value is not found at the same path in
/// `effective`, as "path: sent X, effective Y" (or "not a setting").
///
/// Objects and arrays are walked, arrays element by element (`layers[0]`):
/// Freerouting fills in what an element leaves out with its defaults, and
/// keeps elements nobody sent (the other layers), so neither is a mismatch.
/// A sent element with no counterpart in the effective array is one.
fn settings_mismatches(sent: &Map<String, Value>, effective: &Map<String, Value>) -> Vec<String> {
    fn walk(sent: &Value, effective: &Value, path: &str, out: &mut Vec<String>) {
        match (sent, effective) {
            (Value::Object(sent), Value::Object(effective)) => {
                for (key, value) in sent {
                    let here = if path.is_empty() {
                        key.clone()
                    } else {
                        format!("{path}.{key}")
                    };
                    match effective.get(key) {
                        None => out.push(format!("{here}: not a Freerouting setting")),
                        Some(effective) => walk(value, effective, &here, out),
                    }
                }
            }
            (Value::Array(sent), Value::Array(effective)) => {
                for (index, value) in sent.iter().enumerate() {
                    let here = format!("{path}[{index}]");
                    match effective.get(index) {
                        None => out.push(format!(
                            "{here}: sent {value}, not in the effective settings"
                        )),
                        Some(effective) => walk(value, effective, &here, out),
                    }
                }
            }
            (sent, effective) if !same_value(sent, effective) => {
                out.push(format!("{path}: sent {sent}, effective {effective}"))
            }
            _ => {}
        }
    }
    let mut out = Vec::new();
    walk(
        &Value::Object(sent.clone()),
        &Value::Object(effective.clone()),
        "",
        &mut out,
    );
    out
}

/// Equality that lets 3 match 3.0: Freerouting echoes some integers as floats.
fn same_value(a: &Value, b: &Value) -> bool {
    match (a.as_f64(), b.as_f64()) {
        (Some(x), Some(y)) => (x - y).abs() <= 1e-9 * x.abs().max(1.0),
        _ => a == b,
    }
}

fn duration_string(seconds: u64) -> String {
    let hours = seconds / 3600;
    let minutes = (seconds % 3600) / 60;
    let seconds = seconds % 60;
    format!("{hours:02}:{minutes:02}:{seconds:02}")
}

fn extension_is(path: &Path, expected: &str) -> bool {
    path.extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| extension.eq_ignore_ascii_case(expected))
}

fn compact(value: &Value) -> String {
    let text = value.to_string();
    if text.chars().count() > 2_000 {
        let truncated: String = text.chars().take(2_000).collect();
        format!("{truncated}...")
    } else {
        text
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use copperpour_ipc::{IpcEffectiveRoutingRules, IpcRoutingRules};

    #[cfg(windows)]
    fn process_is_running(pid: u32) -> bool {
        let output = std::process::Command::new("tasklist")
            .args(["/FI", &format!("PID eq {pid}"), "/FO", "CSV", "/NH"])
            .output()
            .expect("run tasklist");
        String::from_utf8_lossy(&output.stdout).contains(&format!("\"{pid}\""))
    }

    #[cfg(not(windows))]
    fn process_is_running(pid: u32) -> bool {
        std::process::Command::new("sh")
            .args(["-c", &format!("kill -0 {pid} 2>/dev/null")])
            .status()
            .expect("probe process")
            .success()
    }

    async fn wait_for_process_exit(pid: u32) -> bool {
        for _ in 0..50 {
            if !process_is_running(pid) {
                return true;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        false
    }

    fn routing_rules() -> IpcEffectiveRoutingRules {
        ["GND", "VCC"]
            .into_iter()
            .map(|net| {
                (
                    net.to_string(),
                    IpcRoutingRules {
                        class_name: "Default".to_string(),
                        constituents: vec!["Default".to_string()],
                        track_width_mm: Some(0.25),
                        clearance_mm: Some(0.2),
                        via_diameter_mm: Some(0.6),
                        via_drill_mm: Some(0.3),
                    },
                )
            })
            .collect()
    }

    #[test]
    fn progress_output_on_stdout_is_not_a_message() {
        for line in [
            "#",
            "####",
            "",
            "   ",
            "Routing pass 3...",
            "{not json",
            "[1,2]",
        ] {
            assert!(json_rpc_message(line).is_none(), "{line:?}");
        }
        let message = json_rpc_message(r#" {"jsonrpc":"2.0","id":7,"result":{}} "#).unwrap();
        assert_eq!(message["id"], 7);
    }

    /// A client whose "server" is `sh -c script`: it reads the request line,
    /// then prints whatever the script prints.
    #[cfg(unix)]
    fn scripted_client(script: &str) -> LocalMcpClient {
        let mut command = Command::new("sh");
        command
            .args(["-c", script])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        LocalMcpClient::spawn_command(command).expect("spawn scripted MCP server")
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn request_skips_progress_lines_before_the_response() {
        let mut client = scripted_client(
            r#"read line; echo '#'; echo '####'; echo; echo '{"jsonrpc":"2.0","id":1,"result":{"ok":true}}'"#,
        );
        let result = client
            .request("tools/list", json!({}), Duration::from_secs(10))
            .await
            .expect("the response after the progress lines is returned");
        assert_eq!(result, json!({ "ok": true }));
        client.close().await;
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn request_with_only_progress_lines_times_out() {
        let mut client = scripted_client("read line; while true; do echo '#'; sleep 0.1; done");
        let error = client
            .request("tools/list", json!({}), Duration::from_secs(1))
            .await
            .expect_err("progress lines alone are not a response");
        assert!(
            error.to_string().contains("timed out"),
            "unexpected error: {error:#}"
        );
        client.close().await;
    }

    /// The tool schemas `validate_tool_contracts` accepts, as `tools/list`
    /// returns them.
    fn conforming_tools_list() -> Value {
        let object = |properties: Value, required: Value| json!({ "type": "object", "properties": properties, "required": required });
        let file = object(
            json!({ "jobId": { "type": "string" }, "filePath": { "type": "string" } }),
            json!(["jobId", "filePath"]),
        );
        let by_path = object(
            json!({ "path": object(json!({ "jobId": { "type": "string" } }), json!(["jobId"])) }),
            json!(["path"]),
        );
        let tool = |name: &str, schema: &Value| json!({ "name": name, "inputSchema": schema });
        json!({ "tools": [
            tool("create_session", &object(json!({}), json!([]))),
            tool(
                "enqueue_job",
                &object(
                    json!({ "body": object(json!({ "session_id": { "type": "string" } }), json!(["session_id"])) }),
                    json!(["body"]),
                ),
            ),
            tool("upload_job_input_from_local_file", &file),
            tool("start_job", &by_path),
            tool("get_job_details", &by_path),
            tool("cancel_job", &by_path),
            tool("download_job_output_to_local_file", &file),
        ] })
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn an_empty_download_during_optimization_is_retried_at_the_next_poll() {
        let temp = tempfile::tempdir().unwrap();
        let dsn = temp.path().join("board.dsn");
        let temporary = temp.path().join("board.ses.tmp");
        std::fs::write(&dsn, "(pcb board)").unwrap();
        // The job is always optimizing. The first download leaves an empty
        // file, as Freerouting does before it has published a board; the
        // second writes the board.
        let script = temp.path().join("server.sh");
        std::fs::write(
            &script,
            format!(
                r#"n=0; downloads=0
while read -r line; do
  n=$((n+1))
  case "$line" in
    *'"tools/list"'*) result='{tools}' ;;
    *create_session*) result='{{"sessionId":"s1"}}' ;;
    *enqueue_job*) result='{{"jobId":"job123"}}' ;;
    *get_job_details*) result='{{"state":"RUNNING","stage":"OPTIMIZATION"}}' ;;
    *download_job_output_to_local_file*)
      downloads=$((downloads+1))
      if [ "$downloads" -eq 1 ]; then : > '{output}'; else printf '(session board)' > '{output}'; fi
      result='{{}}' ;;
    *cancel_job*) result='{{}}'; last=1 ;;
    *) result='{{}}' ;;
  esac
  printf '{{"jsonrpc":"2.0","id":%s,"result":%s}}\n' "$n" "$result"
  if [ -n "${{last:-}}" ]; then exit 0; fi
done
"#,
                tools = conforming_tools_list(),
                output = temporary.display(),
            ),
        )
        .unwrap();
        // exec: the client's child is the script itself, so close() can reap it.
        let mut client = scripted_client(&format!("exec sh '{}'", script.display()));
        let settings = RouteSettings {
            max_passes: None,
            optimizer_enabled: None,
            job_timeout_seconds: None,
            optimize_seconds: Some(0),
            optimizer_max_passes: None,
            optimizer_improvement_threshold: None,
            plane_nets: None,
            plane_as_obstacle: None,
            router_settings: None,
            project_rules: Map::new(),
            poll_interval: Duration::from_millis(10),
            overall_timeout: Duration::from_secs(30),
        };

        let evidence = timeout(
            Duration::from_secs(20),
            run_state_machine(&mut client, &dsn, &temporary, &settings, Instant::now()),
        )
        .await
        .expect("the scripted job ends")
        .expect("the board from the second download is kept");
        client.close().await;

        assert_eq!(evidence.final_state, "OPTIMIZATION_STOPPED");
        assert_eq!(
            evidence.poll_count, 2,
            "the empty first download must not end the job"
        );
        assert_eq!(
            std::fs::read_to_string(&temporary).unwrap(),
            "(session board)"
        );
    }

    /// A Freerouting server scripted in sh that takes the job settings,
    /// answers `get_effective_settings` with `effective`, and logs the name of
    /// each tool called to `calls`, the `update_job_settings` request to `sent`.
    #[cfg(unix)]
    fn settings_server(temp: &Path, effective: Value) -> (LocalMcpClient, PathBuf, PathBuf) {
        settings_server_reading_back(temp, Some(effective))
    }

    /// [`settings_server`], without `get_effective_settings` when `effective`
    /// is None.
    #[cfg(unix)]
    fn settings_server_reading_back(
        temp: &Path,
        effective: Option<Value>,
    ) -> (LocalMcpClient, PathBuf, PathBuf) {
        let read_back = effective.is_some();
        let temporary = temp.join("board.ses.tmp");
        let sent = temp.join("sent.json");
        let calls = temp.join("calls.log");
        let mut tools = conforming_tools_list();
        let by_path = tools["tools"][3]["inputSchema"].clone();
        let names: &[&str] = if read_back {
            &["update_job_settings", "get_effective_settings"]
        } else {
            &["update_job_settings"]
        };
        for name in names {
            tools["tools"]
                .as_array_mut()
                .unwrap()
                .push(json!({ "name": name, "inputSchema": by_path }));
        }
        // Freerouting 2.5.0 answers get_effective_settings with the settings
        // as JSON text.
        let effective = json!({ "content": [{ "type": "text", "text": json!({
            "status": 200,
            "contentType": "application/json",
            "body": effective.unwrap_or_default()
        }).to_string() }] });
        let script = temp.join("server.sh");
        std::fs::write(
            &script,
            format!(
                r#"n=0
while read -r line; do
  n=$((n+1))
  tool=$(printf '%s' "$line" | sed -n 's/.*"name":"\([a-z_]*\)".*/\1/p')
  printf '%s\n' "${{tool:-tools/list}}" >> '{calls}'
  case "$line" in
    *'"tools/list"'*) result='{tools}' ;;
    *create_session*) result='{{"sessionId":"s1"}}' ;;
    *enqueue_job*) result='{{"jobId":"job123"}}' ;;
    *update_job_settings*) printf '%s\n' "$line" > '{sent}'; result='{{}}' ;;
    *get_effective_settings*) result='{effective}' ;;
    *get_job_details*) result='{{"state":"COMPLETED"}}' ;;
    *download_job_output_to_local_file*) printf '(session board)' > '{output}'; result='{{}}'; last=1 ;;
    *) result='{{}}' ;;
  esac
  printf '{{"jsonrpc":"2.0","id":%s,"result":%s}}\n' "$n" "$result"
  if [ -n "${{last:-}}" ]; then exit 0; fi
done
"#,
                sent = sent.display(),
                calls = calls.display(),
                output = temporary.display(),
            ),
        )
        .unwrap();
        let client = scripted_client(&format!("exec sh '{}'", script.display()));
        (client, sent, calls)
    }

    /// `max_passes` named on top of a `router_settings` that also sets it,
    /// a misspelt key and a scoring value.
    #[cfg(unix)]
    fn merged_settings() -> RouteSettings {
        RouteSettings {
            max_passes: Some(7),
            optimizer_enabled: None,
            job_timeout_seconds: None,
            optimize_seconds: None,
            optimizer_max_passes: None,
            optimizer_improvement_threshold: None,
            plane_nets: None,
            plane_as_obstacle: None,
            router_settings: json!({
                "autorouter": { "max_passes": 99, "max_threads": 4, "max_pases": 3 },
                "scoring": { "via_costs": 80 }
            })
            .as_object()
            .cloned(),
            project_rules: Map::new(),
            poll_interval: Duration::from_millis(10),
            overall_timeout: Duration::from_secs(30),
        }
    }

    /// Freerouting kept max_passes but dropped the misspelt key and replaced
    /// via_costs: the call fails naming both, and the job is never started.
    #[cfg(unix)]
    #[tokio::test]
    async fn settings_freerouting_did_not_apply_are_refused_before_start_job() {
        let temp = tempfile::tempdir().unwrap();
        let dsn = temp.path().join("board.dsn");
        let temporary = temp.path().join("board.ses.tmp");
        std::fs::write(&dsn, "(pcb board)").unwrap();
        let (mut client, sent, calls) = settings_server(
            temp.path(),
            json!({
                "autorouter": { "max_passes": 7, "max_threads": 4 },
                "scoring": { "via_costs": 50 }
            }),
        );

        let error = timeout(
            Duration::from_secs(20),
            run_state_machine(
                &mut client,
                &dsn,
                &temporary,
                &merged_settings(),
                Instant::now(),
            ),
        )
        .await
        .expect("the refusal comes before any poll")
        .expect_err("settings that did not take are refused");
        client.close().await;

        let message = format!("{error:#}");
        assert!(message.contains("nothing was routed"), "{message}");
        assert!(
            message.contains("autorouter.max_pases: not a Freerouting setting"),
            "{message}"
        );
        assert!(
            message.contains("scoring.via_costs: sent 80, effective 50"),
            "{message}"
        );
        assert!(!message.contains("max_threads"), "{message}");
        let request: Value =
            serde_json::from_str(&std::fs::read_to_string(&sent).unwrap()).unwrap();
        assert_eq!(
            request["params"]["arguments"]["body"],
            json!({
                "autorouter": { "max_passes": 7, "max_threads": 4, "max_pases": 3 },
                "scoring": { "via_costs": 80 }
            })
        );
        let calls = std::fs::read_to_string(&calls).unwrap();
        assert!(calls.contains("get_effective_settings"), "{calls}");
        assert!(!calls.contains("start_job"), "{calls}");
        assert!(!temporary.exists());
    }

    /// Every setting sent found with its value among the effective ones (3
    /// echoed as 3.0 included): the job runs, with Freerouting's warnings.
    #[cfg(unix)]
    #[tokio::test]
    async fn router_settings_that_took_are_sent_merged_and_routed() {
        let temp = tempfile::tempdir().unwrap();
        let dsn = temp.path().join("board.dsn");
        let temporary = temp.path().join("board.ses.tmp");
        std::fs::write(&dsn, "(pcb board)").unwrap();
        let (mut client, sent, calls) = settings_server(
            temp.path(),
            json!({
                "autorouter": { "max_passes": 7, "max_threads": 4, "max_pases": 3.0 },
                "scoring": { "via_costs": 80, "bend_penalty": 2 },
                "validation_warnings": ["max_threads above the CPU count"]
            }),
        );

        let evidence = timeout(
            Duration::from_secs(20),
            run_state_machine(
                &mut client,
                &dsn,
                &temporary,
                &merged_settings(),
                Instant::now(),
            ),
        )
        .await
        .expect("the scripted job ends")
        .expect("the routed board is kept");
        client.close().await;

        let request: Value =
            serde_json::from_str(&std::fs::read_to_string(&sent).unwrap()).unwrap();
        assert_eq!(
            request["params"]["arguments"]["body"],
            json!({
                "autorouter": { "max_passes": 7, "max_threads": 4, "max_pases": 3 },
                "scoring": { "via_costs": 80 }
            })
        );
        assert!(std::fs::read_to_string(&calls)
            .unwrap()
            .contains("start_job"));
        assert_eq!(evidence.final_state, "COMPLETED");
        assert!(evidence.settings_checked);
        assert_eq!(
            evidence.settings_warnings,
            [json!("max_threads above the CPU count")]
        );
    }

    /// A server without get_effective_settings cannot say what took: the job
    /// runs, misspelt key included, and the evidence says it was not checked.
    #[cfg(unix)]
    #[tokio::test]
    async fn settings_that_cannot_be_read_back_are_routed_unchecked() {
        let temp = tempfile::tempdir().unwrap();
        let dsn = temp.path().join("board.dsn");
        let temporary = temp.path().join("board.ses.tmp");
        std::fs::write(&dsn, "(pcb board)").unwrap();
        let (mut client, _sent, calls) = settings_server_reading_back(temp.path(), None);

        let evidence = timeout(
            Duration::from_secs(20),
            run_state_machine(
                &mut client,
                &dsn,
                &temporary,
                &merged_settings(),
                Instant::now(),
            ),
        )
        .await
        .expect("the scripted job ends")
        .expect("the routed board is kept");
        client.close().await;

        let calls = std::fs::read_to_string(&calls).unwrap();
        assert!(!calls.contains("get_effective_settings"), "{calls}");
        assert!(calls.contains("start_job"), "{calls}");
        assert_eq!(evidence.final_state, "COMPLETED");
        assert!(!evidence.settings_checked);
        assert!(evidence.settings_warnings.is_empty());
    }

    /// The count `route_response` decides on comes from the job's own lines
    /// in Freerouting's log, read when the job ends.
    /// Only `project_rules` and, optionally, a `router_settings`.
    fn with_project_rules(project_rules: Value, router_settings: Option<Value>) -> RouteSettings {
        RouteSettings {
            max_passes: None,
            optimizer_enabled: None,
            job_timeout_seconds: None,
            optimize_seconds: None,
            optimizer_max_passes: None,
            optimizer_improvement_threshold: None,
            plane_nets: None,
            plane_as_obstacle: None,
            router_settings: router_settings.and_then(|value| value.as_object().cloned()),
            project_rules: project_rules.as_object().unwrap().clone(),
            poll_interval: Duration::from_millis(10),
            overall_timeout: Duration::from_secs(30),
        }
    }

    #[test]
    fn project_design_rules_become_freerouting_settings_in_um() {
        let project = json!({ "board": { "design_settings": { "rules": {
            "min_copper_edge_clearance": 0.5,
            "min_hole_to_hole": 0.25,
            "min_hole_clearance": 0.0,
            "min_clearance": 0.2
        } } } });
        assert_eq!(
            Value::Object(project_route_rules(&project)),
            json!({ "copper_to_edge_clearance_um": 500.0, "hole_to_hole_um": 250.0 })
        );
        // Below zero, not a number, or absent: Freerouting keeps its default.
        let unusable = json!({ "board": { "design_settings": { "rules": {
            "min_copper_edge_clearance": -0.5,
            "min_hole_to_hole": "0.25"
        } } } });
        assert!(project_route_rules(&unusable).is_empty());
        assert!(project_route_rules(&json!({})).is_empty());
        // Rounded to the micrometre Freerouting takes.
        let fine = json!({ "board": { "design_settings": { "rules": {
            "min_hole_clearance": 0.2504
        } } } });
        assert_eq!(
            Value::Object(project_route_rules(&fine)),
            json!({ "hole_clearance_um": 250.0 })
        );
    }

    #[test]
    fn project_rules_go_under_what_the_caller_asked_for() {
        let settings = with_project_rules(
            json!({ "copper_to_edge_clearance_um": 500.0, "hole_to_hole_um": 500.0 }),
            Some(json!({ "copper_to_edge_clearance_um": 300.0 })),
        );
        assert_eq!(
            Value::Object(job_settings_body(&settings)),
            json!({ "copper_to_edge_clearance_um": 300.0, "hole_to_hole_um": 500.0 })
        );
        let requested = requested_settings(&settings);
        assert_eq!(
            Value::Object(requested.clone()),
            json!({ "copper_to_edge_clearance_um": 300.0 })
        );
        // Only the rule the caller left alone is checked leniently.
        assert_eq!(
            Value::Object(project_only_settings(&settings, &requested)),
            json!({ "hole_to_hole_um": 500.0 })
        );
    }

    /// The JAR does not know `hole_to_hole_um` (Freerouting 2.5.0): the run
    /// goes on and says so, and the caller's `hole_clearance_um` replaces the
    /// project's.
    #[cfg(unix)]
    #[tokio::test]
    async fn a_project_rule_freerouting_did_not_apply_is_reported_not_refused() {
        let temp = tempfile::tempdir().unwrap();
        let dsn = temp.path().join("board.dsn");
        let temporary = temp.path().join("board.ses.tmp");
        std::fs::write(&dsn, "(pcb board)").unwrap();
        let (mut client, sent, calls) = settings_server(
            temp.path(),
            json!({ "copper_to_edge_clearance_um": 500.0, "hole_clearance_um": 200 }),
        );
        let settings = with_project_rules(
            json!({
                "copper_to_edge_clearance_um": 500.0,
                "hole_to_hole_um": 250.0,
                "hole_clearance_um": 300.0
            }),
            Some(json!({ "hole_clearance_um": 200 })),
        );

        let evidence = timeout(
            Duration::from_secs(20),
            run_state_machine(&mut client, &dsn, &temporary, &settings, Instant::now()),
        )
        .await
        .expect("the scripted job ends")
        .expect("a project rule the JAR skipped does not refuse the run");
        client.close().await;

        let request: Value =
            serde_json::from_str(&std::fs::read_to_string(&sent).unwrap()).unwrap();
        assert_eq!(
            request["params"]["arguments"]["body"],
            json!({
                "copper_to_edge_clearance_um": 500.0,
                "hole_to_hole_um": 250.0,
                "hole_clearance_um": 200
            })
        );
        assert_eq!(
            evidence.project_rules_not_applied,
            ["hole_to_hole_um: not a Freerouting setting"]
        );
        assert!(std::fs::read_to_string(&calls)
            .unwrap()
            .contains("start_job"));
    }

    /// The project has the same rule: what the caller asked for is still
    /// checked strictly, and refused when Freerouting did not apply it.
    #[cfg(unix)]
    #[tokio::test]
    async fn a_requested_setting_over_a_project_rule_is_still_refused_when_not_applied() {
        let temp = tempfile::tempdir().unwrap();
        let dsn = temp.path().join("board.dsn");
        let temporary = temp.path().join("board.ses.tmp");
        std::fs::write(&dsn, "(pcb board)").unwrap();
        let (mut client, _sent, calls) =
            settings_server(temp.path(), json!({ "copper_to_edge_clearance_um": 250.0 }));
        let settings = with_project_rules(
            json!({ "copper_to_edge_clearance_um": 500.0 }),
            Some(json!({ "copper_to_edge_clearance_um": 300.0 })),
        );

        let error = timeout(
            Duration::from_secs(20),
            run_state_machine(&mut client, &dsn, &temporary, &settings, Instant::now()),
        )
        .await
        .expect("the refusal comes before any poll")
        .expect_err("the caller's setting did not take");
        client.close().await;

        let message = format!("{error:#}");
        assert!(
            message.contains("copper_to_edge_clearance_um: sent 300.0, effective 250.0"),
            "{message}"
        );
        assert!(!std::fs::read_to_string(&calls)
            .unwrap()
            .contains("start_job"));
    }

    /// A JAR without `update_job_settings` routes as it did before project
    /// rules were sent, and lists each one as not sent. Asked-for settings
    /// are still refused there.
    #[cfg(unix)]
    #[tokio::test]
    async fn project_rules_alone_do_not_need_update_job_settings() {
        let temp = tempfile::tempdir().unwrap();
        let dsn = temp.path().join("board.dsn");
        let temporary = temp.path().join("board.ses.tmp");
        std::fs::write(&dsn, "(pcb board)").unwrap();
        let script = temp.path().join("server.sh");
        std::fs::write(
            &script,
            format!(
                r#"n=0
while read -r line; do
  n=$((n+1))
  case "$line" in
    *'"tools/list"'*) result='{tools}' ;;
    *create_session*) result='{{"sessionId":"s1"}}' ;;
    *enqueue_job*) result='{{"jobId":"job123"}}' ;;
    *get_job_details*) result='{{"state":"COMPLETED"}}' ;;
    *download_job_output_to_local_file*) printf '(session board)' > '{output}'; result='{{}}'; last=1 ;;
    *) result='{{}}' ;;
  esac
  printf '{{"jsonrpc":"2.0","id":%s,"result":%s}}\n' "$n" "$result"
  if [ -n "${{last:-}}" ]; then exit 0; fi
done
"#,
                tools = conforming_tools_list(),
                output = temporary.display(),
            ),
        )
        .unwrap();
        let project = json!({ "copper_to_edge_clearance_um": 500.0 });

        let mut client = scripted_client(&format!("exec sh '{}'", script.display()));
        let evidence = timeout(
            Duration::from_secs(20),
            run_state_machine(
                &mut client,
                &dsn,
                &temporary,
                &with_project_rules(project.clone(), None),
                Instant::now(),
            ),
        )
        .await
        .expect("the scripted job ends")
        .expect("project rules alone do not fail the run");
        client.close().await;
        assert_eq!(evidence.final_state, "COMPLETED");
        assert_eq!(
            evidence.project_rules_not_applied,
            ["copper_to_edge_clearance_um: not sent, this Freerouting has no update_job_settings"]
        );

        std::fs::remove_file(&temporary).unwrap();
        let mut client = scripted_client(&format!("exec sh '{}'", script.display()));
        let error = timeout(
            Duration::from_secs(20),
            run_state_machine(
                &mut client,
                &dsn,
                &temporary,
                &with_project_rules(project, Some(json!({ "hole_clearance_um": 200 }))),
                Instant::now(),
            ),
        )
        .await
        .expect("the refusal comes before any poll")
        .expect_err("asked-for settings need update_job_settings");
        client.close().await;
        assert!(
            format!("{error:#}").contains("does not expose update_job_settings"),
            "{error:#}"
        );
    }

    /// The count `route_response` decides on comes from the job's own lines
    /// in Freerouting's log, read when the job ends.
    #[cfg(unix)]
    #[tokio::test]
    async fn a_completed_job_reports_the_unrouted_count_from_its_log() {
        let temp = tempfile::tempdir().unwrap();
        let dsn = temp.path().join("board.dsn");
        let temporary = temp.path().join("board.ses.tmp");
        let log = temp.path().join("freerouting.log");
        std::fs::write(&dsn, "(pcb board)").unwrap();
        // A job id no other test uses: FREEROUTING_LOG is process-wide, and
        // the other scripted jobs' tags find no line of theirs here.
        std::fs::write(
            &log,
            "\
2026-10-29 17:52:37.907 INFO   [BEEF01] Auto-routing pass #29 on board 'a' was completed in 17.53 seconds with score 944.89 (5 unrouted and 0 violations), using 1.00 CPU seconds.
2026-10-29 17:52:39.000 INFO   [BEEF01] Auto-routing pass #30 on board 'a' was completed in 17.53 seconds with score 950.12 (3 unrouted and 0 violations), using 1.00 CPU seconds.
",
        )
        .unwrap();
        let script = temp.path().join("server.sh");
        std::fs::write(
            &script,
            format!(
                r#"n=0
while read -r line; do
  n=$((n+1))
  case "$line" in
    *'"tools/list"'*) result='{tools}' ;;
    *create_session*) result='{{"sessionId":"s1"}}' ;;
    *enqueue_job*) result='{{"jobId":"beef01aa"}}' ;;
    *get_job_details*) result='{{"state":"COMPLETED"}}' ;;
    *download_job_output_to_local_file*) printf '(session board)' > '{output}'; result='{{}}'; last=1 ;;
    *) result='{{}}' ;;
  esac
  printf '{{"jsonrpc":"2.0","id":%s,"result":%s}}\n' "$n" "$result"
  if [ -n "${{last:-}}" ]; then exit 0; fi
done
"#,
                tools = conforming_tools_list(),
                output = temporary.display(),
            ),
        )
        .unwrap();
        let mut client = scripted_client(&format!("exec sh '{}'", script.display()));
        let settings = RouteSettings {
            max_passes: None,
            optimizer_enabled: None,
            job_timeout_seconds: None,
            optimize_seconds: None,
            optimizer_max_passes: None,
            optimizer_improvement_threshold: None,
            plane_nets: None,
            plane_as_obstacle: None,
            router_settings: None,
            project_rules: Map::new(),
            poll_interval: Duration::from_millis(10),
            overall_timeout: Duration::from_secs(30),
        };

        let previous = std::env::var_os("FREEROUTING_LOG");
        std::env::set_var("FREEROUTING_LOG", &log);
        let evidence = timeout(
            Duration::from_secs(20),
            run_state_machine(&mut client, &dsn, &temporary, &settings, Instant::now()),
        )
        .await;
        match previous {
            Some(value) => std::env::set_var("FREEROUTING_LOG", value),
            None => std::env::remove_var("FREEROUTING_LOG"),
        }
        let evidence = evidence
            .expect("the scripted job ends")
            .expect("the routed board is kept");
        client.close().await;

        assert_eq!(evidence.final_state, "COMPLETED");
        assert_eq!(evidence.unrouted, Some(3), "the last count of the job");
        assert_eq!(
            evidence.freerouting_log.as_deref(),
            Some(log.to_str().unwrap()),
            "the log the count was read from"
        );
    }

    #[test]
    fn routing_activity_is_read_from_the_jobs_own_log_lines() {
        // Lines as Freerouting 2.5.0 writes them; [B00000] is another job.
        let log = "\
2026-10-29 17:52:37.907 INFO   [A3AA32] Auto-routing pass #7 on board '7c06' was completed in 17.53 seconds with score 944.89 (9 unrouted and 218 violations), using 194.58 CPU seconds and the job allocated 188.63 GB of memory so far.
2026-10-29 17:52:38.001 DEBUG  [A3AA32]   Net 'SCK2' has 4 incomplete(s)
2026-10-29 17:52:39.000 INFO   [B00000] Auto-routing pass #2 on board 'ffff' was completed in 1.00 seconds with score 1.00 (50 unrouted and 3 violations), using 1.00 CPU seconds.
";
        assert_eq!(
            latest_activity(log, "[A3AA32]").as_deref(),
            Some("pass 7: 9 unrouted, 218 violations")
        );
        let optimizing = format!(
            "{log}2026-10-29 17:54:26.961 INFO   [A3AA32] Optimization stage started on board 'ce32'. Baseline router score: 980.71, optimizer score: 71.70, incomplete connections: 0, clearance violations: 218.\n"
        );
        assert_eq!(
            latest_activity(&optimizing, "[A3AA32]").as_deref(),
            Some("optimizing, 0 unrouted, 218 violations")
        );
        let optimizer_pass = format!(
            "{optimizing}2026-10-29 17:58:00.204 INFO   [A3AA32] Optimizer pass #2 on board '008e' was completed in 152.08 seconds with the score of 84.26 (0 unrouted and 218 violations).
2026-10-29 17:58:00.204 INFO   [A3AA32] Optimizer pass #2: optimizer score 73.70 -> 84.26 (IMPROVED, 14.3278%), router score: 980.71, incomplete connections: 0, clearance violations: 218.\n"
        );
        assert_eq!(
            latest_activity(&optimizer_pass, "[A3AA32]").as_deref(),
            Some("optimizer pass 2: score 73.70 -> 84.26 (improved)")
        );
        assert_eq!(latest_activity(log, "[C11111]"), None);
    }

    #[test]
    fn an_unchanged_job_is_reported_once_then_at_each_heartbeat() {
        let mut progress = JobProgress::new("a3aa32eb");
        progress.log = None;
        let details = json!({ "current_pass": 3 });
        let started = Instant::now();
        let at = |seconds| started + Duration::from_secs(seconds);

        assert_eq!(
            progress
                .next_message("RUNNING", &details, started, at(5))
                .as_deref(),
            Some("Freerouting running, pass 3 (0m05s)")
        );
        // Same state, inside the heartbeat: nothing new to say.
        assert_eq!(
            progress.next_message("RUNNING", &details, started, at(10)),
            None
        );
        assert_eq!(
            progress.next_message("RUNNING", &details, started, at(64)),
            None
        );
        // Still the same, but a minute after the last report: the heartbeat.
        assert_eq!(
            progress
                .next_message("RUNNING", &details, started, at(65))
                .as_deref(),
            Some("Freerouting running, pass 3 (1m05s)")
        );
        // A change is reported at once, and progress keeps increasing.
        assert_eq!(
            progress
                .next_message("RUNNING", &json!({ "current_pass": 4 }), started, at(66))
                .as_deref(),
            Some("Freerouting running, pass 4 (1m06s)")
        );
        assert_eq!(progress.sent, 3);
    }

    #[test]
    fn the_log_activity_replaces_the_pass_number() {
        let mut progress = JobProgress::new("a3aa32eb");
        progress.log = None;
        progress.activity = Some("pass 7: 9 unrouted, 218 violations".to_string());
        let started = Instant::now();
        assert_eq!(
            progress
                .next_message("RUNNING", &json!({ "current_pass": 3 }), started, started)
                .as_deref(),
            Some("Freerouting running, pass 7: 9 unrouted, 218 violations (0m00s)")
        );
    }

    #[test]
    fn every_terminal_job_state_ends_the_poll_loop() {
        for (state, expected) in [
            ("COMPLETED", JobState::Completed),
            ("completed", JobState::Completed),
            ("FAILED", JobState::Ended),
            ("CANCELLED", JobState::Ended),
            ("CANCELED", JobState::Ended),
            ("TIMED_OUT", JobState::EndedWithoutOutput),
            ("TERMINATED", JobState::EndedWithoutOutput),
            ("INVALID", JobState::EndedWithoutOutput),
            ("timed_out", JobState::EndedWithoutOutput),
            ("QUEUED", JobState::Running),
            ("RUNNING", JobState::Running),
            ("SOMETHING_NEW", JobState::Running),
        ] {
            assert_eq!(JobState::of(state), expected, "{state}");
        }
    }

    #[test]
    fn the_optimizer_bound_counts_from_the_first_poll_that_sees_the_optimizer() {
        let start = Instant::now();
        let at = |seconds| start + Duration::from_secs(seconds);

        let mut bound = OptimizerBound::new(Some(60));
        // Still routing: however long it takes, the bound does not run.
        assert!(!bound.reached(false, at(0)));
        assert!(!bound.reached(false, at(600)));
        // The optimizer is seen at 610 s: a minute from there.
        assert!(!bound.reached(true, at(610)));
        assert!(!bound.reached(true, at(669)));
        // A poll that no longer sees it does not restart the clock.
        assert!(!bound.reached(false, at(660)));
        assert!(bound.reached(false, at(670)));

        let mut zero = OptimizerBound::new(Some(0));
        assert!(!zero.reached(false, at(0)));
        assert!(
            zero.reached(true, at(1)),
            "0 stops at the first optimizer poll"
        );

        let mut none = OptimizerBound::new(None);
        assert!(!none.reached(true, at(0)));
        assert!(!none.reached(true, at(86_400)));
    }

    #[test]
    fn optimize_seconds_above_a_day_is_refused() {
        let temp = tempfile::tempdir().unwrap();
        let jar = temp.path().join("freerouting.jar");
        let dsn = temp.path().join("board.dsn");
        let ses = temp.path().join("board.ses");
        std::fs::write(&jar, "").unwrap();
        std::fs::write(&dsn, "").unwrap();
        let settings = |optimize_seconds| RouteSettings {
            max_passes: None,
            optimizer_enabled: None,
            job_timeout_seconds: None,
            optimize_seconds,
            optimizer_max_passes: None,
            optimizer_improvement_threshold: None,
            plane_nets: None,
            plane_as_obstacle: None,
            router_settings: None,
            project_rules: Map::new(),
            poll_interval: Duration::from_secs(3),
            overall_timeout: Duration::from_secs(900),
        };
        for accepted in [None, Some(0), Some(86_400)] {
            validate_inputs(&jar, &dsn, &ses, &settings(accepted)).unwrap();
        }
        let error = validate_inputs(&jar, &dsn, &ses, &settings(Some(86_401)))
            .unwrap_err()
            .to_string();
        assert!(error.contains("optimize_seconds"), "{error}");
    }

    #[test]
    fn optimizer_stage_is_seen_in_the_log_or_the_job_stage() {
        let mut progress = JobProgress::new("a3aa32eb");
        assert!(!progress.is_optimizing());
        progress.activity = Some("pass 18: 0 unrouted, 218 violations".into());
        assert!(!progress.is_optimizing());
        progress.activity = Some("optimizing, 218 violations".into());
        assert!(progress.is_optimizing());
        progress.activity = Some("optimizer pass 2: score 73.70 -> 84.26 (improved)".into());
        assert!(progress.is_optimizing());

        assert!(stage_is_optimizing(&json!({ "stage": "OPTIMIZATION" })));
        assert!(!stage_is_optimizing(&json!({ "stage": "ROUTING" })));
        assert!(!stage_is_optimizing(&json!({})));
    }

    #[test]
    fn the_last_unrouted_count_of_the_job_is_found() {
        let routing = "\
2026-10-29 17:52:54.000 INFO   [A3AA32] Auto-routing pass #8 on board 'a' was completed in 16.00 seconds with score 964.79 (4 unrouted and 218 violations), using 1.00 CPU seconds.
2026-10-29 17:52:55.000 DEBUG  [A3AA32]   Net 'SCK2' has 4 incomplete(s)
2026-10-29 17:52:56.000 INFO   [B00000] Auto-routing pass #2 on board 'b' was completed in 1.00 seconds with score 1.00 (50 unrouted and 3 violations), using 1.00 CPU seconds.
";
        assert_eq!(latest_unrouted(routing, "[A3AA32]"), Some(4));
        let optimizing = format!(
            "{routing}2026-10-29 17:54:26.961 INFO   [A3AA32] Optimization stage started on board 'c'. Baseline router score: 980.71, optimizer score: 71.70, incomplete connections: 0, clearance violations: 218.\n"
        );
        assert_eq!(latest_unrouted(&optimizing, "[A3AA32]"), Some(0));
        assert_eq!(latest_unrouted(routing, "[C11111]"), None);
    }

    #[test]
    fn job_tag_is_the_upper_cased_id_prefix() {
        assert_eq!(
            JobProgress::new("a3aa32eb-5b14-4a2b-8742-6b44c34d63bb").tag,
            "[A3AA32]"
        );
    }

    #[test]
    fn nested_and_text_wrapped_identifiers_are_found() {
        let value = expand_text_content(json!({
            "content": [{ "type": "text", "text": "{\"data\":{\"sessionId\":\"abc\"}}" }]
        }));
        assert_eq!(find_string(&value, &["sessionId"]), Some("abc".to_string()));
    }

    #[test]
    fn duration_settings_use_documented_shape() {
        assert_eq!(duration_string(5 * 60), "00:05:00");
        assert_eq!(duration_string(25 * 60 * 60), "25:00:00");
    }

    #[test]
    fn optimizer_settings_out_of_range_are_refused() {
        let temp = tempfile::tempdir().unwrap();
        let jar = temp.path().join("freerouting.jar");
        let dsn = temp.path().join("board.dsn");
        let ses = temp.path().join("board.ses");
        std::fs::write(&jar, "").unwrap();
        std::fs::write(&dsn, "").unwrap();
        let settings = |passes, threshold| RouteSettings {
            max_passes: None,
            optimizer_enabled: None,
            job_timeout_seconds: None,
            optimize_seconds: None,
            optimizer_max_passes: passes,
            optimizer_improvement_threshold: threshold,
            plane_nets: None,
            plane_as_obstacle: None,
            router_settings: None,
            project_rules: Map::new(),
            poll_interval: Duration::from_secs(3),
            overall_timeout: Duration::from_secs(900),
        };
        for (passes, threshold) in [
            (None, None),
            (Some(1), Some(0.0)),
            (None, Some(0.1)),
            (Some(100), Some(100.0)),
        ] {
            validate_inputs(&jar, &dsn, &ses, &settings(passes, threshold)).unwrap();
        }
        for (passes, threshold, named) in [
            (Some(0), None, "optimizer_max_passes"),
            (Some(101), None, "optimizer_max_passes"),
            (None, Some(-0.1), "optimizer_improvement_threshold"),
            (None, Some(100.1), "optimizer_improvement_threshold"),
            (None, Some(f64::NAN), "optimizer_improvement_threshold"),
            (None, Some(0.05), "as a fraction"),
            (None, Some(0.099), "as a fraction"),
        ] {
            let error = validate_inputs(&jar, &dsn, &ses, &settings(passes, threshold))
                .unwrap_err()
                .to_string();
            assert!(error.contains(named), "{passes:?} {threshold:?}: {error}");
        }
        let with_router_threshold = |named, routed: f64| RouteSettings {
            router_settings: json!({ "optimizer": { "improvement_threshold": routed } })
                .as_object()
                .cloned(),
            ..settings(None, named)
        };
        let error = validate_inputs(&jar, &dsn, &ses, &with_router_threshold(None, 0.05))
            .unwrap_err()
            .to_string();
        assert!(error.contains(ROUTER_SETTINGS_THRESHOLD), "{error}");
        assert!(error.contains("as a fraction"), "{error}");
        for (named, routed) in [(None, 0.0), (None, 0.1), (Some(2.5), 0.05)] {
            validate_inputs(&jar, &dsn, &ses, &with_router_threshold(named, routed)).unwrap();
        }
    }

    fn with_router_settings(router_settings: Value) -> RouteSettings {
        RouteSettings {
            max_passes: None,
            optimizer_enabled: None,
            job_timeout_seconds: None,
            optimize_seconds: None,
            optimizer_max_passes: None,
            optimizer_improvement_threshold: None,
            plane_nets: None,
            plane_as_obstacle: None,
            router_settings: router_settings.as_object().cloned(),
            poll_interval: Duration::from_secs(3),
            overall_timeout: Duration::from_secs(900),
        }
    }

    #[test]
    fn negative_router_settings_are_named_by_path() {
        let settings = with_router_settings(json!({
            "scoring": { "via_costs": -5, "plane_via_costs": 5 },
            "layers": [{ "bend_cost": 0.0 }, { "bend_cost": -0.5 }],
            "plane_nets": ["GND"],
            "fanout": { "min_escape_length_mm": 2.5 },
            "neck_width_um": -0.0
        }));
        let negative: Vec<String> = negative_settings_sent(&settings)
            .iter()
            .map(|(path, value)| format!("{path} is {value}"))
            .collect();
        assert_eq!(
            negative,
            [
                "router_settings.layers[1].bend_cost is -0.5",
                "router_settings.scoring.via_costs is -5"
            ]
        );
    }

    #[test]
    fn negative_router_settings_a_named_argument_replaces_are_not_sent() {
        let settings = RouteSettings {
            max_passes: Some(4),
            optimizer_improvement_threshold: Some(1.0),
            plane_nets: Some(vec!["GND".to_string()]),
            ..with_router_settings(json!({
                "autorouter": { "max_passes": -1, "max_threads": -2 },
                "optimizer": { "improvement_threshold": -3 },
                "plane_nets": [-4]
            }))
        };
        let negative = negative_settings_sent(&settings);
        assert_eq!(
            negative,
            [(
                "router_settings.autorouter.max_threads".to_string(),
                Number::from(-2)
            )]
        );
        // What is reported is what job_settings_body really sends.
        let sent = job_settings_body(&settings);
        assert_eq!(sent["autorouter"]["max_threads"], json!(-2));
        assert_eq!(sent["autorouter"]["max_passes"], json!(4));
        assert_eq!(sent["optimizer"]["improvement_threshold"], json!(1.0));
        assert_eq!(sent["plane_nets"], json!(["GND"]));
    }

    #[test]
    fn negative_router_settings_are_refused_before_java() {
        let temp = tempfile::tempdir().unwrap();
        let jar = temp.path().join("freerouting.jar");
        let dsn = temp.path().join("board.dsn");
        let ses = temp.path().join("board.ses");
        std::fs::write(&jar, "").unwrap();
        std::fs::write(&dsn, "").unwrap();
        let refused = with_router_settings(json!({
            "scoring": { "via_costs": -5 },
            "optimizer": { "improvement_threshold": -1 }
        }));
        let error = validate_inputs(&jar, &dsn, &ses, &refused)
            .unwrap_err()
            .to_string();
        assert!(
            error.contains("router_settings.optimizer.improvement_threshold is -1"),
            "{error}"
        );
        assert!(
            error.contains("router_settings.scoring.via_costs is -5"),
            "{error}"
        );
        assert!(error.contains("must not be negative"), "{error}");
        let accepted = with_router_settings(json!({ "scoring": { "via_costs": 0 } }));
        validate_inputs(&jar, &dsn, &ses, &accepted).unwrap();
    }

    #[test]
    fn router_settings_pass_through_and_named_arguments_win() {
        let settings = RouteSettings {
            max_passes: Some(7),
            optimizer_enabled: None,
            job_timeout_seconds: None,
            optimize_seconds: None,
            optimizer_max_passes: None,
            optimizer_improvement_threshold: None,
            plane_nets: None,
            plane_as_obstacle: None,
            router_settings: Some(
                json!({
                    "autorouter": { "max_passes": 99, "max_threads": 4 },
                    "scoring": { "via_costs": 80 },
                    "hole_clearance_um": 200.0
                })
                .as_object()
                .unwrap()
                .clone(),
            ),
            project_rules: Map::new(),
            poll_interval: Duration::from_secs(3),
            overall_timeout: Duration::from_secs(900),
        };
        assert_eq!(
            Value::Object(job_settings_body(&settings)),
            json!({
                "autorouter": { "max_passes": 7, "max_threads": 4 },
                "scoring": { "via_costs": 80 },
                "hole_clearance_um": 200.0
            })
        );
    }

    #[test]
    fn settings_that_did_not_take_are_reported() {
        let sent = json!({
            "autorouter": { "max_passes": 7, "max_pases": 3 },
            "optimizer": { "improvement_threshold": 1 },
            "hole_clearance_um": 200.0,
            "plane_nets": ["GND"],
            "no_such_setting": true
        });
        // Freerouting 2.5.0 wraps the effective settings like this.
        let result = json!({
            "parsedTextContent": [{
                "status": 200,
                "contentType": "application/json",
                "body": {
                    "autorouter": { "max_passes": 7, "max_threads": 15 },
                    "optimizer": { "improvement_threshold": 1.0 },
                    "hole_clearance_um": 0.0,
                    "plane_nets": ["GND"]
                }
            }]
        });
        let effective = effective_settings(&result).unwrap();
        assert_eq!(
            settings_mismatches(sent.as_object().unwrap(), effective),
            [
                "autorouter.max_pases: not a Freerouting setting",
                "hole_clearance_um: sent 200.0, effective 0.0",
                "no_such_setting: not a Freerouting setting",
            ]
        );

        // Freerouting 2.5.0 fills in what a layer leaves out and keeps the
        // layers nobody sent: a partial layers array that took is no mismatch.
        let effective = json!({
            "layers": [
                { "bend_cost": 0.5, "preferred_direction_horizontal": true },
                { "bend_cost": 0.0, "preferred_direction_horizontal": false }
            ]
        });
        let effective = effective.as_object().unwrap();
        for (sent, expected) in [
            (json!({ "layers": [{ "bend_cost": 0.5 }] }), vec![]),
            (
                json!({ "layers": [{ "bend_cost": 0.5 }, { "bend_cost": 2.0 }] }),
                vec!["layers[1].bend_cost: sent 2.0, effective 0.0"],
            ),
            (
                json!({ "layers": [{ "bend_kost": 1.0 }] }),
                vec!["layers[0].bend_kost: not a Freerouting setting"],
            ),
            (
                json!({ "layers": [{}, {}, { "bend_cost": 1.0 }] }),
                vec![r#"layers[2]: sent {"bend_cost":1.0}, not in the effective settings"#],
            ),
            (
                json!({ "layers": { "bend_cost": 0.5 } }),
                vec![concat!(
                    r#"layers: sent {"bend_cost":0.5}, effective "#,
                    r#"[{"bend_cost":0.5,"preferred_direction_horizontal":true},"#,
                    r#"{"bend_cost":0.0,"preferred_direction_horizontal":false}]"#
                )],
            ),
        ] {
            assert_eq!(
                settings_mismatches(sent.as_object().unwrap(), effective),
                expected,
                "{sent}"
            );
        }
    }

    #[test]
    fn job_settings_use_the_keys_freerouting_reads() {
        let settings = RouteSettings {
            max_passes: Some(7),
            optimizer_enabled: Some(true),
            job_timeout_seconds: Some(300),
            optimize_seconds: None,
            optimizer_max_passes: Some(3),
            optimizer_improvement_threshold: Some(0.5),
            plane_nets: Some(vec!["GND".into(), "+3V3".into()]),
            plane_as_obstacle: Some(false),
            router_settings: None,
            project_rules: Map::new(),
            poll_interval: Duration::from_secs(3),
            overall_timeout: Duration::from_secs(900),
        };
        // The shape get_effective_settings echoed back from Freerouting 2.5.0.
        assert_eq!(
            Value::Object(job_settings_body(&settings)),
            json!({
                "autorouter": { "max_passes": 7 },
                "optimizer": { "enabled": true, "max_passes": 3, "improvement_threshold": 0.5 },
                "plane_nets": ["GND", "+3V3"],
                "plane_as_obstacle": false,
                "job_timeout": "00:05:00"
            })
        );
        let nothing = RouteSettings {
            max_passes: None,
            optimizer_enabled: None,
            job_timeout_seconds: None,
            optimizer_max_passes: None,
            optimizer_improvement_threshold: None,
            plane_nets: None,
            plane_as_obstacle: None,
            router_settings: None,
            ..settings
        };
        assert!(job_settings_body(&nothing).is_empty());
    }

    #[test]
    fn output_path_is_unique_and_stays_beside_destination() {
        let output = Path::new("build/board.ses");
        let first = temporary_output_path(output).unwrap();
        let second = temporary_output_path(output).unwrap();
        assert_eq!(first.parent(), output.parent());
        assert_ne!(first, second);
        assert!(extension_is(&first, "ses"));
        let name = first.file_name().unwrap().to_string_lossy();
        assert!(name.starts_with(".board.ses.copperpour-"), "{name}");
    }

    #[test]
    fn server_command_passes_headless_as_a_jvm_option_before_jar() {
        let command = LocalMcpClient::server_command(Path::new("freerouting.jar"));
        let arguments: Vec<_> = command.as_std().get_args().collect();
        let jar = arguments.iter().position(|a| *a == "-jar").unwrap();
        assert!(arguments[..jar].contains(&std::ffi::OsStr::new("-Djava.awt.headless=true")));
        assert_eq!(arguments[jar + 1], "freerouting.jar");
    }

    #[test]
    fn jvm_crash_reports_stay_out_of_the_project_directory() {
        let state = tempfile::tempdir().unwrap();
        let directory = state.path().join("freerouting");
        let arguments = jvm_crash_arguments(Some(&directory));
        let mut expected = std::ffi::OsString::from("-XX:ErrorFile=");
        expected.push(directory.join("hs_err_pid%p.log"));
        assert_eq!(arguments, [expected, "-XX:-CreateCoredumpOnCrash".into()]);
        assert!(directory.is_dir());

        let blocked = state.path().join("file");
        std::fs::write(&blocked, "").unwrap();
        let fallback = jvm_crash_arguments(Some(&blocked.join("freerouting")));
        let mut expected = std::ffi::OsString::from("-XX:ErrorFile=");
        expected.push(std::env::temp_dir().join("hs_err_pid%p.log"));
        assert_eq!(fallback[0], expected);
    }

    #[test]
    fn jvm_crash_reports_go_to_the_temporary_directory_without_an_absolute_state_directory() {
        let mut expected = std::ffi::OsString::from("-XX:ErrorFile=");
        expected.push(std::env::temp_dir().join("hs_err_pid%p.log"));
        assert_eq!(jvm_crash_arguments(None)[0], expected);

        let relative = Path::new("relative-state-for-crash-reports/freerouting");
        assert_eq!(jvm_crash_arguments(Some(relative))[0], expected);
        assert!(!Path::new("relative-state-for-crash-reports").exists());
    }

    #[test]
    fn server_command_passes_the_crash_report_options_before_jar() {
        let command = LocalMcpClient::server_command(Path::new("freerouting.jar"));
        let arguments: Vec<_> = command.as_std().get_args().collect();
        let jar = arguments.iter().position(|a| *a == "-jar").unwrap();
        let error_file = arguments
            .iter()
            .position(|a| a.to_string_lossy().starts_with("-XX:ErrorFile="))
            .expect("server command sets -XX:ErrorFile");
        assert!(error_file < jar);
        assert!(arguments[..jar].contains(&std::ffi::OsStr::new("-XX:-CreateCoredumpOnCrash")));
        let report = arguments[error_file].to_string_lossy();
        assert!(Path::new(&report["-XX:ErrorFile=".len()..]).is_absolute());
    }

    #[test]
    fn unauthenticated_servers_are_forced_to_loopback() {
        let arguments = server_arguments();
        assert!(arguments.contains(&"--api_server.authentication.enabled=false"));
        assert!(arguments.contains(&"--api_server-endpoints=http://127.0.0.1:37864"));
        assert!(arguments.contains(&"--mcp_server.authentication.enabled=false"));
        assert!(arguments.contains(&"--mcp_server-endpoints=http://127.0.0.1:37964"));
        assert!(!arguments
            .iter()
            .any(|argument| argument.contains("0.0.0.0")));
    }

    #[test]
    fn required_tool_schema_drift_is_refused_before_routing() {
        let object = |properties: Value, required: Value| json!({ "type": "object", "properties": properties, "required": required });
        let mut tools = BTreeMap::new();
        tools.insert("enqueue_job".into(), object(json!({"body": object(json!({"session_id": {"type":"string"}}), json!(["session_id"]))}), json!(["body"])));
        tools.insert(
            "upload_job_input_from_local_file".into(),
            object(
                json!({"jobId":{"type":"string"},"filePath":{"type":"string"}}),
                json!(["jobId", "filePath"]),
            ),
        );
        tools.insert(
            "start_job".into(),
            object(
                json!({"path": object(json!({"jobId":{"type":"string"}}), json!(["jobId"]))}),
                json!(["path"]),
            ),
        );
        tools.insert("get_job_details".into(), tools["start_job"].clone());
        tools.insert(
            "download_job_output_to_local_file".into(),
            tools["upload_job_input_from_local_file"].clone(),
        );
        validate_tool_contracts(&tools).unwrap();

        tools.get_mut("start_job").unwrap()["properties"]["path"]["required"] = json!([]);
        let error = validate_tool_contracts(&tools).unwrap_err().to_string();
        assert!(
            error.contains("start_job") && error.contains("path.jobId"),
            "{error}"
        );
    }

    #[tokio::test]
    async fn stderr_diagnostics_are_preserved_beside_the_requested_output() {
        let temp = tempfile::tempdir().unwrap();
        let output = temp.path().join("board.ses");
        let path = write_diagnostics(&output, "router failed at pass 3")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            std::fs::read_to_string(path).unwrap(),
            "router failed at pass 3"
        );
    }

    #[test]
    fn stderr_in_errors_is_cut_to_its_last_lines() {
        let long: String = (1..=40)
            .map(|n| format!("2026-10-29 INFO line {n}\n"))
            .collect();
        let tail = stderr_tail(&long);
        assert!(tail.starts_with("2026-10-29 INFO line 29"), "{tail}");
        assert!(tail.ends_with("line 40"), "{tail}");
        let wide = "é".repeat(5000);
        let cut = stderr_tail(&wide);
        assert_eq!(cut.chars().count(), 2003);
        assert!(cut.starts_with("..."), "{cut}");
        assert_eq!(stderr_tail("short"), "short");
    }

    /// The cause comes first, the banner's start is left out, and the file
    /// holding all of stderr is named.
    #[test]
    fn an_error_puts_its_cause_before_the_end_of_stderr() {
        let stderr: String = (1..=40)
            .map(|n| format!("banner line {n}\n"))
            .chain(["java.lang.OutOfMemoryError: Java heap space\n".to_string()])
            .collect();
        let error = with_stderr_tail(
            anyhow::anyhow!("job ended").context("Freerouting job abc ended in state FAILED"),
            &stderr,
            Some("/work/board.freerouting.log"),
        );
        let message = format!("{error:#}");
        assert!(
            message.starts_with("Freerouting job abc ended in state FAILED: job ended\n"),
            "{message}"
        );
        assert!(
            message.contains("all of it in /work/board.freerouting.log"),
            "{message}"
        );
        assert!(
            message.ends_with("java.lang.OutOfMemoryError: Java heap space"),
            "{message}"
        );
        assert!(!message.contains("banner line 29\n"), "{message}");
        assert!(message.contains("banner line 30"), "{message}");

        let bare = with_stderr_tail(anyhow::anyhow!("no answer"), "", None);
        assert_eq!(format!("{bare:#}"), "no answer");
    }

    /// Runs `run` with `JAVA_HOME` pointing at a fake `java` that writes 40
    /// banner lines and a last line to stderr, then runs `server` (sh).
    #[cfg(unix)]
    fn with_fake_java<T>(
        temp: &Path,
        server: &str,
        run: impl std::future::Future<Output = T>,
    ) -> T {
        use std::os::unix::fs::PermissionsExt;
        let bin = temp.join("jdk").join("bin");
        std::fs::create_dir_all(&bin).unwrap();
        let java = bin.join("java");
        std::fs::write(
            &java,
            format!(
                "#!/bin/sh\n\
                 i=1; while [ $i -le 40 ]; do echo \"banner line $i\" >&2; i=$((i+1)); done\n\
                 echo 'last stderr line' >&2\n\
                 {server}\n"
            ),
        )
        .unwrap();
        std::fs::set_permissions(&java, std::fs::Permissions::from_mode(0o755)).unwrap();
        let _env = crate::java_runtime::JAVA_ENV_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let previous = std::env::var_os("JAVA_HOME");
        std::env::set_var("JAVA_HOME", temp.join("jdk"));
        let result = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
            .block_on(run);
        match previous {
            Some(value) => std::env::set_var("JAVA_HOME", value),
            None => std::env::remove_var("JAVA_HOME"),
        }
        result
    }

    /// The error of a job that failed starts with its cause and ends with the
    /// last lines of stderr, and names the log that holds all of it.
    #[cfg(unix)]
    #[test]
    fn a_failed_job_reports_its_cause_before_the_end_of_stderr() {
        let temp = tempfile::tempdir().unwrap();
        let jar = temp.path().join("freerouting.jar");
        let dsn = temp.path().join("board.dsn");
        let ses = temp.path().join("board.ses");
        std::fs::write(&jar, "not a jar").unwrap();
        std::fs::write(&dsn, "(pcb board)").unwrap();
        let tools = conforming_tools_list();
        let server = format!(
            r#"while read -r line; do
  id=$(printf '%s' "$line" | sed -n 's/.*"id":\([0-9]*\).*/\1/p')
  [ -n "$id" ] || continue
  case "$line" in
    *'"initialize"'*) result='{{"protocolVersion":"2025-06-18"}}' ;;
    *'"tools/list"'*) result='{tools}' ;;
    *create_session*) result='{{"sessionId":"s1"}}' ;;
    *enqueue_job*) result='{{"jobId":"job123"}}' ;;
    *get_job_details*) result='{{"state":"FAILED"}}' ;;
    *) result='{{}}' ;;
  esac
  printf '{{"jsonrpc":"2.0","id":%s,"result":%s}}\n' "$id" "$result"
done"#
        );
        let settings = RouteSettings {
            max_passes: None,
            optimizer_enabled: None,
            job_timeout_seconds: None,
            optimize_seconds: None,
            optimizer_max_passes: None,
            optimizer_improvement_threshold: None,
            plane_nets: None,
            plane_as_obstacle: None,
            router_settings: None,
            // The least route_local accepts; FAILED comes on the first poll.
            poll_interval: Duration::from_secs(2),
            overall_timeout: Duration::from_secs(30),
        };
        let error = with_fake_java(
            temp.path(),
            &server,
            route_local(&jar, &dsn, &ses, &settings),
        )
        .expect_err("a FAILED job is an error");

        let message = format!("{error:#}");
        let log = diagnostics_path(&ses);
        assert!(
            message.starts_with("Freerouting job job123 ended in state FAILED\n"),
            "{message}"
        );
        assert!(
            message.contains(&format!("all of it in {}", log.display())),
            "{message}"
        );
        assert!(message.ends_with("last stderr line"), "{message}");
        assert!(message.contains("banner line 30\n"), "{message}");
        assert!(!message.contains("banner line 29\n"), "{message}");
        let full = std::fs::read_to_string(&log).unwrap();
        assert!(full.contains("banner line 1\n"), "{full}");
    }

    /// A server that dies before answering `initialize`: the cause first,
    /// then the end of stderr.
    #[cfg(unix)]
    #[test]
    fn a_server_that_does_not_start_reports_its_cause_before_the_end_of_stderr() {
        let temp = tempfile::tempdir().unwrap();
        let jar = temp.path().join("freerouting.jar");
        std::fs::write(&jar, "not a jar").unwrap();
        let probe = with_fake_java(temp.path(), "exit 1", probe_local(&jar));

        let message = probe.error.expect("the start failed");
        assert!(!message.starts_with("Freerouting stderr"), "{message}");
        assert!(
            message.contains("\nFreerouting stderr, last lines:\n"),
            "{message}"
        );
        assert!(message.ends_with("last stderr line"), "{message}");
        assert!(!message.contains("banner line 29\n"), "{message}");
    }

    /// Optional local parity check for Freerouting's actual stdio MCP state
    /// machine. CI does not install Java or Freerouting.
    #[tokio::test]
    #[ignore = "requires Java and FREEROUTING_JAR"]
    async fn native_mcp_routes_the_export_fixture() {
        let jar = PathBuf::from(std::env::var_os("FREEROUTING_JAR").expect("set FREEROUTING_JAR"));
        let source = include_str!("../../../tests/fixtures/specctra_two_resistors.kicad_pcb");
        let temp = tempfile::tempdir().unwrap();
        let board = temp.path().join("board.kicad_pcb");
        let dsn = temp.path().join("board.dsn");
        let ses = temp.path().join("board.ses");
        std::fs::write(&board, source).unwrap();
        let export = crate::specctra::export_dsn(&board, source, &routing_rules()).unwrap();
        std::fs::write(&dsn, export.dsn).unwrap();

        let evidence = route_local(
            &jar,
            &dsn,
            &ses,
            &RouteSettings {
                max_passes: Some(2),
                optimizer_enabled: Some(false),
                job_timeout_seconds: Some(300),
                optimize_seconds: None,
                optimizer_max_passes: None,
                optimizer_improvement_threshold: None,
                plane_nets: None,
                plane_as_obstacle: None,
                router_settings: None,
                project_rules: Map::new(),
                poll_interval: Duration::from_secs(2),
                overall_timeout: Duration::from_secs(300),
            },
        )
        .await
        .unwrap();
        assert_eq!(evidence.final_state, "COMPLETED");
        assert!(evidence.ses_bytes > 0);
        assert!(ses.is_file());
    }

    #[tokio::test]
    #[ignore = "requires Java and FREEROUTING_JAR"]
    async fn overall_timeout_closes_child_and_removes_partial_output() {
        let jar = PathBuf::from(std::env::var_os("FREEROUTING_JAR").expect("set FREEROUTING_JAR"));
        let source = include_str!("../../../tests/fixtures/specctra_two_resistors.kicad_pcb");
        let temp = tempfile::tempdir().unwrap();
        let board = temp.path().join("board.kicad_pcb");
        let dsn = temp.path().join("board.dsn");
        let ses = temp.path().join("board.ses");
        std::fs::write(&board, source).unwrap();
        let export = crate::specctra::export_dsn(&board, source, &routing_rules()).unwrap();
        std::fs::write(&dsn, export.dsn).unwrap();

        let error = format!(
            "{:#}",
            route_local(
                &jar,
                &dsn,
                &ses,
                &RouteSettings {
                    max_passes: Some(100),
                    optimizer_enabled: Some(true),
                    job_timeout_seconds: Some(300),
                    optimize_seconds: None,
                    optimizer_max_passes: None,
                    optimizer_improvement_threshold: None,
                    plane_nets: None,
                    plane_as_obstacle: None,
                    router_settings: None,
                    project_rules: Map::new(),
                    poll_interval: Duration::from_secs(5),
                    overall_timeout: Duration::from_millis(100),
                },
            )
            .await
            .unwrap_err()
        );
        assert!(error.contains("overall timeout"), "{error}");
        let pid = LAST_CHILD_PID.load(Ordering::SeqCst);
        assert!(
            wait_for_process_exit(pid).await,
            "Freerouting child {pid} survived timeout"
        );
        assert!(!ses.exists());
        assert!(
            std::fs::read_dir(temp.path()).unwrap().all(|entry| !entry
                .unwrap()
                .file_name()
                .to_string_lossy()
                .contains(".tmp.ses")),
            "timeout left a temporary SES"
        );
    }

    #[tokio::test]
    #[ignore = "requires Java and FREEROUTING_JAR"]
    async fn cancelling_route_reaps_the_owned_child() {
        let jar = PathBuf::from(std::env::var_os("FREEROUTING_JAR").expect("set FREEROUTING_JAR"));
        let source = include_str!("../../../tests/fixtures/specctra_two_resistors.kicad_pcb");
        let temp = tempfile::tempdir().unwrap();
        let board = temp.path().join("board.kicad_pcb");
        let dsn = temp.path().join("board.dsn");
        let ses = temp.path().join("board.ses");
        std::fs::write(&board, source).unwrap();
        let export = crate::specctra::export_dsn(&board, source, &routing_rules()).unwrap();
        std::fs::write(&dsn, export.dsn).unwrap();

        LAST_CHILD_PID.store(0, Ordering::SeqCst);
        let task = tokio::spawn(async move {
            route_local(
                &jar,
                &dsn,
                &ses,
                &RouteSettings {
                    max_passes: Some(100),
                    optimizer_enabled: Some(true),
                    job_timeout_seconds: Some(300),
                    optimize_seconds: None,
                    optimizer_max_passes: None,
                    optimizer_improvement_threshold: None,
                    plane_nets: None,
                    plane_as_obstacle: None,
                    router_settings: None,
                    project_rules: Map::new(),
                    poll_interval: Duration::from_secs(5),
                    overall_timeout: Duration::from_secs(300),
                },
            )
            .await
        });
        let pid = loop {
            let pid = LAST_CHILD_PID.load(Ordering::SeqCst);
            if pid != 0 {
                break pid;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        };
        task.abort();
        let _ = task.await;
        assert!(
            wait_for_process_exit(pid).await,
            "Freerouting child {pid} survived cancellation"
        );
    }

    #[test]
    fn existing_diagnostics_are_refused_before_routing() {
        let temp = tempfile::tempdir().unwrap();
        let jar = temp.path().join("freerouting.jar");
        let dsn = temp.path().join("board.dsn");
        let ses = temp.path().join("board.ses");
        std::fs::write(&jar, "").unwrap();
        std::fs::write(&dsn, "").unwrap();
        let settings = RouteSettings {
            max_passes: None,
            optimizer_enabled: None,
            job_timeout_seconds: None,
            optimize_seconds: None,
            optimizer_max_passes: None,
            optimizer_improvement_threshold: None,
            plane_nets: None,
            plane_as_obstacle: None,
            router_settings: None,
            project_rules: Map::new(),
            poll_interval: Duration::from_secs(3),
            overall_timeout: Duration::from_secs(900),
        };
        validate_inputs(&jar, &dsn, &ses, &settings).unwrap();

        std::fs::write(diagnostics_path(&ses), "previous run").unwrap();
        let error = validate_inputs(&jar, &dsn, &ses, &settings)
            .unwrap_err()
            .to_string();
        assert!(
            error.contains("diagnostics output already exists"),
            "{error}"
        );
    }
}
