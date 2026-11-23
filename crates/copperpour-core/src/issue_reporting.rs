//! Agent issue reporting and opt-in local crash/error capture to Sentry (#824).
//!
//! Automatic sending is off by default: panics, `handler_error`s, and
//! `mutation_outcome_uncertain` results are scrubbed first, stored locally in
//! the Copperpour state directory (`0600` on Unix), and surfaced to the user in
//! the same or next session so they can choose to send, discard, always send,
//! or never ask.

use serde::{Deserialize, Serialize};
#[cfg(feature = "issue-reporting")]
use std::collections::{BTreeMap, HashSet};
#[cfg(feature = "issue-reporting")]
use std::path::{Path, PathBuf};
#[cfg(feature = "issue-reporting")]
use std::sync::atomic::{AtomicBool, Ordering};
#[cfg(feature = "issue-reporting")]
use std::sync::{Arc, Mutex, OnceLock, RwLock};

/// Compiled-in public write-only DSN for the `copperpour` Sentry project.
#[cfg(feature = "issue-reporting")]
pub const DEFAULT_SENTRY_DSN: &str =
    "https://aa4df6e19578aab601121d1c96e57bec@o4512480290734080.ingest.de.sentry.io/4512480357974096";

/// Valid `report_issue` categories (also enforced by the JSON schema gate).
#[cfg(feature = "issue-reporting")]
pub const VALID_CATEGORIES: &[&str] = &[
    "wrong_result",
    "bad_refusal",
    "crash",
    "missing_tool",
    "missing_argument",
    "docs_or_guidance",
    "other",
];

#[cfg(feature = "issue-reporting")]
pub const MAX_PENDING_REPORTS: usize = 20;

#[cfg(feature = "issue-reporting")]
pub const MAX_PENDING_AGE_MS: u64 = 30 * 24 * 60 * 60 * 1000;

fn default_true() -> bool {
    true
}

/// Configuration for Sentry issue reporting (`[issue_reporting]` in `config.toml`).
///
/// Always compiled even when the `issue-reporting` Cargo feature is disabled so
/// configuration files remain portable between default and offline builds.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct IssueReportingConfig {
    /// Master switch for issue reporting (default: `true`).
    #[serde(default = "default_true")]
    pub enabled: bool,
    /// Store scrubbed crash and error reports locally and ask before sending (default: `true`).
    #[serde(default = "default_true")]
    pub capture: bool,
    /// Send captured crash and error reports automatically without asking (default: `false`).
    #[serde(default)]
    pub automatic: bool,
}

impl Default for IssueReportingConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            capture: true,
            automatic: false,
        }
    }
}

impl IssueReportingConfig {
    /// Apply process environment overrides (`COPPERPOUR_ISSUE_REPORTING=0` and
    /// `DO_NOT_TRACK=1`).
    pub fn effective(&self) -> Self {
        self.effective_with(env_issue_reporting_disabled(), env_do_not_track())
    }

    pub fn effective_with(&self, env_disabled: bool, do_not_track: bool) -> Self {
        let mut effective = self.clone();
        if env_disabled {
            effective.enabled = false;
            effective.capture = false;
            effective.automatic = false;
        }
        if do_not_track {
            effective.automatic = false;
        }
        if !effective.enabled {
            effective.capture = false;
            effective.automatic = false;
        }
        effective
    }
}

/// Whether `COPPERPOUR_ISSUE_REPORTING` disables issue reporting.
pub fn env_issue_reporting_disabled() -> bool {
    std::env::var("COPPERPOUR_ISSUE_REPORTING")
        .ok()
        .is_some_and(|val| {
            matches!(
                val.trim().to_ascii_lowercase().as_str(),
                "0" | "false" | "no" | "off"
            )
        })
}

/// Whether `DO_NOT_TRACK=1` is set in the environment.
pub fn env_do_not_track() -> bool {
    std::env::var("DO_NOT_TRACK")
        .ok()
        .is_some_and(|val| matches!(val.trim().to_ascii_lowercase().as_str(), "1" | "true"))
}

// ─── Full implementation (feature = "issue-reporting") ───────────────────────

#[cfg(feature = "issue-reporting")]
mod imp {
    use super::*;
    use crate::observability::{unix_ms, CallRecord};
    use regex::Regex;
    use serde_json::{json, Value};
    use std::io::Write;
    use std::str::FromStr;

    static GLOBAL_REPORTER: OnceLock<RwLock<Option<Arc<IssueReporter>>>> = OnceLock::new();
    static PANIC_HOOK_INSTALLED: OnceLock<()> = OnceLock::new();

    thread_local! {
        static PANIC_CAPTURED_BY_HOOK: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    }

    fn global_slot() -> &'static RwLock<Option<Arc<IssueReporter>>> {
        GLOBAL_REPORTER.get_or_init(|| RwLock::new(None))
    }

    /// Register the process-wide reporter used by the panic hook and FFI
    /// `catch_unwind` boundaries.
    pub fn register_global_reporter(reporter: Arc<IssueReporter>) {
        if let Ok(mut guard) = global_slot().write() {
            *guard = Some(reporter);
        }
    }

    /// Best-effort bounded flush of any already-initialized Sentry client on
    /// clean process shutdown (does nothing if no event was ever sent).
    pub fn flush_global_reporter(timeout: std::time::Duration) {
        if let Some(reporter) = current_global_reporter() {
            reporter.flush_if_initialized(timeout);
        }
    }

    /// Install the panic hook once per process. Chains to the existing hook
    /// after capturing a scrubbed crash report.
    pub fn install_panic_hook() {
        PANIC_HOOK_INSTALLED.get_or_init(|| {
            let previous = std::panic::take_hook();
            std::panic::set_hook(Box::new(move |info| {
                let message = panic_payload_message(info.payload());
                let location = info
                    .location()
                    .map(|loc| format!("{}:{}:{}", loc.file(), loc.line(), loc.column()));
                if let Some(reporter) = current_global_reporter() {
                    PANIC_CAPTURED_BY_HOOK.with(|flag| flag.set(true));
                    let _ = reporter.capture_panic(&message, location.as_deref());
                }
                previous(info);
            }));
        });
    }

    /// Record a panic caught by an explicit `catch_unwind` boundary (for
    /// example `ffi.rs`), only if the process panic hook did not already
    /// capture it on this thread.
    pub fn capture_caught_panic(context: &str, payload: &(dyn std::any::Any + Send)) {
        let already_captured = PANIC_CAPTURED_BY_HOOK.with(|flag| flag.replace(false));
        if already_captured {
            return;
        }
        if let Some(reporter) = current_global_reporter() {
            let message = panic_payload_message(payload);
            let _ = reporter.capture_panic(&format!("{context}: {message}"), None);
        }
    }

    fn current_global_reporter() -> Option<Arc<IssueReporter>> {
        global_slot().read().ok().and_then(|g| g.clone())
    }

    fn panic_payload_message(payload: &(dyn std::any::Any + Send)) -> String {
        if let Some(s) = payload.downcast_ref::<&str>() {
            (*s).to_string()
        } else if let Some(s) = payload.downcast_ref::<String>() {
            s.clone()
        } else {
            "non-string panic payload".to_string()
        }
    }

    // ─── Privacy scrubbing ───────────────────────────────────────────────────

    /// Machine- and project-specific strings to scrub before any report is
    /// written to disk or sent to Sentry.
    #[derive(Debug, Clone, Default)]
    pub struct ScrubContext {
        pub home_dir: Option<String>,
        pub project_dir: Option<String>,
        pub username: Option<String>,
        pub hostname: Option<String>,
    }

    impl ScrubContext {
        pub fn from_environment(project_dir: Option<&Path>) -> Self {
            let home_dir = crate::app_dirs::home_dir().map(|p| p.display().to_string());
            let project_dir = project_dir.map(|p| p.display().to_string());
            let username = std::env::var("USER")
                .or_else(|_| std::env::var("USERNAME"))
                .ok()
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty());
            let hostname = detect_hostname();
            Self {
                home_dir,
                project_dir,
                username,
                hostname,
            }
        }
    }

    fn detect_hostname() -> Option<String> {
        if let Ok(h) = std::env::var("HOSTNAME")
            .or_else(|_| std::env::var("HOST"))
            .or_else(|_| std::env::var("COMPUTERNAME"))
        {
            let trimmed = h.trim().to_string();
            if !trimmed.is_empty() {
                return Some(trimmed);
            }
        }
        #[cfg(target_os = "linux")]
        if let Ok(h) = std::fs::read_to_string("/etc/hostname") {
            let trimmed = h.trim().to_string();
            if !trimmed.is_empty() {
                return Some(trimmed);
            }
        }
        None
    }

    /// Scrub paths, home/project roots, usernames, hostnames, and IP addresses
    /// from any text before writing to disk or sending to Sentry.
    /// A stable 64-bit FNV-1a fingerprint of a captured message, so repeats
    /// deduplicate and can be matched without the message leaving the machine.
    pub fn message_fingerprint(message: &str) -> String {
        let hash = message
            .bytes()
            .fold(0xcbf2_9ce4_8422_2325_u64, |hash, byte| {
                (hash ^ u64::from(byte)).wrapping_mul(0x0100_0000_01b3)
            });
        format!("{hash:016x}")
    }

    pub fn scrub_text(input: &str, ctx: &ScrubContext) -> String {
        static UNIX_ABS_PATH: OnceLock<Regex> = OnceLock::new();
        static WIN_ABS_PATH: OnceLock<Regex> = OnceLock::new();
        static UNC_PATH: OnceLock<Regex> = OnceLock::new();
        static IPV4_RE: OnceLock<Regex> = OnceLock::new();
        static IPV6_CANDIDATE_RE: OnceLock<Regex> = OnceLock::new();

        let unix_abs = UNIX_ABS_PATH.get_or_init(|| {
            Regex::new(r#"(?:^|(?P<pre>[\s"'`(\[{=:,;]))(?P<path>/(?:[^\s"'`)\]},;:<>]+/)*[^\s"'`)\]},;:<>]+)"#)
                .expect("valid unix path regex")
        });
        let win_abs = WIN_ABS_PATH.get_or_init(|| {
            Regex::new(r#"[A-Za-z]:\\(?:[^\s"'`)\]},;:<>]+\\)*[^\s"'`)\]},;:<>]*"#)
                .expect("valid windows path regex")
        });
        let unc = UNC_PATH.get_or_init(|| {
            Regex::new(r#"\\\\[^\s\\"'`)\]},;:<>]+\\[^\s"'`)\]},;:<>]+"#)
                .expect("valid unc path regex")
        });
        let ipv4 = IPV4_RE
            .get_or_init(|| Regex::new(r"\b(?:\d{1,3}\.){3}\d{1,3}\b").expect("valid ipv4 regex"));
        let ipv6_candidate = IPV6_CANDIDATE_RE.get_or_init(|| {
            Regex::new(r"[0-9a-fA-F:]{2,39}(?:\.\d{1,3}){0,3}").expect("valid ipv6 candidate regex")
        });

        let mut out = input.to_string();

        // 1. Replace project root (and drop trailing project-specific components).
        if let Some(proj) = ctx.project_dir.as_deref().filter(|s| !s.is_empty()) {
            out = replace_root_path(&out, proj, "<project>", false);
        }

        // 2. Replace home directory (preserve only well-known Copperpour paths
        //    directly under home, drop other path components).
        if let Some(home) = ctx.home_dir.as_deref().filter(|s| !s.is_empty()) {
            out = replace_root_path(&out, home, "~", true);
        }

        // 3. Scrub remaining Windows UNC and drive paths.
        out = unc
            .replace_all(&out, |caps: &regex::Captures| scrub_single_path(&caps[0]))
            .into_owned();
        out = win_abs
            .replace_all(&out, |caps: &regex::Captures| scrub_single_path(&caps[0]))
            .into_owned();

        // 4. Scrub remaining Unix absolute paths (preserving Copperpour crate
        //    source paths and ignoring URLs like `https://...`).
        out = unix_abs
            .replace_all(&out, |caps: &regex::Captures| {
                let pre = caps.name("pre").map(|m| m.as_str()).unwrap_or("");
                let path = &caps["path"];
                if path.starts_with("//") {
                    return format!("{pre}{path}");
                }
                let replaced = scrub_single_path(path);
                format!("{pre}{replaced}")
            })
            .into_owned();

        // 5. Scrub IPv6 (validated with `std::net::Ipv6Addr` so compressed
        //    addresses like `::1` and `fe80::1` match while `12:34:56` does not),
        //    then IPv4.
        out = ipv6_candidate
            .replace_all(&out, |caps: &regex::Captures| {
                let tok = &caps[0];
                let (addr, suffix) = if tok.ends_with(':') && !tok.ends_with("::") {
                    (&tok[..tok.len() - 1], ":")
                } else {
                    (tok, "")
                };
                if addr.contains(':') && addr.parse::<std::net::Ipv6Addr>().is_ok() {
                    format!("<ip>{suffix}")
                } else {
                    tok.to_string()
                }
            })
            .into_owned();
        out = ipv4.replace_all(&out, "<ip>").into_owned();

        // 6. Scrub hostname and username on word boundaries so short tokens
        //    do not corrupt unrelated words.
        if let Some(host) = ctx.hostname.as_deref().filter(|s| s.len() >= 2) {
            out = replace_word_case_insensitive(&out, host, "<host>");
        }
        if let Some(user) = ctx.username.as_deref().filter(|s| s.len() >= 2) {
            out = replace_word_case_insensitive(&out, user, "<user>");
        }

        out
    }

    fn replace_root_path(
        text: &str,
        root: &str,
        token: &str,
        keep_copperpour_subpath: bool,
    ) -> String {
        let trimmed = root.trim_end_matches(['/', '\\']);
        if trimmed.is_empty() {
            return text.to_string();
        }
        let mut variants = vec![trimmed.to_string()];
        let fwd = trimmed.replace('\\', "/");
        if fwd != trimmed {
            variants.push(fwd);
        }
        let mut out = text.to_string();
        for variant in variants {
            let escaped = regex::escape(&variant);
            let pattern = format!(r#"{escaped}(?:[/\\][^\s"'`)\]}},;:<>]*)?"#);
            if let Ok(re) = Regex::new(&pattern) {
                out = re
                    .replace_all(&out, |caps: &regex::Captures| {
                        let matched = &caps[0];
                        if let Some(own) = extract_copperpour_own_path(matched) {
                            return own;
                        }
                        let rest = &matched[variant.len()..];
                        if keep_copperpour_subpath {
                            let rest_fwd = rest.replace('\\', "/");
                            if rest_fwd == "/.copperpour/logs/calls.jsonl"
                                || rest_fwd == "/.config/copperpour/config.toml"
                                || rest_fwd == "/.copperpour/config.toml"
                                || rest_fwd == "/.copperpour"
                            {
                                return format!("{token}{rest_fwd}");
                            }
                        }
                        token.to_string()
                    })
                    .into_owned();
            }
        }
        out
    }

    fn scrub_single_path(path: &str) -> String {
        if let Some(own) = extract_copperpour_own_path(path) {
            return own;
        }
        "<path>".to_string()
    }

    fn extract_copperpour_own_path(path: &str) -> Option<String> {
        let fwd = path.replace('\\', "/");
        if let Some(idx) = fwd.find("/crates/copperpour") {
            return Some(fwd[idx + 1..].to_string());
        }
        if fwd.starts_with("crates/copperpour") {
            return Some(fwd);
        }
        for file in [
            "/plugin/__init__.py",
            "/plugin/settings_dialog.py",
            "/plugin/native_bridge.py",
        ] {
            if let Some(idx) = fwd.find(file) {
                return Some(fwd[idx + 1..].to_string());
            }
        }
        None
    }

    fn replace_word_case_insensitive(text: &str, needle: &str, replacement: &str) -> String {
        let pattern = format!(r"(?i)\b{}\b", regex::escape(needle));
        match Regex::new(&pattern) {
            Ok(re) => re.replace_all(text, replacement).into_owned(),
            Err(_) => text.to_string(),
        }
    }

    // ─── Report payload & pending store ──────────────────────────────────────

    /// Minimal `CallRecord` projection attached to reports. Never includes
    /// arguments, results, or byte sizes.
    #[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
    pub struct SanitizedCallRecord {
        pub tool: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        pub toolset: Option<String>,
        pub status: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        pub error_kind: Option<String>,
        pub dur_ms: u64,
    }

    impl From<&CallRecord> for SanitizedCallRecord {
        fn from(rec: &CallRecord) -> Self {
            Self {
                tool: rec.tool.clone(),
                toolset: rec.toolset.clone(),
                status: rec.status.as_str().to_string(),
                error_kind: rec.error_kind.clone(),
                dur_ms: rec.dur_ms,
            }
        }
    }

    /// Scrubbed report payload — identical to what is stored on disk for
    /// pending reports and what is sent to Sentry.
    #[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
    pub struct IssueReportPayload {
        pub category: String,
        pub report_type: String,
        pub summary: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        pub tool: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        pub argument: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        pub capability: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        pub expected: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        pub actual: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        pub error_kind: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        pub workaround: Option<String>,
        pub source: String,
        pub version: String,
        pub features: Vec<String>,
        pub os: String,
        pub arch: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        pub kicad_version: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        pub kicad_cli_version: Option<String>,
        pub transport: String,
        pub recent_calls: Vec<SanitizedCallRecord>,
        pub occurrences: u32,
    }

    impl IssueReportPayload {
        /// Apply privacy scrubbing to every text field in place.
        pub fn scrub(&mut self, ctx: &ScrubContext) {
            self.summary = scrub_text(&self.summary, ctx);
            self.tool = self.tool.as_deref().map(|s| scrub_text(s, ctx));
            self.argument = self.argument.as_deref().map(|s| scrub_text(s, ctx));
            self.capability = self.capability.as_deref().map(|s| scrub_text(s, ctx));
            self.expected = self.expected.as_deref().map(|s| scrub_text(s, ctx));
            self.actual = self.actual.as_deref().map(|s| scrub_text(s, ctx));
            self.error_kind = self.error_kind.as_deref().map(|s| scrub_text(s, ctx));
            self.workaround = self.workaround.as_deref().map(|s| scrub_text(s, ctx));
            self.kicad_version = self.kicad_version.as_deref().map(|s| scrub_text(s, ctx));
            self.kicad_cli_version = self
                .kicad_cli_version
                .as_deref()
                .map(|s| scrub_text(s, ctx));
        }
    }

    /// A locally stored, already-scrubbed pending report awaiting user choice.
    #[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
    pub struct PendingReport {
        pub id: String,
        pub signature: String,
        pub count: u32,
        pub created_at_ms: u64,
        pub updated_at_ms: u64,
        pub summary: String,
        pub payload: IssueReportPayload,
    }

    /// Compile-time list of enabled Copperpour Cargo features.
    pub fn compiled_features() -> Vec<String> {
        let mut features = Vec::new();
        if cfg!(feature = "jlcpcb") {
            features.push("jlcpcb".to_string());
        }
        if cfg!(feature = "freerouting") {
            features.push("freerouting".to_string());
        }
        if cfg!(feature = "native-bridge") {
            features.push("native-bridge".to_string());
        }
        if cfg!(feature = "issue-reporting") {
            features.push("issue-reporting".to_string());
        }
        features
    }

    pub fn report_type_for_category(category: &str) -> &'static str {
        match category {
            "missing_tool" | "missing_argument" => "feature_request",
            _ => "bug",
        }
    }

    /// Resolve the pending-reports directory inside the Copperpour state dir
    /// (`COPPERPOUR_STATE_DIR` or platform local-data dir, beside `locks`).
    pub fn default_pending_reports_dir() -> Option<PathBuf> {
        if let Some(state_dir) = crate::app_dirs::env_var("STATE_DIR") {
            let root = PathBuf::from(state_dir);
            if root.is_absolute() {
                return Some(root.join("pending_reports"));
            }
            return None;
        }
        crate::app_dirs::local_data_dir()
            .map(|dir| crate::app_dirs::user_dir(&dir, false).join("pending_reports"))
            .or_else(|| crate::app_dirs::user_state_dir().map(|d| d.join("pending_reports")))
    }

    /// File-backed store for scrubbed pending reports.
    #[derive(Debug, Clone)]
    pub struct PendingReportStore {
        dir: PathBuf,
    }

    impl PendingReportStore {
        pub fn new(dir: PathBuf) -> Self {
            Self { dir }
        }

        pub fn dir(&self) -> &Path {
            &self.dir
        }

        /// Load all valid, non-expired pending reports (pruning expired ones on disk).
        pub fn list(&self) -> Vec<PendingReport> {
            let now = unix_ms();
            let Ok(entries) = std::fs::read_dir(&self.dir) else {
                return Vec::new();
            };
            let mut reports = Vec::new();
            for entry in entries.flatten() {
                let path = entry.path();
                if path.extension().and_then(|e| e.to_str()) != Some("json") {
                    continue;
                }
                let Ok(content) = std::fs::read_to_string(&path) else {
                    continue;
                };
                let Ok(report) = serde_json::from_str::<PendingReport>(&content) else {
                    continue;
                };
                if now.saturating_sub(report.updated_at_ms) > MAX_PENDING_AGE_MS {
                    let _ = std::fs::remove_file(&path);
                    continue;
                }
                reports.push(report);
            }
            reports.sort_by_key(|r| r.updated_at_ms);
            reports
        }

        /// Store or deduplicate (by `signature`) a scrubbed pending report,
        /// enforcing the 20-report / 30-day cap and `0600` permissions on Unix.
        pub fn upsert(
            &self,
            signature: &str,
            summary: &str,
            mut payload: IssueReportPayload,
        ) -> anyhow::Result<PendingReport> {
            std::fs::create_dir_all(&self.dir)?;
            let now = unix_ms();
            let mut existing = self.list();

            if let Some(found) = existing.iter_mut().find(|r| r.signature == signature) {
                found.count = found.count.saturating_add(1);
                found.updated_at_ms = now;
                found.payload.occurrences = found.count;
                let updated = found.clone();
                self.write_report_file(&updated)?;
                return Ok(updated);
            }

            // Enforce cap before inserting a new report (drop oldest first).
            while existing.len() >= MAX_PENDING_REPORTS {
                let oldest = existing.remove(0);
                let _ = std::fs::remove_file(self.report_path(&oldest.id));
            }

            let id = format!("rep_{}", uuid::Uuid::new_v4().simple());
            payload.occurrences = 1;
            let report = PendingReport {
                id,
                signature: signature.to_string(),
                count: 1,
                created_at_ms: now,
                updated_at_ms: now,
                summary: summary.to_string(),
                payload,
            };
            self.write_report_file(&report)?;
            Ok(report)
        }

        /// Remove the given report IDs (or all if `all` is true), verifying by
        /// readback that removed files are gone.
        pub fn remove(
            &self,
            ids: &[String],
            all: bool,
        ) -> anyhow::Result<(Vec<String>, Vec<String>)> {
            let existing = self.list();
            let target_ids: Vec<String> = if all {
                existing.iter().map(|r| r.id.clone()).collect()
            } else {
                ids.to_vec()
            };

            let mut removed = Vec::new();
            let mut not_found = Vec::new();
            for id in target_ids {
                if !is_safe_report_id(&id) {
                    not_found.push(id);
                    continue;
                }
                let path = self.report_path(&id);
                if !path.exists() {
                    not_found.push(id);
                    continue;
                }
                std::fs::remove_file(&path)?;
                if path.exists() {
                    anyhow::bail!("pending report '{id}' still exists after removal");
                }
                removed.push(id);
            }
            Ok((removed, not_found))
        }

        pub fn report_path(&self, id: &str) -> PathBuf {
            self.dir.join(format!("{id}.json"))
        }

        fn write_report_file(&self, report: &PendingReport) -> anyhow::Result<()> {
            let path = self.report_path(&report.id);
            let tmp = self.dir.join(format!(
                ".{}.{}.tmp",
                report.id,
                uuid::Uuid::new_v4().simple()
            ));
            let serialized = serde_json::to_string_pretty(report)?;

            let mut options = std::fs::OpenOptions::new();
            options.write(true).create_new(true);
            #[cfg(unix)]
            {
                use std::os::unix::fs::OpenOptionsExt;
                options.mode(0o600);
            }
            let write_res = (|| -> anyhow::Result<()> {
                let mut file = options.open(&tmp)?;
                file.write_all(serialized.as_bytes())?;
                file.sync_all()?;
                drop(file);
                std::fs::rename(&tmp, &path)?;
                Ok(())
            })();
            if let Err(err) = write_res {
                let _ = std::fs::remove_file(&tmp);
                return Err(err);
            }

            // Readback verification: confirm the persisted file parses back with the same ID.
            let readback = std::fs::read_to_string(&path)?;
            let parsed: PendingReport = serde_json::from_str(&readback)?;
            if parsed.id != report.id || parsed.count != report.count {
                anyhow::bail!("pending report readback mismatch for '{}'", report.id);
            }
            Ok(())
        }
    }

    fn is_safe_report_id(id: &str) -> bool {
        !id.is_empty()
            && id
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
    }

    // ─── Sentry event building & sending ─────────────────────────────────────

    /// Build a scrubbed Sentry `Event` from a scrubbed `IssueReportPayload`.
    pub fn build_sentry_event(
        payload: &IssueReportPayload,
        signature: Option<&str>,
    ) -> sentry::protocol::Event<'static> {
        let level = match payload.category.as_str() {
            "crash" => sentry::Level::Fatal,
            "wrong_result" | "bad_refusal" => sentry::Level::Error,
            "missing_tool" | "missing_argument" | "docs_or_guidance" => sentry::Level::Info,
            _ => sentry::Level::Warning,
        };

        let mut tags = BTreeMap::new();
        tags.insert("category".to_string(), payload.category.clone());
        tags.insert("report_type".to_string(), payload.report_type.clone());
        tags.insert("source".to_string(), payload.source.clone());
        tags.insert("os".to_string(), payload.os.clone());
        tags.insert("arch".to_string(), payload.arch.clone());
        tags.insert("transport".to_string(), payload.transport.clone());
        if let Some(tool) = &payload.tool {
            tags.insert("tool".to_string(), tool.clone());
        }
        if let Some(error_kind) = &payload.error_kind {
            tags.insert("error_kind".to_string(), error_kind.clone());
        }
        if let Some(kv) = &payload.kicad_version {
            tags.insert("kicad_version".to_string(), kv.clone());
        }
        if let Some(kcv) = &payload.kicad_cli_version {
            tags.insert("kicad_cli_version".to_string(), kcv.clone());
        }

        let mut extra = BTreeMap::new();
        if let Some(arg) = &payload.argument {
            extra.insert("argument".to_string(), Value::String(arg.clone()));
        }
        if let Some(cap) = &payload.capability {
            extra.insert("capability".to_string(), Value::String(cap.clone()));
        }
        if let Some(exp) = &payload.expected {
            extra.insert("expected".to_string(), Value::String(exp.clone()));
        }
        if let Some(act) = &payload.actual {
            extra.insert("actual".to_string(), Value::String(act.clone()));
        }
        if let Some(work) = &payload.workaround {
            extra.insert("workaround".to_string(), Value::String(work.clone()));
        }
        extra.insert("features".to_string(), json!(payload.features));
        extra.insert("occurrences".to_string(), json!(payload.occurrences));
        extra.insert("recent_calls".to_string(), json!(payload.recent_calls));

        let fingerprint = signature
            .map(|sig| vec![std::borrow::Cow::Owned(sig.to_string())].into())
            .unwrap_or_default();

        sentry::protocol::Event {
            event_id: uuid::Uuid::new_v4(),
            level,
            message: Some(format!("[{}] {}", payload.category, payload.summary)),
            release: Some(format!("copperpour@{}", payload.version).into()),
            server_name: None,
            user: None,
            tags,
            extra,
            fingerprint,
            ..Default::default()
        }
    }

    // ─── Config persistence for `always_send` / `never_ask` ──────────────────

    /// Resolve the config file to update when the user picks `always_send` or
    /// `never_ask`. Uses the active config file if one was loaded, otherwise
    /// the platform's per-user `config.toml`.
    pub fn target_config_path(selected_path: Option<&Path>) -> Option<PathBuf> {
        if let Some(path) = selected_path {
            return Some(path.to_path_buf());
        }
        #[cfg(not(any(target_os = "windows", target_os = "macos")))]
        let dir = crate::app_dirs::home_dir()
            .map(|home| crate::app_dirs::user_dir(&home.join(".config"), false));
        #[cfg(any(target_os = "windows", target_os = "macos"))]
        let dir = crate::app_dirs::user_state_dir();
        dir.map(|d| d.join("config.toml"))
    }

    /// Update boolean keys under `issue_reporting` in a TOML or JSON config
    /// file, preserving all other settings, and verify by readback.
    pub fn persist_issue_reporting_settings(
        path: &Path,
        updates: &[(&str, bool)],
    ) -> anyhow::Result<()> {
        if let Some(parent) = path.parent() {
            if !parent.as_os_str().is_empty() {
                std::fs::create_dir_all(parent)?;
            }
        }
        let is_json = path.extension().and_then(|e| e.to_str()) == Some("json");
        let existing = match std::fs::read_to_string(path) {
            Ok(content) => Some(content),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
            Err(e) => return Err(e.into()),
        };

        let updated = if is_json {
            let mut root: serde_json::Map<String, Value> = match existing {
                Some(content) if !content.trim().is_empty() => serde_json::from_str(&content)?,
                _ => serde_json::Map::new(),
            };
            let section = root
                .entry("issue_reporting".to_string())
                .or_insert_with(|| Value::Object(serde_json::Map::new()));
            if !section.is_object() {
                *section = Value::Object(serde_json::Map::new());
            }
            if let Some(map) = section.as_object_mut() {
                for &(key, value) in updates {
                    map.insert(key.to_string(), Value::Bool(value));
                }
            }
            serde_json::to_string_pretty(&Value::Object(root))?
        } else {
            let mut root: toml::Table = match existing {
                Some(content) if !content.trim().is_empty() => toml::from_str(&content)?,
                _ => toml::Table::new(),
            };
            let section = root
                .entry("issue_reporting".to_string())
                .or_insert_with(|| toml::Value::Table(toml::Table::new()));
            if !section.is_table() {
                *section = toml::Value::Table(toml::Table::new());
            }
            if let Some(tbl) = section.as_table_mut() {
                for &(key, value) in updates {
                    tbl.insert(key.to_string(), toml::Value::Boolean(value));
                }
            }
            toml::to_string_pretty(&root)?
        };

        let tmp = path.with_extension(format!(
            "{}.tmp.{}",
            path.extension().and_then(|e| e.to_str()).unwrap_or("toml"),
            uuid::Uuid::new_v4().simple()
        ));
        let write_res = (|| -> anyhow::Result<()> {
            std::fs::write(&tmp, updated.as_bytes())?;
            std::fs::rename(&tmp, path)?;
            Ok(())
        })();
        if let Err(err) = write_res {
            let _ = std::fs::remove_file(&tmp);
            return Err(err);
        }

        // Readback verification.
        let readback = std::fs::read_to_string(path)?;
        for &(key, value) in updates {
            let verified = if is_json {
                serde_json::from_str::<Value>(&readback)
                    .ok()
                    .and_then(|v| v.get("issue_reporting")?.get(key)?.as_bool())
            } else {
                toml::from_str::<toml::Table>(&readback)
                    .ok()
                    .and_then(|t| t.get("issue_reporting")?.as_table()?.get(key)?.as_bool())
            };
            if verified != Some(value) {
                anyhow::bail!(
                    "config readback mismatch for issue_reporting.{key} in '{}'",
                    path.display()
                );
            }
        }
        Ok(())
    }

    pub fn persist_issue_reporting_setting(
        path: &Path,
        key: &str,
        value: bool,
    ) -> anyhow::Result<()> {
        persist_issue_reporting_settings(path, &[(key, value)])
    }

    // ─── IssueReporter ───────────────────────────────────────────────────────

    pub struct IssueReporter {
        config: RwLock<IssueReportingConfig>,
        config_file_path: Option<PathBuf>,
        store: Option<PendingReportStore>,
        dsn_override: Option<String>,
        transport_override: Option<Arc<dyn sentry::TransportFactory>>,
        client: OnceLock<Option<Arc<sentry::Client>>>,
        transport_name: String,
        project_dir: Option<PathBuf>,
        session_sent_signatures: Mutex<HashSet<String>>,
        notice_shown_this_session: AtomicBool,
    }

    impl IssueReporter {
        pub fn new(
            config: IssueReportingConfig,
            config_file_path: Option<PathBuf>,
            transport_name: String,
            project_dir: Option<PathBuf>,
        ) -> Self {
            Self {
                config: RwLock::new(config.effective()),
                config_file_path,
                store: default_pending_reports_dir().map(PendingReportStore::new),
                dsn_override: None,
                transport_override: None,
                client: OnceLock::new(),
                transport_name,
                project_dir,
                session_sent_signatures: Mutex::new(HashSet::new()),
                notice_shown_this_session: AtomicBool::new(false),
            }
        }

        /// Construct a reporter that does not read or write the host's
        /// pending-reports directory on disk. Used as the default for
        /// `ToolContext` until a server entry point installs the disk-backed
        /// reporter, so unit tests never pollute or race on the host state dir.
        pub fn in_memory(
            config: IssueReportingConfig,
            transport_name: String,
            project_dir: Option<PathBuf>,
        ) -> Self {
            Self {
                config: RwLock::new(config.effective()),
                config_file_path: None,
                store: None,
                dsn_override: None,
                transport_override: None,
                client: OnceLock::new(),
                transport_name,
                project_dir,
                session_sent_signatures: Mutex::new(HashSet::new()),
                notice_shown_this_session: AtomicBool::new(false),
            }
        }

        /// Construct a reporter with explicit store directory, DSN, and/or
        /// Sentry transport factory (for tests).
        pub fn with_overrides(
            config: IssueReportingConfig,
            config_file_path: Option<PathBuf>,
            store_dir: Option<PathBuf>,
            dsn_override: Option<String>,
            transport_override: Option<Arc<dyn sentry::TransportFactory>>,
            transport_name: String,
            project_dir: Option<PathBuf>,
        ) -> Self {
            Self {
                config: RwLock::new(config),
                config_file_path,
                store: store_dir.map(PendingReportStore::new),
                dsn_override,
                transport_override,
                client: OnceLock::new(),
                transport_name,
                project_dir,
                session_sent_signatures: Mutex::new(HashSet::new()),
                notice_shown_this_session: AtomicBool::new(false),
            }
        }

        pub fn effective_config(&self) -> IssueReportingConfig {
            self.config.read().map(|g| g.clone()).unwrap_or_default()
        }

        pub fn config_file_path(&self) -> Option<&Path> {
            self.config_file_path.as_deref()
        }

        pub fn store(&self) -> Option<&PendingReportStore> {
            self.store.as_ref()
        }

        pub fn scrub_context(&self) -> ScrubContext {
            ScrubContext::from_environment(self.project_dir.as_deref())
        }

        pub fn transport_name(&self) -> &str {
            &self.transport_name
        }

        fn sentry_client(&self) -> Option<&Arc<sentry::Client>> {
            self.client
                .get_or_init(|| {
                    let dsn_str = self.dsn_override.as_deref().unwrap_or(DEFAULT_SENTRY_DSN);
                    let dsn = sentry::types::Dsn::from_str(dsn_str).ok()?;
                    let mut opts = sentry::ClientOptions::default();
                    opts.dsn = Some(dsn);
                    opts.release = Some(format!("copperpour@{}", env!("CARGO_PKG_VERSION")).into());
                    opts.send_default_pii = false;
                    opts.server_name = None;
                    opts.default_integrations = false;
                    opts.auto_session_tracking = false;
                    opts.shutdown_timeout = std::time::Duration::from_secs(1);
                    opts.before_send = Some(Arc::new(|mut event| {
                        event.server_name = None;
                        event.user = None;
                        Some(event)
                    }));
                    opts.transport =
                        Some(self.transport_override.clone().unwrap_or_else(|| {
                            Arc::new(sentry::transports::DefaultTransportFactory)
                        }));
                    Some(Arc::new(sentry::Client::from_config(opts)))
                })
                .as_ref()
        }

        pub fn flush_if_initialized(&self, timeout: std::time::Duration) {
            if let Some(Some(client)) = self.client.get() {
                client.flush(Some(timeout));
            }
        }

        /// Send a scrubbed payload to Sentry in the background (non-blocking)
        /// and return the Sentry event ID.
        pub fn send_payload(
            &self,
            payload: &IssueReportPayload,
            signature: Option<&str>,
        ) -> anyhow::Result<String> {
            let cfg = self.effective_config();
            if !cfg.enabled {
                anyhow::bail!("issue reporting is disabled");
            }
            let Some(client) = self.sentry_client() else {
                anyhow::bail!("Sentry client is unavailable (invalid DSN)");
            };
            let event = build_sentry_event(payload, signature);
            // `capture_event` enqueues onto Sentry's background transport
            // thread and returns immediately without blocking on network I/O.
            let sent_id = client.capture_event(event, None);
            if sent_id.is_nil() {
                anyhow::bail!("Sentry transport did not accept event");
            }
            Ok(sent_id.to_string())
        }

        /// Capture a panic according to the current config:
        /// - `!enabled || !capture`: do nothing.
        /// - `automatic`: send once per signature per session, store nothing.
        /// - `capture && !automatic`: scrub and store locally as a pending report.
        pub fn capture_panic(
            &self,
            message: &str,
            location: Option<&str>,
        ) -> Option<PendingReport> {
            let cfg = self.effective_config();
            if !cfg.enabled || !cfg.capture {
                return None;
            }
            let scrub = self.scrub_context();
            let scrubbed_loc = location.map(|l| scrub_text(l, &scrub));
            // The message can name references, nets or files, so only its
            // fingerprint is kept (#824's privacy rules).
            let fingerprint = message_fingerprint(&scrub_text(message, &scrub));
            let sig = match &scrubbed_loc {
                Some(loc) => format!("panic:{loc}"),
                None => format!("panic:{fingerprint}"),
            };
            let summary = match &scrubbed_loc {
                Some(loc) => format!("Panic at {loc} (message {fingerprint})"),
                None => format!("Panic (message {fingerprint})"),
            };
            let mut payload = IssueReportPayload {
                category: "crash".to_string(),
                report_type: "bug".to_string(),
                summary: summary.clone(),
                tool: None,
                argument: None,
                capability: None,
                expected: None,
                actual: None,
                error_kind: Some("panic".to_string()),
                workaround: None,
                source: "panic".to_string(),
                version: env!("CARGO_PKG_VERSION").to_string(),
                features: compiled_features(),
                os: std::env::consts::OS.to_string(),
                arch: std::env::consts::ARCH.to_string(),
                kicad_version: None,
                kicad_cli_version: None,
                transport: self.transport_name.clone(),
                recent_calls: Vec::new(),
                occurrences: 1,
            };
            payload.scrub(&scrub);
            self.handle_auto_capture(&sig, &payload.summary.clone(), payload, true)
        }

        /// Capture a runtime `handler_error` or `mutation_outcome_uncertain`
        /// result from tool dispatch. Returns a `pending_reports` notice if
        /// one should be attached to this tool result (at most once per session).
        pub fn capture_tool_error(
            &self,
            tool: &str,
            error_kind: &str,
            error_message: &str,
            kicad_version: Option<String>,
            kicad_cli_version: Option<String>,
            recent_calls: Vec<SanitizedCallRecord>,
        ) -> Option<Value> {
            let cfg = self.effective_config();
            if !cfg.enabled || !cfg.capture {
                return None;
            }
            let scrub = self.scrub_context();
            // The message can name references, nets or files, so only its
            // fingerprint is kept (#824's privacy rules).
            let fingerprint = message_fingerprint(&scrub_text(error_message, &scrub));
            let sig = format!("{error_kind}:{tool}:{fingerprint}");
            let category = match error_kind {
                "mutation_outcome_uncertain" => "wrong_result",
                _ => "bad_refusal",
            };
            let summary = format!("{tool} returned {error_kind} (message {fingerprint})");
            let mut payload = IssueReportPayload {
                category: category.to_string(),
                report_type: "bug".to_string(),
                summary: summary.clone(),
                tool: Some(tool.to_string()),
                argument: None,
                capability: None,
                expected: None,
                actual: None,
                error_kind: Some(error_kind.to_string()),
                workaround: None,
                source: error_kind.to_string(),
                version: env!("CARGO_PKG_VERSION").to_string(),
                features: compiled_features(),
                os: std::env::consts::OS.to_string(),
                arch: std::env::consts::ARCH.to_string(),
                kicad_version,
                kicad_cli_version,
                transport: self.transport_name.clone(),
                recent_calls,
                occurrences: 1,
            };
            payload.scrub(&scrub);
            let stored =
                self.handle_auto_capture(&sig, &payload.summary.clone(), payload, false)?;
            if self
                .notice_shown_this_session
                .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
                .is_ok()
            {
                Some(build_pending_reports_notice_json(&[stored]))
            } else {
                None
            }
        }

        fn handle_auto_capture(
            &self,
            signature: &str,
            summary: &str,
            payload: IssueReportPayload,
            flush_on_send: bool,
        ) -> Option<PendingReport> {
            let cfg = self.effective_config();
            if !cfg.enabled || !cfg.capture {
                return None;
            }
            if cfg.automatic {
                // One event per signature per session; store nothing on disk.
                let is_new = self
                    .session_sent_signatures
                    .lock()
                    .map(|mut set| set.insert(signature.to_string()))
                    .unwrap_or(false);
                if is_new && self.send_payload(&payload, Some(signature)).is_ok() && flush_on_send {
                    if let Some(client) = self.sentry_client() {
                        client.flush(Some(std::time::Duration::from_secs(1)));
                    }
                }
                return None;
            }
            let store = self.store.as_ref()?;
            store.upsert(signature, summary, payload).ok()
        }

        /// List pending reports on disk.
        pub fn list_pending(&self) -> Vec<PendingReport> {
            self.store
                .as_ref()
                .map(PendingReportStore::list)
                .unwrap_or_default()
        }

        /// Build the `initialize` instructions notice for any pending reports
        /// from a previous session, and mark the session as already asked.
        pub fn initialize_pending_notice(&self) -> Option<String> {
            let cfg = self.effective_config();
            if !cfg.enabled || !cfg.capture {
                return None;
            }
            let pending = self.list_pending();
            if pending.is_empty() {
                return None;
            }
            if self
                .notice_shown_this_session
                .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
                .is_err()
            {
                return None;
            }
            Some(format_pending_reports_notice_text(&pending))
        }

        /// Resolved config path used when persisting `always_send` / `never_ask`.
        pub fn resolved_config_path(&self) -> Option<PathBuf> {
            target_config_path(self.config_file_path.as_deref())
        }

        /// Persist `issue_reporting.automatic = true` (and `capture = true`) and
        /// update in-memory state (still honoring `DO_NOT_TRACK=1`).
        pub fn enable_always_send(&self) -> anyhow::Result<PathBuf> {
            let path = self.resolved_config_path().ok_or_else(|| {
                anyhow::anyhow!(
                    "cannot persist issue_reporting.automatic: {}",
                    crate::app_dirs::NO_USER_DIR
                )
            })?;
            persist_issue_reporting_settings(&path, &[("capture", true), ("automatic", true)])?;
            if let Ok(mut cfg) = self.config.write() {
                cfg.capture = true;
                cfg.automatic = !env_do_not_track();
            }
            Ok(path)
        }

        /// Persist `issue_reporting.capture = false` and update in-memory state.
        pub fn enable_never_ask(&self) -> anyhow::Result<PathBuf> {
            let path = self.resolved_config_path().ok_or_else(|| {
                anyhow::anyhow!(
                    "cannot persist issue_reporting.capture: {}",
                    crate::app_dirs::NO_USER_DIR
                )
            })?;
            persist_issue_reporting_setting(&path, "capture", false)?;
            if let Ok(mut cfg) = self.config.write() {
                cfg.capture = false;
                cfg.automatic = false;
            }
            Ok(path)
        }
    }

    pub fn build_pending_reports_notice_json(reports: &[PendingReport]) -> Value {
        let items: Vec<Value> = reports
            .iter()
            .map(|r| {
                json!({
                    "id": r.id,
                    "summary": r.summary,
                    "count": r.count,
                    "payload": r.payload,
                })
            })
            .collect();
        json!({
            "message": "Copperpour captured error/crash report(s) locally. Show the exact payload to the user and ask them to choose one of the four options before sending anything.",
            "reports": items,
            "choices": {
                "send": "Call report_issue(pending_ids: [...]) to send these reports and return their Sentry event IDs.",
                "discard": "Call discard_pending_reports(pending_ids: [...]) to delete them without sending.",
                "always_send": "Call report_issue(pending_ids: [...], always_send: true) to set issue_reporting.automatic = true and send these; future reports will send automatically without storing.",
                "never_ask": "Call discard_pending_reports(pending_ids: [...], never_ask: true) to set issue_reporting.capture = false and stop storing reports."
            }
        })
    }

    pub fn format_pending_reports_notice_text(reports: &[PendingReport]) -> String {
        let notice = build_pending_reports_notice_json(reports);
        format!(
            "pending_reports: {}",
            serde_json::to_string(&notice).unwrap_or_default()
        )
    }

    #[cfg(test)]
    mod tests {
        use super::*;
        use sentry::protocol::Event;
        use sentry::{Envelope, Transport, TransportFactory};

        struct RecordingTransport {
            events: Arc<Mutex<Vec<Event<'static>>>>,
        }

        impl Transport for RecordingTransport {
            fn send_envelope(&self, envelope: Envelope) {
                if let Some(event) = envelope.event() {
                    self.events.lock().unwrap().push(event.clone());
                }
            }
        }

        struct RecordingFactory {
            events: Arc<Mutex<Vec<Event<'static>>>>,
        }

        impl TransportFactory for RecordingFactory {
            fn create_transport(&self, _options: &sentry::ClientOptions) -> Arc<dyn Transport> {
                Arc::new(RecordingTransport {
                    events: self.events.clone(),
                })
            }
        }

        #[test]
        fn scrubbing_removes_project_home_user_host_ip_and_absolute_paths() {
            let scrub = ScrubContext {
                home_dir: Some("/home/alice_unique_99".to_string()),
                project_dir: Some("/home/alice_unique_99/boards/secret_board_xyz".to_string()),
                username: Some("alice_unique_99".to_string()),
                hostname: Some("workstation-host-77".to_string()),
            };
            let raw = "Failed on /home/alice_unique_99/boards/secret_board_xyz/main.kicad_sch \
                       for user alice_unique_99 on workstation-host-77 (192.168.42.17) \
                       at /tmp/other_secret_dir/foo.kicad_pcb and \
                       /home/alice_unique_99/.copperpour/logs/calls.jsonl \
                       in /build/Copperpour/crates/copperpour-core/src/tools/sch_wiring.rs:42";
            let scrubbed = scrub_text(raw, &scrub);

            assert!(
                !scrubbed.contains("alice_unique_99"),
                "username survived: {scrubbed}"
            );
            assert!(
                !scrubbed.contains("secret_board_xyz"),
                "project path survived: {scrubbed}"
            );
            assert!(
                !scrubbed.contains("main.kicad_sch"),
                "project filename survived: {scrubbed}"
            );
            assert!(
                !scrubbed.contains("workstation-host-77"),
                "hostname survived: {scrubbed}"
            );
            assert!(
                !scrubbed.contains("192.168.42.17"),
                "IP survived: {scrubbed}"
            );
            assert!(
                !scrubbed.contains("other_secret_dir"),
                "abs path survived: {scrubbed}"
            );
            assert!(
                scrubbed.contains("<project>"),
                "missing <project>: {scrubbed}"
            );
            assert!(scrubbed.contains("<host>"), "missing <host>: {scrubbed}");
            assert!(scrubbed.contains("<user>"), "missing <user>: {scrubbed}");
            assert!(scrubbed.contains("<ip>"), "missing <ip>: {scrubbed}");
            assert!(
                scrubbed.contains("~/.copperpour/logs/calls.jsonl"),
                "Copperpour own path should stay readable: {scrubbed}"
            );
            assert!(
                scrubbed.contains("crates/copperpour-core/src/tools/sch_wiring.rs:42"),
                "Copperpour source path should stay readable: {scrubbed}"
            );
        }

        #[test]
        fn built_sentry_event_has_release_no_pii_and_feature_request_tag() {
            let payload = IssueReportPayload {
                category: "missing_tool".to_string(),
                report_type: "feature_request".to_string(),
                summary: "Need impedance calculator".to_string(),
                tool: None,
                argument: None,
                capability: Some("microstrip impedance calculation".to_string()),
                expected: None,
                actual: None,
                error_kind: None,
                workaround: None,
                source: "agent".to_string(),
                version: "0.17.0".to_string(),
                features: vec!["issue-reporting".to_string()],
                os: "linux".to_string(),
                arch: "x86_64".to_string(),
                kicad_version: Some("10.0.5".to_string()),
                kicad_cli_version: Some("10.0.5".to_string()),
                transport: "stdio".to_string(),
                recent_calls: vec![SanitizedCallRecord {
                    tool: "list_toolboxes".to_string(),
                    toolset: None,
                    status: "ok".to_string(),
                    error_kind: None,
                    dur_ms: 3,
                }],
                occurrences: 1,
            };
            let event = build_sentry_event(&payload, None);
            assert_eq!(event.release.as_deref(), Some("copperpour@0.17.0"));
            assert!(event.server_name.is_none());
            assert!(event.user.is_none());
            assert_eq!(
                event.tags.get("report_type").map(String::as_str),
                Some("feature_request")
            );
            assert_eq!(
                event.tags.get("category").map(String::as_str),
                Some("missing_tool")
            );
        }

        #[test]
        fn panic_capture_stores_pending_by_default_and_deduplicates_by_signature() {
            let dir = tempfile::tempdir().unwrap();
            let events = Arc::new(Mutex::new(Vec::new()));
            let reporter = IssueReporter::with_overrides(
                IssueReportingConfig::default(),
                None,
                Some(dir.path().to_path_buf()),
                Some(DEFAULT_SENTRY_DSN.to_string()),
                Some(Arc::new(RecordingFactory {
                    events: events.clone(),
                })),
                "stdio".to_string(),
                Some(PathBuf::from("/home/secret_user_1/unique_proj_alpha")),
            );

            let first = reporter
                .capture_panic(
                    "boom in /home/secret_user_1/unique_proj_alpha/top.kicad_sch",
                    Some("/build/crates/copperpour-core/src/tools/foo.rs:12:5"),
                )
                .expect("stored pending report");
            let second = reporter
                .capture_panic(
                    "boom again",
                    Some("/build/crates/copperpour-core/src/tools/foo.rs:12:5"),
                )
                .expect("deduplicated pending report");

            assert_eq!(first.id, second.id);
            assert_eq!(second.count, 2);
            assert!(
                events.lock().unwrap().is_empty(),
                "default mode must not send"
            );

            // Verify on-disk file is scrubbed and 0600 on Unix.
            let file_path = reporter.store().unwrap().report_path(&first.id);
            let on_disk = std::fs::read_to_string(&file_path).unwrap();
            assert!(
                !on_disk.contains("unique_proj_alpha"),
                "raw project path on disk: {on_disk}"
            );
            assert!(
                !on_disk.contains("top.kicad_sch"),
                "raw filename on disk: {on_disk}"
            );
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                let mode = std::fs::metadata(&file_path).unwrap().permissions().mode() & 0o777;
                assert_eq!(mode, 0o600, "pending report file must be 0600");
            }
        }

        /// Captured messages name references, nets and files; #824 forbids
        /// those in a report, so neither the stored nor the sent report
        /// holds the message, only its fingerprint.
        #[test]
        fn captured_messages_leave_no_design_names_in_reports() {
            const SECRETS: [&str; 4] = [
                "U_SECRET7",
                "NET_SECRET9",
                "secret_board.kicad_sch",
                "LBL_SECRET3",
            ];
            let message = "Component 'U_SECRET7' on net NET_SECRET9 in secret_board.kicad_sch \
                           (label LBL_SECRET3) could not be read back";
            for automatic in [false, true] {
                let dir = tempfile::tempdir().unwrap();
                let events = Arc::new(Mutex::new(Vec::new()));
                let reporter = IssueReporter::with_overrides(
                    IssueReportingConfig {
                        enabled: true,
                        capture: true,
                        automatic,
                    },
                    None,
                    Some(dir.path().to_path_buf()),
                    Some(DEFAULT_SENTRY_DSN.to_string()),
                    Some(Arc::new(RecordingFactory {
                        events: events.clone(),
                    })),
                    "stdio".to_string(),
                    None,
                );
                reporter.capture_tool_error(
                    "add_wire",
                    "mutation_outcome_uncertain",
                    message,
                    None,
                    None,
                    Vec::new(),
                );
                reporter.capture_panic(message, None);
                let mut written: Vec<String> = std::fs::read_dir(dir.path())
                    .into_iter()
                    .flatten()
                    .flatten()
                    .filter_map(|entry| std::fs::read_to_string(entry.path()).ok())
                    .collect();
                written.extend(
                    events
                        .lock()
                        .unwrap()
                        .iter()
                        .map(|event| serde_json::to_string(event).unwrap()),
                );
                assert_eq!(written.len(), 2, "automatic={automatic}: {written:?}");
                for text in &written {
                    assert!(
                        text.contains(&message_fingerprint(message)),
                        "the fingerprint identifies the message: {text}"
                    );
                    for secret in SECRETS {
                        assert!(!text.contains(secret), "{secret} in report: {text}");
                    }
                }
            }
        }

        #[test]
        fn automatic_mode_sends_once_per_signature_and_stores_nothing() {
            let dir = tempfile::tempdir().unwrap();
            let events = Arc::new(Mutex::new(Vec::new()));
            let reporter = IssueReporter::with_overrides(
                IssueReportingConfig {
                    enabled: true,
                    capture: true,
                    automatic: true,
                },
                None,
                Some(dir.path().to_path_buf()),
                Some(DEFAULT_SENTRY_DSN.to_string()),
                Some(Arc::new(RecordingFactory {
                    events: events.clone(),
                })),
                "stdio".to_string(),
                None,
            );

            assert!(reporter
                .capture_panic("boom", Some("crates/copperpour-core/src/a.rs:10:1"))
                .is_none());
            assert!(reporter
                .capture_panic("boom", Some("crates/copperpour-core/src/a.rs:10:1"))
                .is_none());
            assert!(reporter.list_pending().is_empty());
            assert_eq!(
                events.lock().unwrap().len(),
                1,
                "automatic mode sends one event per signature per session"
            );
        }

        #[test]
        fn capture_false_stores_nothing_and_sends_nothing() {
            let dir = tempfile::tempdir().unwrap();
            let events = Arc::new(Mutex::new(Vec::new()));
            let reporter = IssueReporter::with_overrides(
                IssueReportingConfig {
                    enabled: true,
                    capture: false,
                    automatic: false,
                },
                None,
                Some(dir.path().to_path_buf()),
                Some(DEFAULT_SENTRY_DSN.to_string()),
                Some(Arc::new(RecordingFactory {
                    events: events.clone(),
                })),
                "stdio".to_string(),
                None,
            );

            assert!(reporter.capture_panic("boom", None).is_none());
            assert!(reporter.list_pending().is_empty());
            assert!(events.lock().unwrap().is_empty());
        }

        #[test]
        fn unroutable_dsn_does_not_block_send() {
            let dir = tempfile::tempdir().unwrap();
            let reporter = IssueReporter::with_overrides(
                IssueReportingConfig::default(),
                None,
                Some(dir.path().to_path_buf()),
                // RFC 5737 TEST-NET-1 unroutable address
                Some("https://public@192.0.2.1/1".to_string()),
                None,
                "stdio".to_string(),
                None,
            );
            let payload = IssueReportPayload {
                category: "other".to_string(),
                report_type: "bug".to_string(),
                summary: "unroutable latency test".to_string(),
                tool: None,
                argument: None,
                capability: None,
                expected: None,
                actual: None,
                error_kind: None,
                workaround: None,
                source: "agent".to_string(),
                version: env!("CARGO_PKG_VERSION").to_string(),
                features: compiled_features(),
                os: std::env::consts::OS.to_string(),
                arch: std::env::consts::ARCH.to_string(),
                kicad_version: None,
                kicad_cli_version: None,
                transport: "stdio".to_string(),
                recent_calls: Vec::new(),
                occurrences: 1,
            };
            let start = std::time::Instant::now();
            let id = reporter.send_payload(&payload, None).unwrap();
            let elapsed = start.elapsed();
            assert!(!id.is_empty());
            assert_ne!(
                id,
                uuid::Uuid::nil().to_string(),
                "send_payload must return a real non-nil Sentry event ID"
            );
            assert!(
                elapsed < std::time::Duration::from_millis(500),
                "send_payload blocked for {elapsed:?} on unroutable DSN"
            );
        }

        #[test]
        fn pending_store_caps_at_max_reports_dropping_oldest() {
            let dir = tempfile::tempdir().unwrap();
            let store = PendingReportStore::new(dir.path().to_path_buf());
            let base = IssueReportPayload {
                category: "other".to_string(),
                report_type: "bug".to_string(),
                summary: "cap test".to_string(),
                tool: None,
                argument: None,
                capability: None,
                expected: None,
                actual: None,
                error_kind: None,
                workaround: None,
                source: "handler_error".to_string(),
                version: "0.17.0".to_string(),
                features: vec![],
                os: "linux".to_string(),
                arch: "x86_64".to_string(),
                kicad_version: None,
                kicad_cli_version: None,
                transport: "stdio".to_string(),
                recent_calls: vec![],
                occurrences: 1,
            };
            for i in 0..(MAX_PENDING_REPORTS + 5) {
                store
                    .upsert(&format!("sig:{i}"), &format!("summary {i}"), base.clone())
                    .unwrap();
            }
            let listed = store.list();
            assert_eq!(listed.len(), MAX_PENDING_REPORTS);
        }

        #[test]
        fn persist_setting_preserves_other_keys_in_toml_and_json() {
            let dir = tempfile::tempdir().unwrap();
            let toml_path = dir.path().join("config.toml");
            std::fs::write(&toml_path, "log_level = \"debug\"\n").unwrap();
            persist_issue_reporting_setting(&toml_path, "automatic", true).unwrap();
            let parsed: toml::Table =
                toml::from_str(&std::fs::read_to_string(&toml_path).unwrap()).unwrap();
            assert_eq!(parsed["log_level"].as_str(), Some("debug"));
            assert_eq!(parsed["issue_reporting"]["automatic"].as_bool(), Some(true));

            let json_path = dir.path().join("settings.json");
            std::fs::write(&json_path, r#"{"log_level":"warn"}"#).unwrap();
            persist_issue_reporting_setting(&json_path, "capture", false).unwrap();
            let parsed_json: Value =
                serde_json::from_str(&std::fs::read_to_string(&json_path).unwrap()).unwrap();
            assert_eq!(parsed_json["log_level"], "warn");
            assert_eq!(parsed_json["issue_reporting"]["capture"], false);
        }

        #[test]
        fn scrubbing_handles_compressed_ipv6_timestamps_windows_paths_and_user_copperpour_dirs() {
            let scrub = ScrubContext {
                home_dir: Some("/home/ed".to_string()),
                project_dir: None,
                username: Some("ed".to_string()),
                hostname: Some("pi".to_string()),
            };
            let raw = "At 12:34:56 failed for ed on pi from ::1 and fe80::1: \
                       /tmp/copperpour/secret_client/board.kicad_sch \
                       /home/ed/my_plugin/secret_board.kicad_sch \
                       C:\\Users\\ed\\SecretBoard\\main.kicad_pcb \
                       \\\\fileserver\\share\\secret.kicad_sch";
            let scrubbed = scrub_text(raw, &scrub);

            assert!(
                scrubbed.contains("12:34:56"),
                "timestamp must survive: {scrubbed}"
            );
            assert!(
                scrubbed.contains("failed"),
                "short username must not corrupt words: {scrubbed}"
            );
            assert!(
                !scrubbed.contains("::1"),
                "compressed IPv6 leaked: {scrubbed}"
            );
            assert!(!scrubbed.contains("fe80::1"), "IPv6 leaked: {scrubbed}");
            assert!(
                !scrubbed.contains("secret_client"),
                "user /copperpour/ path leaked: {scrubbed}"
            );
            assert!(
                !scrubbed.contains("secret_board.kicad_sch"),
                "user plugin/ path leaked: {scrubbed}"
            );
            assert!(
                !scrubbed.contains("SecretBoard"),
                "Windows path leaked: {scrubbed}"
            );
            assert!(
                !scrubbed.contains("fileserver"),
                "UNC path leaked: {scrubbed}"
            );
        }

        #[test]
        fn invalid_dsn_returns_error_instead_of_fabricating_event_id() {
            let dir = tempfile::tempdir().unwrap();
            let reporter = IssueReporter::with_overrides(
                IssueReportingConfig::default(),
                None,
                Some(dir.path().to_path_buf()),
                Some("not-a-valid-dsn".to_string()),
                None,
                "stdio".to_string(),
                None,
            );
            let payload = IssueReportPayload {
                category: "other".to_string(),
                report_type: "bug".to_string(),
                summary: "bad dsn".to_string(),
                tool: None,
                argument: None,
                capability: None,
                expected: None,
                actual: None,
                error_kind: None,
                workaround: None,
                source: "agent".to_string(),
                version: "0.17.0".to_string(),
                features: vec![],
                os: "linux".to_string(),
                arch: "x86_64".to_string(),
                kicad_version: None,
                kicad_cli_version: None,
                transport: "stdio".to_string(),
                recent_calls: vec![],
                occurrences: 1,
            };
            assert!(reporter.send_payload(&payload, None).is_err());
        }

        #[test]
        fn expired_pending_reports_are_pruned_on_list() {
            let dir = tempfile::tempdir().unwrap();
            let store = PendingReportStore::new(dir.path().to_path_buf());
            let mut old = PendingReport {
                id: "rep_expired_1".to_string(),
                signature: "sig:old".to_string(),
                count: 1,
                created_at_ms: 1,
                updated_at_ms: 1,
                summary: "old report".to_string(),
                payload: IssueReportPayload {
                    category: "other".to_string(),
                    report_type: "bug".to_string(),
                    summary: "old report".to_string(),
                    tool: None,
                    argument: None,
                    capability: None,
                    expected: None,
                    actual: None,
                    error_kind: None,
                    workaround: None,
                    source: "panic".to_string(),
                    version: "0.17.0".to_string(),
                    features: vec![],
                    os: "linux".to_string(),
                    arch: "x86_64".to_string(),
                    kicad_version: None,
                    kicad_cli_version: None,
                    transport: "stdio".to_string(),
                    recent_calls: vec![],
                    occurrences: 1,
                },
            };
            let path = store.report_path(&old.id);
            std::fs::write(&path, serde_json::to_string(&old).unwrap()).unwrap();
            assert!(store.list().is_empty(), "expired report must be pruned");
            assert!(!path.exists(), "expired file must be deleted from disk");

            old.updated_at_ms = unix_ms();
            std::fs::write(&path, serde_json::to_string(&old).unwrap()).unwrap();
            assert_eq!(store.list().len(), 1);
        }
    }
}

#[cfg(feature = "issue-reporting")]
pub use imp::*;

#[cfg(test)]
mod config_tests {
    use super::*;

    #[test]
    fn env_overrides_disable_and_respect_do_not_track() {
        let cfg = IssueReportingConfig {
            enabled: true,
            capture: true,
            automatic: true,
        };
        let dnt = cfg.effective_with(false, true);
        assert!(dnt.enabled);
        assert!(dnt.capture);
        assert!(!dnt.automatic, "DO_NOT_TRACK=1 must force automatic=false");

        let disabled = cfg.effective_with(true, false);
        assert!(!disabled.enabled);
        assert!(!disabled.capture);
        assert!(!disabled.automatic);
    }
}
