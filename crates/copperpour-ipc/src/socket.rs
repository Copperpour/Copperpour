//! Where KiCad's IPC socket lives when the environment does not say.
//!
//! KiCad exports `KICAD_API_SOCKET` only to plugins it launches itself, so a
//! standalone server started by an MCP client sees nothing and every IPC call
//! fails as unconfigured. KiCad's own default is predictable —
//! `<temp dir>/kicad/api.sock` — so look there before giving up.
//!
//! Looking is all this module does, and it never opens a connection: what
//! sits at that path is KiCad's NNG endpoint, and dialling it outside the NNG
//! protocol is what wedged the editor in #498. Whether the endpoint answers is
//! decided later, by the bounded NNG `Ping` in [`crate::client`].
//!
//! Where "looking" happens is per-platform, because the endpoint is. On Unix
//! the socket is a filesystem entry and this reads its metadata. On Windows
//! NNG maps `ipc://` to a **named pipe**, which has no filesystem presence at
//! all, so the same path is looked for in the pipe namespace instead (#529).

use std::path::{Path, PathBuf};

/// Socket paths KiCad may have created on this platform, most likely first.
pub fn candidate_socket_paths() -> Vec<PathBuf> {
    candidates_in(&std::env::temp_dir())
}

fn candidates_in(temp_dir: &Path) -> Vec<PathBuf> {
    let mut candidates = vec![temp_dir.join("kicad").join("api.sock")];
    // macOS resolves the temp dir under /var/folders/…, but KiCad has been
    // seen on /tmp there; try both rather than betting on one. Only there:
    // /tmp is shared and world-writable, and on Linux the temp dir already
    // *is* /tmp unless TMPDIR says otherwise, so the fallback would buy no
    // coverage while letting any local user pre-bind the path we adopt.
    if cfg!(target_os = "macos") {
        let shared_tmp = PathBuf::from("/tmp/kicad/api.sock");
        if !candidates.contains(&shared_tmp) {
            candidates.push(shared_tmp);
        }
    }
    candidates
}

/// The IPC address to use when `KICAD_API_SOCKET` is unset, or `None` when
/// this platform's default path holds nothing this user could talk to.
///
/// Returning `None` rather than a guess keeps the "socket path not configured"
/// guidance in place instead of replacing it with a dial failure against an
/// address nobody chose.
///
/// What is returned is a *candidate*, not a proven endpoint. KiCad does not
/// unlink `api.sock` when it exits, so a socket file left by a finished
/// session is indistinguishable from a live one by metadata alone, and this
/// function will return it. The `Ping` that follows then fails within its own
/// bound and the session falls back to file editing — the same outcome as a
/// KiCad that was never running, reached without touching the editor. Proving
/// liveness here instead would mean a second, hand-written handshake against
/// the socket, which is exactly what #498 is.
pub fn detect_ipc_address() -> Option<String> {
    #[cfg(unix)]
    {
        let candidates = candidate_socket_paths();
        let fallbacks = pid_socket_fallbacks(&candidates, pid_is_alive);
        detect_with_fallbacks(&candidates, &fallbacks, is_adoptable, modified)
    }
    #[cfg(not(unix))]
    {
        detect_ipc_address_in(&candidate_socket_paths(), is_adoptable)
    }
}

/// `api-<pid>.sock` beside each candidate whose `<pid>` is a running process.
///
/// KiCad 10 binds `api.sock` only while it holds `api.lock`. A lock left by a
/// session that did not exit cleanly sends the next KiCad to
/// `api-<pid>.sock` instead (#374), so a server that only looked at `api.sock`
/// adopted the dead socket and reported "Connection refused" while KiCad was
/// listening next to it. A socket whose process is gone is left out: KiCad
/// does not unlink these either.
#[cfg(unix)]
fn pid_socket_fallbacks(candidates: &[PathBuf], is_alive: impl Fn(i32) -> bool) -> Vec<PathBuf> {
    let mut found = Vec::new();
    for candidate in candidates {
        let Some(dir) = candidate.parent() else {
            continue;
        };
        let Ok(entries) = std::fs::read_dir(dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let name = entry.file_name();
            let Some(pid) = name
                .to_str()
                .and_then(|name| name.strip_prefix("api-"))
                .and_then(|rest| rest.strip_suffix(".sock"))
                .and_then(|digits| digits.parse::<i32>().ok())
            else {
                continue;
            };
            if pid > 0 && is_alive(pid) {
                found.push(entry.path());
            }
        }
    }
    found.sort();
    found
}

/// Whether a process with this id exists. Signal 0 checks without sending
/// anything; `EPERM` means it exists under another account.
#[cfg(unix)]
fn pid_is_alive(pid: i32) -> bool {
    // SAFETY: kill with signal 0 performs only the existence and permission
    // checks and touches no memory.
    let result = unsafe { libc::kill(pid, 0) };
    result == 0 || std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
}

#[cfg(unix)]
fn modified(path: &Path) -> Option<std::time::SystemTime> {
    std::fs::metadata(path)
        .and_then(|meta| meta.modified())
        .ok()
}

/// The first adoptable candidate, unless an adoptable `api-<pid>.sock` was
/// bound after it: then the newest of those, since the KiCad that made it
/// started after whatever left `api.sock` behind.
#[cfg(unix)]
fn detect_with_fallbacks(
    candidates: &[PathBuf],
    fallbacks: &[PathBuf],
    is_adoptable: impl Fn(&Path) -> bool,
    modified: impl Fn(&Path) -> Option<std::time::SystemTime>,
) -> Option<String> {
    let primary = candidates.iter().find(|path| is_adoptable(path));
    let newest_fallback = fallbacks
        .iter()
        .filter(|path| is_adoptable(path))
        .max_by_key(|path| modified(path));
    let chosen = match (primary, newest_fallback) {
        (Some(primary), Some(fallback)) if modified(fallback) > modified(primary) => fallback,
        (Some(primary), _) => primary,
        (None, Some(fallback)) => fallback,
        (None, None) => return None,
    };
    Some(format_address(chosen))
}

// On Unix `detect_with_fallbacks` does this and more; the tests keep it.
#[cfg(any(not(unix), test))]
fn detect_ipc_address_in(
    candidates: &[PathBuf],
    is_adoptable: impl Fn(&Path) -> bool,
) -> Option<String> {
    candidates
        .iter()
        .find(|path| is_adoptable(path))
        .map(|path| format_address(path))
}

/// Whether this socket belongs to the user running Copperpour.
///
/// A detected socket is adopted as the board endpoint unread, so a path
/// another account can create is a path another account can be handed the
/// board through. The shared-/tmp candidate is the one that matters: its
/// directory is world-writable, so ownership is what separates KiCad's socket
/// from a squatter's.
#[cfg(unix)]
fn is_owned_by_us(path: &Path) -> bool {
    use std::os::unix::fs::MetadataExt;

    // SAFETY: geteuid() is always successful and touches no memory.
    let euid = unsafe { libc::geteuid() };
    std::fs::metadata(path).is_ok_and(|meta| meta.uid() == euid)
}

/// Whether a candidate path may be adopted as the board endpoint.
///
/// Metadata only, and deliberately so. Existence alone is not enough — KiCad
/// leaves `api.sock` behind when it exits, and a directory anyone can write to
/// is a directory anyone can drop a socket into — so this asks the filesystem
/// three things it already knows: the path exists, it is a socket rather than
/// some other file, and it belongs to this user.
///
/// What it must not do is connect. KiCad's API server is an NNG `REP` socket,
/// and a raw `AF_UNIX` stream that connects and disappears without completing
/// NNG's handshake leaves that server unable to answer *any* client, KiCad's
/// own `kipy` included, until the editor is restarted (#498). Liveness is the
/// job of the bounded `Ping` in [`crate::client`], which speaks the protocol
/// the endpoint expects and is the only handshake this crate performs.
#[cfg(unix)]
fn is_adoptable(path: &Path) -> bool {
    is_adoptable_owned_by(path, is_owned_by_us)
}

/// [`is_adoptable`] with the ownership rule supplied, so a test can prove the
/// gate refuses a socket that is genuinely live. Faking the *owner* is the
/// only way to do that without a second account, and the alternative — trusting
/// that a guard nothing exercises still holds — is how a guard stops holding.
#[cfg(unix)]
fn is_adoptable_owned_by(path: &Path, is_ours: impl Fn(&Path) -> bool) -> bool {
    use std::os::unix::fs::FileTypeExt;

    if !is_ours(path) {
        return false;
    }
    std::fs::metadata(path).is_ok_and(|meta| meta.file_type().is_socket())
}

/// The Windows pipe namespace. NNG derives a pipe name by prefixing the
/// `ipc://` path with this, verbatim and separators intact, so the candidate
/// list needs no new entries — only a different place to look them up.
#[cfg(windows)]
const PIPE_NAMESPACE: &str = r"\\.\pipe";

/// Whether a candidate path may be adopted as the board endpoint.
///
/// On Windows the endpoint is not a file. NNG maps `ipc://<path>` to the named
/// pipe `\\.\pipe\<path>`, and with KiCad running the path itself does not
/// exist while that pipe does. Probing the path therefore answered "no" on
/// every Windows install, and a client-launched Copperpour — the shipped PCM
/// package registered with an MCP client, with no `ipc_address` and no
/// `KICAD_API_SOCKET` — took the file fallback on every hybrid tool while
/// KiCad held the board open. That is the one outcome `attempt_ipc_write`'s
/// gate exists to prevent, reached because discovery could not see a
/// reachable transport (#529).
///
/// Enumerating the namespace is the whole check. `FindFirstFileW` over
/// `\\.\pipe` lists names without opening any of them, which keeps the
/// metadata-only rule #498 made non-negotiable — and note that
/// `std::fs::metadata` is *not* metadata-only here: on Windows it opens a
/// handle, which against KiCad's endpoint would consume a server instance.
///
/// Matching is case-insensitive because Windows paths are, and the name KiCad
/// derives comes from its own spelling of the temp directory rather than ours.
///
/// One limit, stated rather than papered over: the pipe namespace is
/// machine-global and any local account may create a name in it, so another
/// user could bind this name before KiCad does and be adopted. Unix answers
/// that with an ownership check; the Windows equivalent needs the pipe's
/// security descriptor, which means opening it. An explicitly configured
/// `ipc_address` has always carried the same exposure, so this is the state of
/// the art on this platform rather than something detection introduces.
#[cfg(windows)]
fn is_adoptable(path: &Path) -> bool {
    is_adoptable_among(path, pipe_names)
}

/// Every name currently bound in the pipe namespace, or nothing when it cannot
/// be read. `file_name()` returns the bound name verbatim, backslashes and
/// drive letter included, which is exactly the candidate path to compare with.
#[cfg(windows)]
fn pipe_names() -> Vec<std::ffi::OsString> {
    std::fs::read_dir(PIPE_NAMESPACE)
        .map(|entries| entries.flatten().map(|entry| entry.file_name()).collect())
        .unwrap_or_default()
}

/// [`is_adoptable`] with the namespace listing supplied, so a test can state
/// what is bound instead of binding it.
#[cfg(windows)]
fn is_adoptable_among(path: &Path, names: impl Fn() -> Vec<std::ffi::OsString>) -> bool {
    let wanted = path.as_os_str().to_string_lossy().to_lowercase();
    names()
        .iter()
        .any(|name| name.to_string_lossy().to_lowercase() == wanted)
}

/// Neither a Unix socket nor a Windows pipe to look for, so nothing is
/// adopted and `IpcAddressSource::Unresolved` keeps its guidance.
#[cfg(not(any(unix, windows)))]
fn is_adoptable(_path: &Path) -> bool {
    false
}

/// Format a socket path the way KiCad prints it, so a detected address and a
/// pasted one are the same string.
fn format_address(path: &Path) -> String {
    format!("ipc://{}", path.display())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn candidates_start_with_the_temp_dir_socket() {
        let candidates = candidates_in(Path::new("/somewhere/tmp"));
        assert_eq!(
            candidates[0],
            PathBuf::from("/somewhere/tmp/kicad/api.sock")
        );
    }

    #[test]
    #[cfg(target_os = "macos")]
    fn shared_tmp_is_a_fallback_candidate_and_is_not_duplicated() {
        let candidates = candidates_in(Path::new("/var/folders/ab/T"));
        assert_eq!(candidates.len(), 2);
        assert_eq!(candidates[1], PathBuf::from("/tmp/kicad/api.sock"));

        let candidates = candidates_in(Path::new("/tmp"));
        assert_eq!(candidates.len(), 1);
    }

    #[test]
    #[cfg(all(unix, not(target_os = "macos")))]
    fn the_shared_tmp_fallback_is_macos_only() {
        // On Linux the temp dir already is /tmp unless TMPDIR redirects it, so
        // the fallback adds no coverage — only a world-writable path anyone
        // could have bound first.
        let candidates = candidates_in(Path::new("/somewhere/else"));
        assert_eq!(
            candidates,
            vec![PathBuf::from("/somewhere/else/kicad/api.sock")]
        );
    }

    #[test]
    #[cfg(unix)]
    fn ownership_gates_the_probe() {
        // Ownership is decided before the connect, so a path that cannot be
        // stat'd is refused rather than dialled.
        let dir = tempfile::tempdir().unwrap();
        assert!(!is_owned_by_us(&dir.path().join("absent.sock")));

        let path = dir.path().join("api.sock");
        let _listener = std::os::unix::net::UnixListener::bind(&path).unwrap();
        assert!(is_owned_by_us(&path));
    }

    /// A genuinely foreign-owned path, with no second account to create one.
    ///
    /// Every Unix ships files this user does not own, and `/etc/passwd` is
    /// root's on Linux and macOS alike. It is also not a socket, so this test
    /// alone would pass on either gate; `an_owned_regular_file_is_not_adopted`
    /// is what separates them, by owning its file.
    #[test]
    #[cfg(unix)]
    fn a_foreign_owned_path_is_refused() {
        // SAFETY: geteuid() is always successful and touches no memory.
        if unsafe { libc::geteuid() } == 0 {
            // root owns it, so there is nothing foreign to test against.
            return;
        }
        let foreign = Path::new("/etc/passwd");
        assert!(
            foreign.exists() && !is_owned_by_us(foreign),
            "/etc/passwd must exist and belong to another account"
        );
        assert!(
            !is_adoptable(foreign),
            "a path this user does not own must never be adopted as the board endpoint"
        );
    }

    /// The ownership gate is load-bearing over a socket that would otherwise
    /// pass every other check: a real, listening endpoint whose owner fails it
    /// is still refused. This is the shared-`/tmp` squatter, without a second
    /// account.
    #[test]
    #[cfg(unix)]
    fn a_live_socket_that_fails_the_ownership_check_is_not_adopted() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("api.sock");
        let _listener = std::os::unix::net::UnixListener::bind(&path).unwrap();
        assert!(is_adoptable(&path), "sanity: this socket is ours");

        assert!(
            !is_adoptable_owned_by(&path, |_| false),
            "a live listener must not be adopted when ownership does not check out"
        );
    }

    #[test]
    #[cfg(unix)]
    fn an_owned_regular_file_is_not_adopted() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("not-a-socket");
        std::fs::write(&path, b"regular file").unwrap();

        assert!(is_owned_by_us(&path), "sanity: this process owns the file");
        assert!(
            !is_adoptable(&path),
            "an owned regular file must not be adopted as an IPC socket"
        );
    }

    /// Discovery must never open a stream connection to a candidate.
    ///
    /// KiCad's API server is an NNG REP endpoint. A raw `AF_UNIX` connect that
    /// never completes NNG's handshake leaves that server unable to answer any
    /// client afterwards, KiCad's own `kipy` included, until the editor is
    /// restarted (#498). The probe therefore has to answer from metadata
    /// alone, and the place to prove it is the listener's side: once detection
    /// has run, nothing may be waiting in its accept queue.
    #[test]
    #[cfg(unix)]
    fn detection_never_connects_to_a_candidate() {
        use std::io::ErrorKind;
        use std::os::unix::net::UnixListener;

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("api.sock");
        let listener = UnixListener::bind(&path).unwrap();
        listener.set_nonblocking(true).unwrap();

        let candidates = vec![path.clone()];
        let detected = detect_ipc_address_in(&candidates, is_adoptable);
        assert_eq!(
            detected,
            Some(format_address(&path)),
            "sanity: this socket is ours and is the only candidate"
        );

        match listener.accept() {
            Err(error) if error.kind() == ErrorKind::WouldBlock => {}
            Ok(_) => panic!(
                "detection opened a stream connection to the candidate; against KiCad's \
                 NNG endpoint that is the wedge in #498"
            ),
            Err(error) => panic!("unexpected accept error: {error}"),
        }
    }

    #[test]
    fn discovery_picks_the_first_adoptable_candidate() {
        let candidates = vec![
            PathBuf::from("/first/kicad/api.sock"),
            PathBuf::from("/second/kicad/api.sock"),
        ];
        let detected = detect_ipc_address_in(&candidates, |path| path.starts_with("/second"));
        assert_eq!(detected.as_deref(), Some("ipc:///second/kicad/api.sock"));
    }

    #[test]
    fn discovery_finds_nothing_when_no_candidate_is_adoptable() {
        let candidates = vec![PathBuf::from("/first/kicad/api.sock")];
        assert!(detect_ipc_address_in(&candidates, |_| false).is_none());
    }

    /// The cost of not connecting, asserted rather than left to be discovered.
    ///
    /// KiCad does not unlink `api.sock` on exit, and a socket file with no
    /// listener is byte-for-byte the same metadata as one with a listener, so
    /// discovery adopts it. That address then fails its `Ping` within the
    /// bound and the session falls back to file editing — the outcome a
    /// not-running KiCad produces anyway. Rejecting it here would cost a
    /// connect, and the connect is the bug.
    #[test]
    #[cfg(unix)]
    fn a_socket_left_behind_by_a_closed_kicad_is_still_adopted() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("api.sock");

        let listener = std::os::unix::net::UnixListener::bind(&path).unwrap();
        assert!(is_adoptable(&path), "sanity: a bound socket is adoptable");

        drop(listener);
        assert!(path.exists(), "sanity: the file outlives the listener");
        assert!(
            is_adoptable(&path),
            "metadata cannot tell a stale socket from a live one, and this \
             check does not connect to find out"
        );
    }

    /// The candidate is adopted when a pipe carries its name — the case that
    /// answered "no" for every Windows install before #529.
    #[test]
    #[cfg(windows)]
    fn a_pipe_named_for_the_candidate_is_adopted() {
        let path = Path::new(r"C:\Users\someone\AppData\Local\Temp\kicad\api.sock");
        let bound = vec![
            std::ffi::OsString::from(r"some-other-pipe"),
            std::ffi::OsString::from(path),
        ];

        assert!(is_adoptable_among(path, || bound.clone()));
    }

    #[test]
    #[cfg(windows)]
    fn a_candidate_with_no_pipe_is_not_adopted() {
        let path = Path::new(r"C:\Users\someone\AppData\Local\Temp\kicad\api.sock");
        let bound = vec![std::ffi::OsString::from(r"C:\elsewhere\kicad\api.sock")];

        assert!(!is_adoptable_among(path, || bound.clone()));
        // An unreadable namespace lists nothing and adopts nothing, which is
        // the same answer as a KiCad that is not running.
        assert!(!is_adoptable_among(path, Vec::new));
    }

    /// Windows paths are case-insensitive, and the bound name is KiCad's
    /// spelling of the temp directory rather than ours.
    #[test]
    #[cfg(windows)]
    fn pipe_matching_ignores_case_as_windows_paths_do() {
        let path = Path::new(r"C:\Users\Someone\AppData\Local\Temp\kicad\api.sock");
        let bound = vec![std::ffi::OsString::from(
            r"c:\users\someone\appdata\local\temp\kicad\API.SOCK",
        )];

        assert!(is_adoptable_among(path, || bound.clone()));
    }

    /// The Windows counterpart of `detection_never_connects_to_a_candidate`,
    /// against a real pipe server: discovery must find it by name and must not
    /// become a client of it. A connect that never completes NNG's handshake
    /// is what wedged KiCad in #498, and on Windows it would also consume a
    /// server instance of the endpoint.
    ///
    /// Note the pipe name embeds a path that does not exist on disk. That is
    /// the whole point: the endpoint is not a filesystem object, so a
    /// filesystem probe can never see it.
    #[tokio::test]
    #[cfg(windows)]
    async fn detection_finds_a_live_pipe_without_connecting_to_it() {
        use tokio::net::windows::named_pipe::ServerOptions;

        let path = std::env::temp_dir()
            .join(format!("konnect-529-{}", std::process::id()))
            .join("kicad")
            .join("api.sock");
        let pipe_name = format!(r"{PIPE_NAMESPACE}\{}", path.display());
        let server = ServerOptions::new()
            .first_pipe_instance(true)
            .create(&pipe_name)
            .expect("bind the pipe KiCad's endpoint would bind");
        assert!(!path.exists(), "sanity: the endpoint is not a file");

        let detected = detect_ipc_address_in(std::slice::from_ref(&path), is_adoptable);
        assert_eq!(
            detected,
            Some(format_address(&path)),
            "a bound pipe carrying the candidate's name must be detected"
        );

        // Nothing connected: the server is still waiting for its first client.
        let connected =
            tokio::time::timeout(std::time::Duration::from_millis(100), server.connect()).await;
        assert!(
            connected.is_err(),
            "detection became a client of the endpoint; against KiCad's NNG server \
             that is the wedge in #498"
        );
    }

    /// With KiCad running, detection works with no configuration at all —
    /// the property #529 is about, and the one no offline test can assert.
    ///
    ///     cargo test -p copperpour-ipc --lib live_kicad -- --ignored --nocapture
    #[test]
    #[cfg(windows)]
    #[ignore = "needs a running KiCad with the API server enabled"]
    fn live_kicad_is_discovered_with_no_configuration() {
        let detected = detect_ipc_address();
        assert!(
            detected.is_some(),
            "no KiCad endpoint discovered; candidates were {:?}",
            candidate_socket_paths()
        );
        let address = detected.unwrap();
        assert!(address.starts_with("ipc://"), "{address}");
        println!("discovered {address}");
    }

    #[test]
    fn detected_address_carries_the_ipc_scheme_kicad_prints() {
        assert_eq!(
            format_address(Path::new("/tmp/kicad/api.sock")),
            "ipc:///tmp/kicad/api.sock"
        );
    }

    #[cfg(unix)]
    mod pid_sockets {
        use super::super::*;
        use std::time::{Duration, SystemTime};

        fn touch(dir: &Path, name: &str) -> PathBuf {
            let path = dir.join(name);
            std::fs::write(&path, b"").unwrap();
            path
        }

        #[test]
        fn only_live_pid_sockets_are_fallbacks() {
            let dir = tempfile::tempdir().unwrap();
            let primary = dir.path().join("api.sock");
            touch(dir.path(), "api-100.sock");
            touch(dir.path(), "api-200.sock");
            touch(dir.path(), "api-x.sock");
            touch(dir.path(), "api-300.lock");
            let found = pid_socket_fallbacks(&[primary], |pid| pid == 200);
            assert_eq!(found, vec![dir.path().join("api-200.sock")]);
        }

        #[test]
        fn a_newer_pid_socket_wins_over_a_stale_api_sock() {
            let primary = PathBuf::from("/k/api.sock");
            let fallback = PathBuf::from("/k/api-42.sock");
            let at = |path: &Path| {
                let base = SystemTime::UNIX_EPOCH;
                Some(if path == Path::new("/k/api.sock") {
                    base + Duration::from_secs(10)
                } else {
                    base + Duration::from_secs(20)
                })
            };
            let chosen = detect_with_fallbacks(
                std::slice::from_ref(&primary),
                std::slice::from_ref(&fallback),
                |_| true,
                at,
            );
            assert_eq!(chosen.as_deref(), Some("ipc:///k/api-42.sock"));
        }

        #[test]
        fn api_sock_stays_first_when_it_is_the_newest() {
            let primary = PathBuf::from("/k/api.sock");
            let fallback = PathBuf::from("/k/api-42.sock");
            let at = |path: &Path| {
                let base = SystemTime::UNIX_EPOCH;
                Some(if path == Path::new("/k/api.sock") {
                    base + Duration::from_secs(30)
                } else {
                    base + Duration::from_secs(20)
                })
            };
            let chosen = detect_with_fallbacks(
                std::slice::from_ref(&primary),
                std::slice::from_ref(&fallback),
                |_| true,
                at,
            );
            assert_eq!(chosen.as_deref(), Some("ipc:///k/api.sock"));
        }

        #[test]
        fn a_pid_socket_is_used_when_api_sock_is_absent_and_never_unadoptable() {
            let primary = PathBuf::from("/k/api.sock");
            let fallback = PathBuf::from("/k/api-42.sock");
            let fallbacks = std::slice::from_ref(&fallback);
            let only_fallback = |path: &Path| path == Path::new("/k/api-42.sock");
            let chosen = detect_with_fallbacks(
                std::slice::from_ref(&primary),
                fallbacks,
                only_fallback,
                |_| None,
            );
            assert_eq!(chosen.as_deref(), Some("ipc:///k/api-42.sock"));

            let chosen = detect_with_fallbacks(
                std::slice::from_ref(&primary),
                fallbacks,
                |_| false,
                |_| None,
            );
            assert_eq!(chosen, None);
        }
    }
}
