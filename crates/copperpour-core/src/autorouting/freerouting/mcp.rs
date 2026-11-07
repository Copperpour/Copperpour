//! Local Freerouting MCP orchestration.
//!
//! Copperpour deliberately does not reimplement the router. It starts the local
//! Freerouting JAR in its documented headless stdio MCP mode, follows the
//! server's routing state machine, and returns only compact job evidence.

use anyhow::{bail, Context, Result};
use serde::Serialize;
use serde_json::{json, Map, Value};
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

    let result = match route_result {
        Ok(evidence) => Ok(evidence),
        Err(error) => {
            if diagnostics.is_empty() {
                Err(error)
            } else {
                Err(error).context(format!("Freerouting stderr: {diagnostics}"))
            }
        }
    };
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
    Ok(())
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
    if !job_settings.is_empty() {
        if !tools.contains_key("update_job_settings") {
            bail!("Freerouting MCP does not expose update_job_settings");
        }
        client
            .call_tool(
                "update_job_settings",
                json!({ "path": { "jobId": job_id }, "body": job_settings }),
            )
            .await?;
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
        last_activity: progress.activity,
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
}

/// Freerouting's log file: `FREEROUTING_LOG`, else its default under the XDG
/// state directory (Linux).
fn freerouting_log_path() -> Option<PathBuf> {
    if let Some(path) = std::env::var_os("FREEROUTING_LOG") {
        return Some(PathBuf::from(path));
    }
    let state = std::env::var_os("XDG_STATE_HOME")
        .map(PathBuf::from)
        .or_else(|| {
            std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".local/state"))
        })?;
    Some(state.join("freerouting/logs/freerouting.log"))
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
            return if diagnostics.is_empty() {
                Err(error)
            } else {
                Err(error).context(format!("Freerouting stderr: {diagnostics}"))
            };
        }
        Ok(client)
    }

    fn server_command(jar: &Path) -> Command {
        let mut command = Command::new(crate::java_runtime::java_executable());
        command
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
fn job_settings_body(settings: &RouteSettings) -> Map<String, Value> {
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
        assert_eq!(arguments[0], "-Djava.awt.headless=true");
        assert_eq!(arguments[1], "-jar");
        assert_eq!(arguments[2], "freerouting.jar");
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
