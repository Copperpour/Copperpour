//! KiCad installation discovery.
//!
//! Keep executable and bundled-library discovery behind this module so every
//! caller sees the same installations in the same order.  Discovery is
//! intentionally read-only: Windows and KiCad already persist install state,
//! while an explicit Konnect configuration remains the durable override.

use std::path::{Path, PathBuf};

#[cfg(target_os = "windows")]
const SUPPORTED_VERSIONS: [&str; 3] = ["10.0", "9.0", "8.0"];

/// Find `kicad-cli`, honoring an explicit configured value first.
pub fn find_cli(configured: &str) -> Option<PathBuf> {
    resolve_configured(configured)
        .or_else(|| cli_candidates().into_iter().find(|path| path.is_file()))
}

/// Find the KiCad project-manager executable.
///
/// An explicit GUI path wins. If the configured CLI resolves to a binary under
/// `<root>/bin`, its sibling GUI is next so a selected KiCad version cannot be
/// mixed with a different installation's libraries or UI.
pub fn find_gui(configured_binary: &str, configured_cli: &str) -> Option<PathBuf> {
    resolve_explicit_path(configured_binary)
        .or_else(|| {
            resolve_configured(configured_cli)
                .and_then(|cli| installation_root_from_binary(&cli))
                .map(|root| root.join("bin").join(gui_filename()))
                .filter(|path| path.is_file())
        })
        .or_else(|| find_bare_name(configured_binary))
        .or_else(|| gui_candidates().into_iter().find(|path| path.is_file()))
}

fn cli_candidates() -> Vec<PathBuf> {
    #[cfg(target_os = "windows")]
    {
        installation_roots()
            .into_iter()
            .map(|root| root.join("bin").join(cli_filename()))
            .collect()
    }
    #[cfg(target_os = "macos")]
    {
        let mut paths = vec![
            PathBuf::from("/Applications/KiCad/KiCad.app/Contents/MacOS/kicad-cli"),
            PathBuf::from("/usr/local/bin/kicad-cli"),
        ];
        if let Some(home) = crate::app_dirs::home_dir() {
            paths.push(home.join("Applications/KiCad/KiCad.app/Contents/MacOS/kicad-cli"));
        }
        paths
    }
    #[cfg(not(any(target_os = "windows", target_os = "macos")))]
    {
        vec![
            PathBuf::from("/usr/bin/kicad-cli"),
            PathBuf::from("/usr/local/bin/kicad-cli"),
        ]
    }
}

fn gui_candidates() -> Vec<PathBuf> {
    #[cfg(target_os = "windows")]
    {
        installation_roots()
            .into_iter()
            .map(|root| root.join("bin").join(gui_filename()))
            .collect()
    }
    #[cfg(target_os = "macos")]
    {
        vec![PathBuf::from(
            "/Applications/KiCad/KiCad.app/Contents/MacOS/kicad",
        )]
    }
    #[cfg(not(any(target_os = "windows", target_os = "macos")))]
    {
        ["kicad"]
            .into_iter()
            .filter_map(|name| find_on_path(Path::new(name)))
            .collect()
    }
}

/// Existing roots that directly contain KiCad's `symbols`, `footprints`, and
/// `3dmodels` directories, newest supported KiCad first.
pub fn share_roots() -> Vec<PathBuf> {
    let mut roots = Vec::new();

    #[cfg(target_os = "windows")]
    for install in installation_roots() {
        push_existing_unique(&mut roots, install.join("share").join("kicad"));
    }

    #[cfg(target_os = "macos")]
    {
        push_existing_unique(
            &mut roots,
            PathBuf::from("/Applications/KiCad/KiCad.app/Contents/SharedSupport"),
        );
        push_existing_unique(&mut roots, PathBuf::from("/usr/local/share/kicad"));
        push_existing_unique(&mut roots, PathBuf::from("/opt/homebrew/share/kicad"));
        if let Some(home) = crate::app_dirs::home_dir() {
            push_existing_unique(
                &mut roots,
                home.join("Applications/KiCad/KiCad.app/Contents/SharedSupport"),
            );
        }
    }

    #[cfg(not(any(target_os = "windows", target_os = "macos")))]
    {
        for root in [
            PathBuf::from("/usr/share/kicad"),
            PathBuf::from("/usr/local/share/kicad"),
            PathBuf::from("/opt/kicad/share/kicad"),
            PathBuf::from("/var/lib/flatpak/app/org.kicad.KiCad/current/active/files/share/kicad"),
            PathBuf::from("/snap/kicad/current/usr/share/kicad"),
        ] {
            push_existing_unique(&mut roots, root);
        }
        if let Some(home) = crate::app_dirs::home_dir() {
            push_existing_unique(
                &mut roots,
                home.join(
                    ".local/share/flatpak/app/org.kicad.KiCad/current/active/files/share/kicad",
                ),
            );
        }
    }

    if roots.is_empty() {
        static WARNED: std::sync::OnceLock<()> = std::sync::OnceLock::new();
        WARNED.get_or_init(|| {
            tracing::warn!(
                target: "konnect::kicad_install",
                "no KiCad bundled-library root was resolved; stock symbols, footprints, and 3D models may be unavailable; configure kicad_cli or the KICAD<major>_*_DIR variables"
            );
        });
    }

    roots
}

#[cfg(target_os = "windows")]
fn installation_roots() -> Vec<PathBuf> {
    static ROOTS: std::sync::OnceLock<Vec<PathBuf>> = std::sync::OnceLock::new();
    ROOTS.get_or_init(discover_installation_roots).clone()
}

#[cfg(target_os = "windows")]
fn discover_installation_roots() -> Vec<PathBuf> {
    let mut roots = Vec::new();
    for root in windows_registry_install_roots() {
        push_existing_unique(&mut roots, root);
    }
    for root in windows_well_known_install_roots(
        std::env::var_os("LOCALAPPDATA").as_deref(),
        std::env::var_os("ProgramFiles").as_deref(),
        std::env::var_os("ProgramFiles(x86)").as_deref(),
    ) {
        push_existing_unique(&mut roots, root);
    }
    roots
}

fn resolve_configured(configured: &str) -> Option<PathBuf> {
    resolve_explicit_path(configured).or_else(|| find_bare_name(configured))
}

/// A bare name is looked up on `PATH` only, so a same-named file in the working
/// directory cannot stand in for it (#182).
fn find_bare_name(configured: &str) -> Option<PathBuf> {
    let path = Path::new(configured.trim());
    (path.components().count() == 1)
        .then(|| find_on_path(path))
        .flatten()
}

fn resolve_explicit_path(configured: &str) -> Option<PathBuf> {
    let path = PathBuf::from(configured.trim());
    (path.components().count() > 1 && path.is_file()).then_some(path)
}

fn find_on_path(filename: &Path) -> Option<PathBuf> {
    let paths = std::env::var_os("PATH")?;
    // An empty or relative entry would search the working directory again.
    std::env::split_paths(&paths)
        .filter(|dir| dir.is_absolute())
        .map(|dir| dir.join(filename))
        .find(|path| path.is_file())
}

fn installation_root_from_binary(binary: &Path) -> Option<PathBuf> {
    let bin_dir = binary.parent()?;
    if !bin_dir
        .file_name()
        .is_some_and(|name| name.eq_ignore_ascii_case("bin"))
    {
        return None;
    }
    bin_dir.parent().map(Path::to_path_buf)
}

fn push_existing_unique(paths: &mut Vec<PathBuf>, path: PathBuf) {
    if path.is_dir() && !paths.contains(&path) {
        paths.push(path);
    }
}

#[cfg(any(target_os = "windows", test))]
fn cli_filename() -> &'static str {
    if cfg!(target_os = "windows") {
        "kicad-cli.exe"
    } else {
        "kicad-cli"
    }
}

fn gui_filename() -> &'static str {
    if cfg!(target_os = "windows") {
        "kicad.exe"
    } else {
        "kicad"
    }
}

#[cfg(target_os = "windows")]
fn windows_well_known_install_roots(
    local_app_data: Option<&std::ffi::OsStr>,
    program_files: Option<&std::ffi::OsStr>,
    program_files_x86: Option<&std::ffi::OsStr>,
) -> Vec<PathBuf> {
    // A relative value would be searched below the working directory (#179).
    let absolute =
        |path: Option<&std::ffi::OsStr>| path.map(PathBuf::from).filter(|p| p.is_absolute());
    let mut bases = Vec::new();
    if let Some(path) = absolute(local_app_data) {
        bases.push(path.join("Programs").join("KiCad"));
    }
    if let Some(path) = absolute(program_files) {
        bases.push(path.join("KiCad"));
    }
    if let Some(path) = absolute(program_files_x86) {
        bases.push(path.join("KiCad"));
    }
    for fallback in [
        r"C:\Program Files\KiCad",
        r"C:\KiCad",
        r"D:\KiCad",
        r"D:\Program Files\KiCad",
    ] {
        let fallback = PathBuf::from(fallback);
        if !bases.contains(&fallback) {
            bases.push(fallback);
        }
    }

    let mut roots = Vec::new();
    for version in SUPPORTED_VERSIONS {
        for base in &bases {
            roots.push(base.join(version));
        }
    }
    for base in bases {
        roots.push(base);
    }
    roots
}

#[cfg(target_os = "windows")]
fn windows_registry_install_roots() -> Vec<PathBuf> {
    use winreg::enums::{
        HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE, KEY_READ, KEY_WOW64_32KEY, KEY_WOW64_64KEY,
    };
    use winreg::RegKey;

    const UNINSTALL: &str = r"SOFTWARE\Microsoft\Windows\CurrentVersion\Uninstall";
    let mut roots = Vec::new();

    for version in SUPPORTED_VERSIONS {
        for hive in [HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE] {
            for view in [KEY_WOW64_64KEY, KEY_WOW64_32KEY] {
                let root = RegKey::predef(hive);
                let Ok(uninstall) = root.open_subkey_with_flags(UNINSTALL, KEY_READ | view) else {
                    continue;
                };
                for name in uninstall.enum_keys().flatten() {
                    let Ok(key) = uninstall.open_subkey_with_flags(&name, KEY_READ | view) else {
                        continue;
                    };
                    let display_name: String = key.get_value("DisplayName").unwrap_or_default();
                    let display_version: String =
                        key.get_value("DisplayVersion").unwrap_or_default();
                    if !display_name.to_ascii_lowercase().starts_with("kicad ")
                        || !(display_version.starts_with(version) || display_name.contains(version))
                    {
                        continue;
                    }
                    let location: String = key.get_value("InstallLocation").unwrap_or_default();
                    if !location.trim().is_empty() {
                        let path = PathBuf::from(location.trim());
                        if path.is_dir() && !roots.contains(&path) {
                            roots.push(path);
                        }
                    }
                }
            }
        }
    }

    // Older KiCad installers used vendor keys instead of an uninstall entry.
    for version in SUPPORTED_VERSIONS {
        for hive in [HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE] {
            for view in [KEY_WOW64_64KEY, KEY_WOW64_32KEY] {
                let root = RegKey::predef(hive);
                for key_path in [
                    format!(r"SOFTWARE\KiCad\{version}"),
                    r"SOFTWARE\KiCad\KiCad".into(),
                ] {
                    let Ok(key) = root.open_subkey_with_flags(&key_path, KEY_READ | view) else {
                        continue;
                    };
                    let location: String = key
                        .get_value("InstallDir")
                        .or_else(|_| key.get_value(""))
                        .unwrap_or_default();
                    if !location.trim().is_empty() {
                        let path = PathBuf::from(location.trim());
                        if path.is_dir() && !roots.contains(&path) {
                            roots.push(path);
                        }
                    }
                }
            }
        }
    }

    roots
}

#[cfg(test)]
mod tests {
    use super::*;

    /// With a relative `HOME`, no per-user KiCad root, `kicad-cli` or native
    /// bridge directory is found below the working directory, even one
    /// planted there (#179). Run in a child process, since `HOME` is
    /// process-wide.
    #[cfg(unix)]
    #[test]
    fn a_relative_home_finds_nothing_below_the_working_directory() {
        const CHILD: &str = "COPPERPOUR_TEST_KICAD_INSTALL_HOME_CHILD";
        if std::env::var_os(CHILD).is_none() {
            let cwd = tempfile::tempdir().unwrap();
            for root in [
                "home/.local/share/flatpak/app/org.kicad.KiCad/current/active/files/share/kicad",
                "home/Applications/KiCad/KiCad.app/Contents/SharedSupport",
            ] {
                std::fs::create_dir_all(cwd.path().join(root)).unwrap();
            }
            let output = std::process::Command::new(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "kicad_install::tests::a_relative_home_finds_nothing_below_the_working_directory",
                    "--nocapture",
                ])
                .current_dir(cwd.path())
                .env(CHILD, "1")
                .env("HOME", "home")
                .env("XDG_DATA_HOME", "")
                .env("COPPERPOUR_BRIDGE_DIR", "")
                .env("KONNECT_BRIDGE_DIR", "")
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
        let found = share_roots()
            .into_iter()
            .chain(cli_candidates())
            .chain(crate::native_specctra_bridge::registration_root());
        for path in found {
            assert!(path.is_absolute(), "{path:?}");
        }
    }

    #[cfg(target_os = "windows")]
    #[test]
    fn derives_install_root_from_cli() {
        let cli = Path::new(r"C:\Users\casey\AppData\Local\Programs\KiCad\10.0\bin\kicad-cli.exe");
        assert_eq!(
            installation_root_from_binary(cli),
            Some(PathBuf::from(
                r"C:\Users\casey\AppData\Local\Programs\KiCad\10.0"
            ))
        );
    }

    #[cfg(target_os = "windows")]
    #[test]
    fn per_user_roots_are_newest_first() {
        use std::ffi::OsStr;

        let roots = windows_well_known_install_roots(
            Some(OsStr::new(r"C:\Users\casey\AppData\Local")),
            Some(OsStr::new(r"C:\Program Files")),
            None,
        );
        assert_eq!(
            roots[0],
            PathBuf::from(r"C:\Users\casey\AppData\Local\Programs\KiCad\10.0")
        );
        assert!(roots.contains(&PathBuf::from(
            r"C:\Users\casey\AppData\Local\Programs\KiCad\9.0"
        )));
    }

    /// A relative `LOCALAPPDATA` or `ProgramFiles` is not searched below the
    /// working directory (#179).
    #[cfg(target_os = "windows")]
    #[test]
    fn a_relative_base_is_not_searched() {
        use std::ffi::OsStr;

        let roots = windows_well_known_install_roots(
            Some(OsStr::new("relative")),
            Some(OsStr::new("Program Files")),
            Some(OsStr::new(r"C:\Program Files (x86)")),
        );
        assert!(roots.iter().all(|root| root.is_absolute()), "{roots:?}");
        assert!(roots.contains(&PathBuf::from(r"C:\Program Files (x86)\KiCad\10.0")));
    }

    #[cfg(target_os = "windows")]
    #[test]
    #[ignore = "requires a complete KiCad installation"]
    fn live_install_exposes_all_bundled_library_kinds() {
        let cli = find_cli("").expect("KiCad installation was not discovered");
        let install = installation_root_from_binary(&cli).expect("CLI was not under <root>/bin");
        let share = install.join("share").join("kicad");

        for kind in ["symbols", "footprints", "3dmodels"] {
            assert!(share.join(kind).is_dir(), "missing KiCad {kind} directory");
        }
        assert!(share_roots().contains(&share));
    }

    #[test]
    fn explicit_cli_and_sibling_gui_win() {
        let temp = tempfile::tempdir().unwrap();
        let bin = temp.path().join("bin");
        std::fs::create_dir(&bin).unwrap();
        let cli = bin.join(cli_filename());
        let gui = bin.join(gui_filename());
        std::fs::write(&cli, b"").unwrap();
        std::fs::write(&gui, b"").unwrap();

        assert_eq!(find_cli(cli.to_str().unwrap()), Some(cli.clone()));
        assert_eq!(find_gui(gui_filename(), cli.to_str().unwrap()), Some(gui));
    }

    /// Tests run in the package root, where `Cargo.toml` is a same-named file
    /// a bare name must not resolve to (#182).
    #[test]
    fn a_bare_name_never_resolves_to_the_working_directory() {
        assert!(Path::new("Cargo.toml").is_file());
        let not_relative = |found: Option<PathBuf>| found.is_none_or(|path| path.is_absolute());

        assert!(not_relative(find_cli("Cargo.toml")));
        assert!(not_relative(find_gui("Cargo.toml", "")));
        // A directory component makes it a path, which is taken as given.
        let explicit = Some(PathBuf::from("./Cargo.toml"));
        assert_eq!(find_cli("./Cargo.toml"), explicit);
        assert_eq!(find_gui("./Cargo.toml", ""), explicit);
    }
}
