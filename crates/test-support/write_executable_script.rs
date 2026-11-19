use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::process::Command;

pub(crate) fn write_executable_script(path: &Path, script: impl AsRef<str>) {
    // The shell owns the writer and exits before the script is used. A
    // concurrent fork by libtest can then never inherit that writable fd
    // and briefly prevent exec with ETXTBSY.
    let status = Command::new("sh")
        .args(["-c", "printf '%s' \"$1\" > \"$2\"", "write-test-script"])
        .arg(script.as_ref())
        .arg(path)
        .status()
        .unwrap();
    assert!(
        status.success(),
        "writing {} failed: {status}",
        path.display()
    );
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700)).unwrap();
}
