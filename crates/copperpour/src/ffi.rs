//! C ABI exports for KiCAD plugin integration.
//!
//! KiCAD (or the thin Python launcher) can load this cdylib and call:
//!   kicad_plugin_init(config_path)  — start the embedded MCP server thread
//!   kicad_plugin_version()          — return the version string
//!   kicad_plugin_shutdown()         — stop the server cleanly
//!
//! The MCP server runs in a background tokio thread; the calling process
//! communicates with it via the configured transport (STDIO or HTTP/SSE).

use crate::config::{Config, ConfigLoadError, ConfigLoadOrigin, TransportMode};
use serde::Serialize;
use std::ffi::{c_char, c_int, CStr, CString};
use std::io::Write;
use std::path::PathBuf;
use std::sync::{Mutex, OnceLock};

static INIT_LOCK: Mutex<()> = Mutex::new(());
static RUNTIME: OnceLock<tokio::runtime::Runtime> = OnceLock::new();
static VERSION_CSTR: OnceLock<CString> = OnceLock::new();
static LAST_REFUSAL: Mutex<Option<StartupRefusal>> = Mutex::new(None);

/// Structured startup refusal recorded and logged when `kicad_plugin_init`
/// refuses to start the embedded server.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct StartupRefusal {
    pub outcome: &'static str,
    pub started: bool,
    pub defaults_substituted: bool,
    pub origin: &'static str,
    pub error: StartupRefusalError,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct StartupRefusalError {
    pub kind: &'static str,
    pub path: Option<String>,
    pub reason: String,
    pub message: String,
}

impl StartupRefusal {
    fn from_config_error(error: &ConfigLoadError) -> Self {
        let message = error.to_string();
        let (kind, origin, path, reason) = match error {
            ConfigLoadError::EmptyPath { origin } => (
                "invalid_argument",
                origin.as_str(),
                None,
                "empty_path".to_string(),
            ),
            ConfigLoadError::NotFound {
                origin,
                path,
                detail,
            } => (
                "file_not_found",
                origin.as_str(),
                Some(path.display().to_string()),
                format!("not_found: {detail}"),
            ),
            ConfigLoadError::Unreadable {
                origin,
                path,
                detail,
            } => (
                "invalid_configuration",
                origin.as_str(),
                Some(path.display().to_string()),
                format!("unreadable: {detail}"),
            ),
            ConfigLoadError::MalformedJson {
                origin,
                path,
                detail,
            } => (
                "invalid_configuration",
                origin.as_str(),
                Some(path.display().to_string()),
                format!("malformed_json: {detail}"),
            ),
            ConfigLoadError::MalformedToml {
                origin,
                path,
                detail,
            } => (
                "invalid_configuration",
                origin.as_str(),
                Some(path.display().to_string()),
                format!("malformed_toml: {detail}"),
            ),
        };
        Self {
            outcome: "failed",
            started: false,
            defaults_substituted: false,
            origin,
            error: StartupRefusalError {
                kind,
                path,
                reason,
                message,
            },
        }
    }

    fn invalid_config_path_utf8() -> Self {
        Self {
            outcome: "failed",
            started: false,
            defaults_substituted: false,
            origin: ConfigLoadOrigin::ExplicitPath.as_str(),
            error: StartupRefusalError {
                kind: "invalid_argument",
                path: None,
                reason: "invalid_utf8".to_string(),
                message: "explicit configuration path is not valid UTF-8".to_string(),
            },
        }
    }

    fn runtime_init_failed(error: &std::io::Error) -> Self {
        Self {
            outcome: "failed",
            started: false,
            defaults_substituted: false,
            origin: "runtime",
            error: StartupRefusalError {
                kind: "runtime_init_failed",
                path: None,
                reason: format!("runtime_init_failed: {error}"),
                message: format!("failed to create Tokio runtime: {error}"),
            },
        }
    }

    fn panic_refusal() -> Self {
        Self {
            outcome: "failed",
            started: false,
            defaults_substituted: false,
            origin: "runtime",
            error: StartupRefusalError {
                kind: "runtime_init_failed",
                path: None,
                reason: "panic: unexpected panic during plugin initialization".to_string(),
                message: "unexpected panic during plugin initialization".to_string(),
            },
        }
    }

    fn to_json_string(&self) -> String {
        serde_json::to_string(self).unwrap_or_else(|_| {
            r#"{"outcome":"failed","started":false,"defaults_substituted":false,"origin":"runtime","error":{"kind":"runtime_init_failed","path":null,"reason":"serialization_failed","message":"failed to serialize startup refusal"}}"#.to_string()
        })
    }
}

fn record_and_log_refusal(refusal: StartupRefusal) -> c_int {
    let json_line = refusal.to_json_string();
    *LAST_REFUSAL.lock().unwrap_or_else(|e| e.into_inner()) = Some(refusal);
    // Avoid `eprintln!`: a closed or unavailable stderr handle (common in GUI
    // hosts such as Windows KiCad) would panic across the `extern "C"` boundary.
    let _ = writeln!(std::io::stderr(), "kicad_plugin_init: {json_line}");
    -1
}

/// Return the structured startup refusal from the most recent failed
/// `kicad_plugin_init` call, or `None` if initialization has not failed (or
/// subsequently succeeded).
pub fn last_startup_refusal() -> Option<StartupRefusal> {
    LAST_REFUSAL
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .clone()
}

/// Initialize and start the embedded MCP server.
///
/// Returns:
/// - `1` when configuration loaded cleanly and the embedded server started.
/// - `0` when the embedded server was already initialized in this process.
/// - `-1` when startup was refused (invalid UTF-8 path, failed configuration
///   load, or runtime creation failure). On refusal, a structured JSON refusal
///   is written to stderr, recorded in [`last_startup_refusal`], no defaults
///   are substituted, and no server is started.
///
/// # Safety
/// `config_path` must be a valid null-terminated UTF-8 C string, or NULL to
/// resolve via the default search path (falling back to built-in defaults only
/// when no configuration file exists).
#[no_mangle]
pub unsafe extern "C" fn kicad_plugin_init(config_path: *const c_char) -> c_int {
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        kicad_plugin_init_inner(config_path)
    }))
    .unwrap_or_else(|_| record_and_log_refusal(StartupRefusal::panic_refusal()))
}

unsafe fn kicad_plugin_init_inner(config_path: *const c_char) -> c_int {
    let _init_guard = INIT_LOCK.lock().unwrap_or_else(|e| e.into_inner());

    if RUNTIME.get().is_some() {
        return 0;
    }

    let config_path = if config_path.is_null() {
        None
    } else {
        match CStr::from_ptr(config_path).to_str() {
            Ok(s) => Some(PathBuf::from(s)),
            Err(_) => return record_and_log_refusal(StartupRefusal::invalid_config_path_utf8()),
        }
    };

    // Load and validate configuration synchronously before creating the runtime
    // or mutating process-wide state. A failed explicit config or malformed
    // search-path config is a startup refusal — never substitute defaults.
    let (config, ipc_source, config_resolution) =
        match Config::load_resolved(config_path.as_deref()) {
            Ok(loaded) => loaded,
            Err(error) => {
                return record_and_log_refusal(StartupRefusal::from_config_error(&error));
            }
        };

    let rt = match tokio::runtime::Runtime::new() {
        Ok(rt) => rt,
        Err(error) => {
            return record_and_log_refusal(StartupRefusal::runtime_init_failed(&error));
        }
    };

    *LAST_REFUSAL.lock().unwrap_or_else(|e| e.into_inner()) = None;

    // `main.rs` installs a subscriber before reporting resolution. `try_init`
    // leaves a subscriber the host already installed alone, which is the case
    // that matters when KiCad loads this cdylib.
    let filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new(&config.log_level));
    let _ = tracing_subscriber::fmt::Subscriber::builder()
        .with_writer(std::io::stderr)
        .with_env_filter(filter)
        .try_init();
    ipc_source.log(&config.ipc_address);
    crate::config::log_config_resolution(&config_resolution);

    let server_config = config.apply_to_process();

    rt.spawn(async move {
        use copperpour_core::mcp::handler::McpHandler;

        let handler =
            McpHandler::new_with_config_resolution(server_config, config_resolution).await;
        if let (Ok(handler), true) = (&handler, ipc_source.rediscovers()) {
            handler.enable_ipc_rediscovery();
        }
        if let Ok(handler) = &handler {
            config.apply_to_handler(handler);
        }
        match handler {
            Ok(handler) => match config.transport {
                TransportMode::Stdio => {
                    // This server is embedded in the host process. Even on
                    // Unix stdio, current_exe() names that host (for example
                    // KiCad), not the Copperpour cdylib, so in-place reload is a
                    // standalone-binary capability and is not enabled here.
                    run_embedded_stdio(handler).await;
                }
                TransportMode::Http => {
                    let _ = crate::transport::http::run_http(handler, &config.http_address).await;
                }
                TransportMode::Both => {
                    let handler_http = handler.clone();
                    let http_addr = config.http_address.clone();
                    tokio::select! {
                        _ = crate::transport::http::run_http(handler_http, &http_addr) => {},
                        _ = run_embedded_stdio(handler) => {},
                    }
                }
            },
            Err(e) => {
                eprintln!("kicad_plugin_init: failed to create handler: {}", e);
            }
        }
    });

    RUNTIME.set(rt).is_ok() as c_int
}

async fn run_embedded_stdio(handler: copperpour_core::mcp::handler::McpHandler) {
    match crate::transport::stdio::run_stdio(handler).await {
        Ok(crate::transport::stdio::StdioExit::Eof) => {}
        #[cfg(unix)]
        Ok(crate::transport::stdio::StdioExit::Reload(plan)) => {
            // Defense in depth: embedded handlers never enable this request.
            // If that invariant regresses, consume and refuse the handoff
            // instead of replacing the KiCad host process.
            eprintln!(
                "kicad_plugin_init: refused an embedded reload request for {}",
                plan.binary_path.display()
            );
        }
        Err(error) => eprintln!("kicad_plugin_init: stdio transport failed: {error}"),
    }
}

/// Return the plugin version string.
///
/// # Safety
/// The returned pointer is valid for the lifetime of the process.
#[no_mangle]
pub unsafe extern "C" fn kicad_plugin_version() -> *const c_char {
    VERSION_CSTR
        .get_or_init(|| CString::new(env!("CARGO_PKG_VERSION")).expect("version string is valid"))
        .as_ptr()
}

/// Shut down the embedded MCP server.
#[no_mangle]
pub extern "C" fn kicad_plugin_shutdown() {
    // The runtime will be dropped here; tokio tasks are cancelled automatically.
    // In a more complete implementation we would signal the server to flush and close.
    let _ = RUNTIME.get();
}
