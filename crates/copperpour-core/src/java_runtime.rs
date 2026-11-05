//! Which `java` runs Freerouting, and whether it is recent enough for the JAR.
//!
//! A bare `Command::new("java")` takes the first `java` on the PATH. That misses
//! `JAVA_HOME`, and on macOS it lands on `/usr/bin/java`, a stub that fails when
//! the only JDK is Homebrew's keg-only `openjdk`. It also says nothing about the
//! JAR: Freerouting 2.5 is built for Java 25, and an older `java` dies on
//! `UnsupportedClassVersionError` only once routing is asked for.

use std::ffi::OsString;
use std::io::Read;
use std::path::{Path, PathBuf};

/// Homebrew's keg-only OpenJDK: not linked into the PATH by default.
const HOMEBREW_JAVA: &[&str] = &[
    "/opt/homebrew/opt/openjdk/bin/java",
    "/usr/local/opt/openjdk/bin/java",
];
/// macOS's launcher stub, only usable when a JDK is registered system-wide.
const MACOS_STUB_JAVA: &str = "/usr/bin/java";

/// The launcher's file name on this platform.
const JAVA_BINARY: &str = if cfg!(windows) { "java.exe" } else { "java" };

/// Serialises tests that set `JAVA_HOME` or `PATH`, or that resolve `java`
/// through them: both are process-wide.
#[cfg(test)]
pub(crate) static JAVA_ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// The `java` to run: `JAVA_HOME`, then the PATH, then Homebrew's OpenJDK on
/// macOS (ahead of the system stub). Falls back to a bare `java`.
pub(crate) fn java_executable() -> PathBuf {
    first_runnable(candidates(
        std::env::var_os("JAVA_HOME"),
        std::env::var_os("PATH"),
        cfg!(target_os = "macos"),
    ))
    .unwrap_or_else(|| PathBuf::from("java"))
}

/// The first candidate that is a file this process may run. On Unix a file
/// without an execute bit is skipped, so a broken `$JAVA_HOME/bin/java` does
/// not hide a working `java` further down.
fn first_runnable(candidates: Vec<PathBuf>) -> Option<PathBuf> {
    candidates.into_iter().find(|path| is_runnable(path))
}

#[cfg(unix)]
fn is_runnable(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(path)
        .is_ok_and(|metadata| metadata.is_file() && metadata.permissions().mode() & 0o111 != 0)
}

#[cfg(not(unix))]
fn is_runnable(path: &Path) -> bool {
    path.is_file()
}

fn candidates(java_home: Option<OsString>, path: Option<OsString>, macos: bool) -> Vec<PathBuf> {
    let binary = JAVA_BINARY;
    let mut found = Vec::new();
    if let Some(home) = java_home.filter(|home| !home.is_empty()) {
        found.push(PathBuf::from(home).join("bin").join(binary));
    }
    let mut stub = None;
    for dir in path.iter().flat_map(std::env::split_paths) {
        let java = dir.join(binary);
        if macos && java == Path::new(MACOS_STUB_JAVA) {
            stub = Some(java);
        } else {
            found.push(java);
        }
    }
    if macos {
        found.extend(HOMEBREW_JAVA.iter().map(PathBuf::from));
        found.extend(stub);
    }
    found
}

/// The Java feature release the JAR's main class was compiled for, read from
/// its class-file major version (Java N writes N + 44).
pub(crate) fn required_java_release(jar: &Path) -> Option<u32> {
    let file = std::fs::File::open(jar).ok()?;
    let mut archive = zip::ZipArchive::new(file).ok()?;
    let mut manifest = String::new();
    archive
        .by_name("META-INF/MANIFEST.MF")
        .ok()?
        .read_to_string(&mut manifest)
        .ok()?;
    let main_class = manifest
        .lines()
        .find_map(|line| line.strip_prefix("Main-Class:"))?
        .trim();
    let mut header = [0u8; 8];
    archive
        .by_name(&format!("{}.class", main_class.replace('.', "/")))
        .ok()?
        .read_exact(&mut header)
        .ok()?;
    class_file_release(&header)
}

fn class_file_release(header: &[u8; 8]) -> Option<u32> {
    if header[..4] != [0xCA, 0xFE, 0xBA, 0xBE] {
        return None;
    }
    let major = u32::from(u16::from_be_bytes([header[6], header[7]]));
    major.checked_sub(44).filter(|release| *release >= 1)
}

/// The feature release from `java -version` output: `version "25.0.4.1"` is 25,
/// the legacy `version "1.8.0_402"` is 8.
pub(crate) fn java_release(version_output: &str) -> Option<u32> {
    let quoted = version_output
        .split_once("version \"")?
        .1
        .split('"')
        .next()?;
    let mut parts = quoted.split(['.', '_', '-', '+']);
    let first: u32 = parts.next()?.parse().ok()?;
    if first == 1 {
        parts.next()?.parse().ok()
    } else {
        Some(first)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn java_home_comes_first_then_the_path() {
        let path = std::env::join_paths(["/usr/local/bin", "/usr/bin"]).unwrap();
        let found = candidates(Some("/jdk".into()), Some(path), false);
        // `java.exe` on Windows: built with `join`, as the resolver does.
        assert_eq!(
            found,
            [
                Path::new("/jdk").join("bin").join(JAVA_BINARY),
                Path::new("/usr/local/bin").join(JAVA_BINARY),
                Path::new("/usr/bin").join(JAVA_BINARY),
            ]
        );
    }

    #[test]
    fn a_directory_or_a_missing_file_is_not_runnable() {
        let temp = tempfile::tempdir().unwrap();
        let file = temp.path().join("java");
        std::fs::write(&file, b"").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        let found = first_runnable(vec![
            temp.path().join("missing"),
            temp.path().to_path_buf(),
            file.clone(),
        ]);
        assert_eq!(found, Some(file));
    }

    /// A `java` without an execute bit is skipped for the next candidate.
    #[cfg(unix)]
    #[test]
    fn a_java_without_an_execute_bit_is_skipped() {
        use std::os::unix::fs::PermissionsExt;
        let temp = tempfile::tempdir().unwrap();
        let broken = temp.path().join("broken");
        let working = temp.path().join("working");
        for (path, mode) in [(&broken, 0o644), (&working, 0o755)] {
            std::fs::write(path, b"").unwrap();
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode)).unwrap();
        }
        assert_eq!(
            first_runnable(vec![broken.clone(), working.clone()]),
            Some(working)
        );
        assert_eq!(first_runnable(vec![broken]), None);
    }

    /// On macOS (a Unix-only layout: the stub is `/usr/bin/java`).
    #[cfg(unix)]
    #[test]
    fn macos_tries_homebrew_before_the_system_stub() {
        let path = std::env::join_paths(["/usr/bin", "/opt/tools/bin"]).unwrap();
        let found = candidates(None, Some(path), true);
        assert_eq!(
            found,
            [
                PathBuf::from("/opt/tools/bin/java"),
                PathBuf::from("/opt/homebrew/opt/openjdk/bin/java"),
                PathBuf::from("/usr/local/opt/openjdk/bin/java"),
                PathBuf::from("/usr/bin/java"),
            ]
        );
    }

    #[test]
    fn empty_java_home_is_ignored() {
        assert!(candidates(Some("".into()), None, false).is_empty());
    }

    #[test]
    fn class_file_major_version_maps_to_the_java_release() {
        // Freerouting 2.5.0's app/freerouting/Freerouting.class: major 69.
        assert_eq!(
            class_file_release(&[0xCA, 0xFE, 0xBA, 0xBE, 0, 0, 0, 69]),
            Some(25)
        );
        assert_eq!(
            class_file_release(&[0xCA, 0xFE, 0xBA, 0xBE, 0, 0, 0, 52]),
            Some(8)
        );
        assert_eq!(class_file_release(&[0, 0, 0, 0, 0, 0, 0, 69]), None);
    }

    #[test]
    fn required_release_is_read_from_the_main_class_of_a_jar() {
        use std::io::Write;
        let temp = tempfile::tempdir().unwrap();
        let jar = temp.path().join("app.jar");
        let mut zip = zip::ZipWriter::new(std::fs::File::create(&jar).unwrap());
        let options = zip::write::SimpleFileOptions::default();
        zip.start_file("META-INF/MANIFEST.MF", options).unwrap();
        zip.write_all(b"Manifest-Version: 1.0\r\nMain-Class: app.freerouting.Freerouting\r\n")
            .unwrap();
        zip.start_file("app/freerouting/Freerouting.class", options)
            .unwrap();
        zip.write_all(&[0xCA, 0xFE, 0xBA, 0xBE, 0, 0, 0, 69, 1, 2, 3])
            .unwrap();
        zip.finish().unwrap();
        assert_eq!(required_java_release(&jar), Some(25));
        assert_eq!(
            required_java_release(&temp.path().join("missing.jar")),
            None
        );
    }

    #[test]
    fn java_version_output_gives_the_feature_release() {
        assert_eq!(
            java_release("openjdk version \"25.0.4.1\" 2026-08-18\nOpenJDK Runtime"),
            Some(25)
        );
        assert_eq!(java_release("java version \"1.8.0_402\""), Some(8));
        assert_eq!(java_release("openjdk version \"21\" 2023-09-19"), Some(21));
        assert_eq!(java_release("no version here"), None);
    }
}
