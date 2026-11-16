//! Starting a GUI application the server does not wait for: KiCad, from
//! `launch_kicad_ui` and the browser viewer's Open in KiCAD.
//!
//! A successful `spawn()` only means `exec` worked. An application that cannot
//! open a window, such as one started without a display on Linux, exits a
//! moment later, so the caller watches it before reporting a launch
//! (mixelpixx/Konnect#702, mixelpixx/Konnect#764, #496).

use std::collections::VecDeque;
use std::ffi::{OsStr, OsString};
use std::process::Stdio;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::io::AsyncBufReadExt;

/// How long an application must stay up before it counts as launched. One
/// that cannot open a window exits well inside this (mixelpixx/Konnect#702,
/// mixelpixx/Konnect#764).
pub const STARTUP_WINDOW: Duration = Duration::from_secs(1);

/// Stderr lines quoted when the application exits during startup, and the
/// length each is cut to, so a huge panic message cannot flood the tool result.
const STDERR_TAIL_LINES: usize = 20;
const STDERR_LINE_CHARS: usize = 500;

/// How long to wait for the rest of a dead application's stderr. A process it
/// started can hold the pipe open after it exits.
const STDERR_DRAIN: Duration = Duration::from_millis(500);

/// Why a GUI started with this environment would find no display, or `None`.
/// `env` reads the started program's environment (Copperpour#225).
pub fn no_display_reason(env: impl Fn(&str) -> Option<OsString>) -> Option<&'static str> {
    if cfg!(any(target_os = "windows", target_os = "macos")) {
        return None;
    }
    let set = |name| env(name).is_some_and(|value| !value.is_empty());
    (!set("DISPLAY") && !set("WAYLAND_DISPLAY")).then_some(
        "Neither DISPLAY nor WAYLAND_DISPLAY is set in the server's environment, as when it \
         runs from an SSH session.",
    )
}

/// The effective display configuration, including overrides on the child.
fn command_no_display_reason(cmd: &std::process::Command) -> Option<&'static str> {
    no_display_reason(
        |name| match cmd.get_envs().find(|(key, _)| *key == OsStr::new(name)) {
            Some((_, value)) => value.map(OsStr::to_os_string),
            None => std::env::var_os(name),
        },
    )
}

/// The Linux viewer tries the local X display only without a configured display.
/// Returns whether the fallback was applied; the parent environment is untouched.
pub(crate) fn default_viewer_display(cmd: &mut std::process::Command) -> bool {
    let fallback = cfg!(target_os = "linux") && command_no_display_reason(cmd).is_some();
    if fallback {
        cmd.env("DISPLAY", ":0");
    }
    fallback
}

/// A spawned application, watched until the caller decides it is up.
pub struct Launch {
    name: &'static str,
    /// [`no_display_reason`] for the environment it was started with.
    no_display: Option<&'static str>,
    /// What the caller's user can do about `no_display`.
    no_display_remedy: Option<&'static str>,
    child: tokio::process::Child,
    tail: Arc<Mutex<VecDeque<String>>>,
    forwarder: Option<tokio::task::JoinHandle<()>>,
}

/// Spawns `cmd` as `name`, sending each stderr line to `log`.
///
/// The application gets neither the caller's stdin nor its stdout, which may
/// carry the MCP server's stdio transport. Not `kill_on_drop`: it outlives the
/// call.
pub fn spawn(
    mut cmd: std::process::Command,
    name: &'static str,
    log: fn(&str),
) -> std::io::Result<Launch> {
    let no_display = command_no_display_reason(&cmd);
    cmd.stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped());
    let mut child = tokio::process::Command::from(cmd).spawn()?;

    let tail = Arc::new(Mutex::new(VecDeque::new()));
    let forwarder = child.stderr.take().map(|stderr| {
        let tail = Arc::clone(&tail);
        tokio::spawn(async move {
            let mut lines = tokio::io::BufReader::new(stderr).split(b'\n');
            while let Ok(Some(line)) = lines.next_segment().await {
                let line = String::from_utf8_lossy(&line);
                let line = line.trim_end();
                if line.is_empty() {
                    continue;
                }
                log(line);
                // Once the caller has returned, nobody reads the tail.
                if Arc::strong_count(&tail) == 1 {
                    continue;
                }
                if let Ok(mut tail) = tail.lock() {
                    if tail.len() == STDERR_TAIL_LINES {
                        tail.pop_front();
                    }
                    tail.push_back(line.chars().take(STDERR_LINE_CHARS).collect::<String>());
                }
            }
        })
    });

    Ok(Launch {
        name,
        no_display,
        no_display_remedy: None,
        child,
        tail,
        forwarder,
    })
}

impl Launch {
    /// Whether it was started with no display, so [`Self::exited`] says so.
    pub fn lacks_display(&self) -> bool {
        self.no_display.is_some()
    }

    /// A sentence [`Self::exited`] adds after saying the display is missing.
    pub fn on_no_display(&mut self, remedy: &'static str) {
        self.no_display_remedy = Some(remedy);
    }

    /// Waits up to `window` for the application to exit. `Ok` means it is
    /// still running; an exit is reported with its status and stderr tail.
    pub async fn watch(&mut self, window: Duration) -> Result<(), String> {
        match tokio::time::timeout(window, self.exited()).await {
            Err(_still_running) => Ok(()),
            Ok(message) => Err(message),
        }
    }

    /// Waits for the application to exit and describes the exit.
    pub async fn exited(&mut self) -> String {
        let status = match self.child.wait().await {
            Ok(status) => status,
            Err(e) => return format!("Failed to check the {} process: {e}", self.name),
        };
        if let Some(forwarder) = self.forwarder.take() {
            let _ = tokio::time::timeout(STDERR_DRAIN, forwarder).await;
        }
        let tail = self
            .tail
            .lock()
            .map(|mut tail| tail.make_contiguous().join("\n"))
            .unwrap_or_default();
        let name = self.name;
        let mut message = if tail.is_empty() {
            format!("{name} exited during startup ({status}) with no stderr output.")
        } else {
            format!("{name} exited during startup ({status}). Its stderr ended with:\n{tail}")
        };
        if let Some(reason) = self.no_display {
            message.push('\n');
            message.push_str(reason);
            if let Some(remedy) = self.no_display_remedy {
                message.push(' ');
                message.push_str(remedy);
            }
        }
        message
    }
}

/// `sh` stands in for the application, so these run on Unix only.
#[cfg(all(test, unix))]
mod tests {
    use super::*;

    /// On a display, so only the test that drops it gets the no-display line.
    fn sh(script: &str) -> std::process::Command {
        let mut cmd = std::process::Command::new("sh");
        cmd.arg("-c").arg(script).env("DISPLAY", ":0");
        cmd
    }

    /// Spawns `script` and watches it for long enough that any exit counts.
    async fn launch(script: &str) -> Result<(), String> {
        spawn(sh(script), "Test app", |_| {})
            .expect("sh spawns")
            .watch(Duration::from_secs(10))
            .await
    }

    /// The panic tao prints when GTK cannot open a display, then the exit
    /// code a Rust panic gives, as mixelpixx/Konnect#702's reproduction recorded
    /// them.
    #[tokio::test]
    async fn an_app_that_crashes_on_startup_is_a_failed_launch() {
        let err =
            launch("echo 'starting' >&2; echo 'Failed to initialize gtk backend!' >&2; exit 101")
                .await
                .expect_err("an exit inside the window must not count as launched");
        assert!(err.starts_with("Test app exited during startup"), "{err}");
        assert!(err.contains("101"), "exit status missing: {err}");
        assert!(
            err.contains("Failed to initialize gtk backend!"),
            "stderr missing: {err}"
        );
    }

    /// The server's environment does not matter: `cmd`'s own wins.
    #[tokio::test]
    async fn an_exit_with_no_display_says_so() {
        let mut cmd = sh("echo 'cannot open display' >&2; exit 1");
        cmd.env("DISPLAY", "").env_remove("WAYLAND_DISPLAY");
        let mut app = spawn(cmd, "Test app", |_| {}).expect("sh spawns");
        let missing = !cfg!(target_os = "macos");
        assert_eq!(app.lacks_display(), missing);
        app.on_no_display("Try a display.");
        let err = app.watch(Duration::from_secs(10)).await.unwrap_err();
        assert!(err.contains("cannot open display"), "{err}");
        assert_eq!(
            err.ends_with("Neither DISPLAY nor WAYLAND_DISPLAY is set in the server's environment, as when it runs from an SSH session. Try a display."),
            missing,
            "{err}"
        );
        // A display set in `cmd` is not a missing one.
        assert!(!spawn(sh("exit 1"), "Test app", |_| {})
            .unwrap()
            .lacks_display());
    }

    #[test]
    fn no_display_is_named_only_when_both_variables_are_unset_or_empty() {
        let env = |vars: &'static [(&'static str, &'static str)]| {
            move |name: &str| {
                vars.iter()
                    .find(|(key, _)| *key == name)
                    .map(|(_, value)| OsString::from(value))
            }
        };
        let missing = !cfg!(target_os = "macos");
        assert_eq!(no_display_reason(env(&[])).is_some(), missing);
        assert_eq!(
            no_display_reason(env(&[("DISPLAY", ""), ("WAYLAND_DISPLAY", "")])).is_some(),
            missing
        );
        assert!(no_display_reason(env(&[("DISPLAY", ":0")])).is_none());
        assert!(no_display_reason(env(&[("WAYLAND_DISPLAY", "wayland-0")])).is_none());
    }

    #[tokio::test]
    async fn a_clean_exit_during_startup_is_still_a_failed_launch() {
        let err = launch("exit 0")
            .await
            .expect_err("an app that exits shows no window");
        assert!(err.contains("no stderr output"), "{err}");
    }

    #[tokio::test]
    async fn only_the_tail_of_a_long_stderr_is_quoted() {
        let err = launch("i=1; while [ $i -le 30 ]; do echo line$i >&2; i=$((i+1)); done; exit 1")
            .await
            .unwrap_err();
        assert!(err.contains("line11\n") && err.ends_with("line30"), "{err}");
        assert!(!err.contains("line10\n"), "{err}");
    }

    #[tokio::test]
    async fn a_very_long_stderr_line_is_cut() {
        let err = launch("printf '%2000s\\n' '' | tr ' ' a >&2; exit 1")
            .await
            .unwrap_err();
        assert!(err.ends_with(&"a".repeat(500)), "{err}");
        assert!(!err.contains(&"a".repeat(501)), "{err}");
    }

    /// A process the application started can still be writing after the
    /// application itself has exited.
    #[tokio::test]
    async fn stderr_from_a_child_the_app_started_is_still_quoted() {
        let err = launch("(sleep 0.2; echo 'render failed' >&2) & exit 1")
            .await
            .unwrap_err();
        assert!(err.ends_with("render failed"), "{err}");
    }

    #[test]
    fn viewer_display_fallback_preserves_configured_displays() {
        for (display, wayland) in [(":42", ""), ("", "wayland-7"), (":42", "wayland-7")] {
            let mut cmd = sh("printf '%s|%s' \"$DISPLAY\" \"$WAYLAND_DISPLAY\"");
            cmd.env("DISPLAY", display).env("WAYLAND_DISPLAY", wayland);
            assert!(!default_viewer_display(&mut cmd));
            let output = cmd.output().unwrap();
            assert!(output.status.success());
            assert_eq!(
                String::from_utf8(output.stdout).unwrap(),
                format!("{display}|{wayland}")
            );
        }
    }

    #[test]
    fn viewer_display_fallback_is_child_only_and_preserves_authorization() {
        let before = (
            std::env::var_os("DISPLAY"),
            std::env::var_os("WAYLAND_DISPLAY"),
        );
        for empty in [false, true] {
            let mut cmd = sh("printf '%s|%s|%s|%s' \"$DISPLAY\" \"$WAYLAND_DISPLAY\" \"$XAUTHORITY\" \"$XDG_RUNTIME_DIR\"");
            cmd.env_remove("DISPLAY")
                .env_remove("WAYLAND_DISPLAY")
                .env("XAUTHORITY", "/tmp/test-xauth")
                .env("XDG_RUNTIME_DIR", "/tmp/test-runtime");
            if empty {
                cmd.env("DISPLAY", "").env("WAYLAND_DISPLAY", "");
            }
            assert_eq!(default_viewer_display(&mut cmd), cfg!(target_os = "linux"));
            let output = cmd.output().unwrap();
            assert!(output.status.success());
            let expected = if cfg!(target_os = "linux") {
                ":0||/tmp/test-xauth|/tmp/test-runtime"
            } else {
                "||/tmp/test-xauth|/tmp/test-runtime"
            };
            assert_eq!(String::from_utf8(output.stdout).unwrap(), expected);
            assert_eq!(
                (
                    std::env::var_os("DISPLAY"),
                    std::env::var_os("WAYLAND_DISPLAY")
                ),
                before
            );
        }
    }

    /// stdin and stdout are the stdio transport's JSON-RPC stream.
    #[tokio::test]
    async fn the_app_does_not_hold_the_servers_stdin_or_stdout() {
        let err = launch(
            "[ /dev/fd/0 -ef /dev/null ] && [ /dev/fd/1 -ef /dev/null ] \\
             && echo detached >&2; exit 1",
        )
        .await
        .unwrap_err();
        assert!(err.ends_with("detached"), "{err}");
    }

    #[tokio::test]
    async fn an_app_still_running_after_the_window_is_launched() {
        spawn(sh("sleep 3"), "Test app", |_| {})
            .expect("sh spawns")
            .watch(Duration::from_millis(200))
            .await
            .expect("an app that outlives the window is up");
    }

    /// The IPC wait watches in short windows; an exit between two of them
    /// must still be seen.
    #[tokio::test]
    async fn an_exit_after_an_earlier_window_is_still_reported() {
        let mut app =
            spawn(sh("sleep 0.3; echo gone >&2; exit 3"), "Test app", |_| {}).expect("sh spawns");
        app.watch(Duration::from_millis(50))
            .await
            .expect("still starting");
        let err = app.watch(Duration::from_secs(10)).await.unwrap_err();
        assert!(err.contains('3') && err.ends_with("gone"), "{err}");
    }
}
