//! The names Copperpour's files and environment variables go by.
//!
//! Copperpour was called Konnect. A directory still under
//! the Konnect name is renamed the first time it is used, so config, logs,
//! caches and project state carry over. A `KONNECT_*` variable is still read
//! when its `COPPERPOUR_*` replacement is not set.

use std::ffi::OsString;
use std::path::{Path, PathBuf};

/// The directory and executable name.
pub const APP_NAME: &str = "copperpour";
/// The name before the rename, still accepted for one release.
pub const LEGACY_APP_NAME: &str = "konnect";

/// The per-project state directory, `<project>/.copperpour`.
pub fn project_state_dir(project_dir: &Path) -> PathBuf {
    migrated(project_dir, ".konnect", ".copperpour")
}

/// Why a per-user path could not be resolved, for refusal messages.
pub const NO_USER_DIR: &str = "the user's home directory is unknown (HOME is relative, or unset \
     with no account entry; on Windows, APPDATA and the roaming profile folder are unavailable)";

/// The user's home directory: `HOME`, or the account's own entry when `HOME`
/// is unset or empty. `None` when that is not an absolute path (a relative
/// `HOME` included), so a per-user file is never looked up relative to the
/// working directory (#171).
pub fn home_dir() -> Option<PathBuf> {
    dirs::home_dir().filter(|path| path.is_absolute())
}

/// `%APPDATA%`, else the roaming application-data folder. `None` as for
/// [`home_dir`].
#[cfg(target_os = "windows")]
pub fn app_data_dir() -> Option<PathBuf> {
    std::env::var_os("APPDATA")
        .map(PathBuf::from)
        .filter(|path| path.is_absolute())
        .or_else(dirs::config_dir)
        .filter(|path| path.is_absolute())
}

/// The platform local-data directory (`~/.local/share`,
/// `~/Library/Application Support`, `%LOCALAPPDATA%`). `None` as for
/// [`home_dir`] (#179).
pub fn local_data_dir() -> Option<PathBuf> {
    dirs::data_local_dir().filter(|path| path.is_absolute())
}

/// The per-user directory for config, logs and run records:
/// `%APPDATA%\copperpour`, `~/Library/Application Support/copperpour`, or
/// `~/.copperpour`.
pub fn user_state_dir() -> Option<PathBuf> {
    #[cfg(target_os = "windows")]
    return app_data_dir().map(|dir| user_dir(&dir, false));
    #[cfg(target_os = "macos")]
    return home_dir().map(|home| user_dir(&home.join("Library/Application Support"), false));
    #[cfg(not(any(target_os = "windows", target_os = "macos")))]
    return home_dir().map(|home| user_dir(&home, true));
}

/// The per-user directory for templates, the JLCPCB database and the EasyEDA
/// cache: `%APPDATA%\copperpour`, else `~/.copperpour` (macOS included).
pub fn user_data_dir() -> Option<PathBuf> {
    #[cfg(target_os = "macos")]
    return home_dir().map(|home| user_dir(&home, true));
    #[cfg(not(target_os = "macos"))]
    return user_state_dir();
}

/// `parent/<leaf>` for the app's per-user directory under `parent`: `.copperpour`
/// when `hidden`, as in a home directory, else `copperpour`.
pub fn user_dir(parent: &Path, hidden: bool) -> PathBuf {
    if hidden {
        migrated(parent, ".konnect", ".copperpour")
    } else {
        migrated(parent, LEGACY_APP_NAME, APP_NAME)
    }
}

/// `parent/new`, after renaming `parent/old` to it when only the old one
/// exists. If the rename fails, the old directory is used where it is.
fn migrated(parent: &Path, old: &str, new: &str) -> PathBuf {
    let new_path = parent.join(new);
    let old_path = parent.join(old);
    if new_path.exists() || !old_path.is_dir() {
        return new_path;
    }
    match std::fs::rename(&old_path, &new_path) {
        Ok(()) => {
            tracing::info!("moved {} to {}", old_path.display(), new_path.display());
            new_path
        }
        // Another process may have moved it first.
        Err(_) if new_path.exists() => new_path,
        Err(error) => {
            tracing::warn!(
                "could not move {} to {} ({error}); using it where it is",
                old_path.display(),
                new_path.display()
            );
            old_path
        }
    }
}

/// `COPPERPOUR_<name>`, or else `KONNECT_<name>`. An empty value counts as unset.
pub fn env_var(name: &str) -> Option<OsString> {
    [APP_NAME, LEGACY_APP_NAME]
        .iter()
        .map(|app| format!("{}_{name}", app.to_ascii_uppercase()))
        .find_map(|key| std::env::var_os(key).filter(|value| !value.is_empty()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_old_directory_is_moved_with_its_contents() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join(".konnect/logs")).unwrap();
        std::fs::write(dir.path().join(".konnect/config.json"), "{}").unwrap();

        let path = user_dir(dir.path(), true);

        assert_eq!(path, dir.path().join(".copperpour"));
        assert!(path.join("config.json").is_file());
        assert!(path.join("logs").is_dir());
        assert!(!dir.path().join(".konnect").exists());
    }

    #[test]
    fn an_existing_new_directory_wins_and_the_old_one_is_left_alone() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("konnect")).unwrap();
        std::fs::create_dir_all(dir.path().join("copperpour")).unwrap();

        assert_eq!(user_dir(dir.path(), false), dir.path().join("copperpour"));
        assert!(dir.path().join("konnect").is_dir());
    }

    #[test]
    fn nothing_is_created_when_neither_exists() {
        let dir = tempfile::tempdir().unwrap();

        assert_eq!(
            project_state_dir(dir.path()),
            dir.path().join(".copperpour")
        );
        assert!(!dir.path().join(".copperpour").exists());
    }

    #[test]
    fn a_file_with_the_old_name_is_not_moved() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join(".konnect"), "not a directory").unwrap();

        assert_eq!(
            project_state_dir(dir.path()),
            dir.path().join(".copperpour")
        );
        assert!(dir.path().join(".konnect").is_file());
    }

    /// A relative `HOME` or `APPDATA` never yields a relative per-user path
    /// (#171, #179). Run in a child process, since the variables are process-wide.
    #[test]
    fn a_relative_home_gives_no_relative_user_directory() {
        const CHILD: &str = "COPPERPOUR_TEST_RELATIVE_HOME_CHILD";
        if std::env::var_os(CHILD).is_none() {
            let output = std::process::Command::new(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "app_dirs::tests::a_relative_home_gives_no_relative_user_directory",
                    "--nocapture",
                ])
                .env(CHILD, "1")
                .env("HOME", "relative")
                .env("APPDATA", "relative")
                .env("XDG_DATA_HOME", "")
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
        // Windows falls back to the roaming profile; elsewhere there is none.
        #[cfg(target_os = "windows")]
        for path in [
            app_data_dir(),
            local_data_dir(),
            user_state_dir(),
            user_data_dir(),
        ] {
            assert!(path.as_deref().is_some_and(Path::is_absolute), "{path:?}");
        }
        #[cfg(not(target_os = "windows"))]
        assert_eq!(
            [
                home_dir(),
                local_data_dir(),
                user_state_dir(),
                user_data_dir()
            ],
            [None, None, None, None]
        );
    }

    #[test]
    fn the_new_variable_wins_and_the_old_one_is_the_fallback() {
        // Variables of its own, so no other test reads them.
        let name = "APP_DIRS_TEST";
        std::env::remove_var("COPPERPOUR_APP_DIRS_TEST");
        std::env::set_var("KONNECT_APP_DIRS_TEST", "old");
        assert_eq!(env_var(name), Some("old".into()));

        std::env::set_var("COPPERPOUR_APP_DIRS_TEST", "new");
        assert_eq!(env_var(name), Some("new".into()));

        std::env::set_var("COPPERPOUR_APP_DIRS_TEST", "");
        assert_eq!(env_var(name), Some("old".into()), "empty counts as unset");

        std::env::remove_var("COPPERPOUR_APP_DIRS_TEST");
        std::env::remove_var("KONNECT_APP_DIRS_TEST");
        assert_eq!(env_var(name), None);
    }
}
