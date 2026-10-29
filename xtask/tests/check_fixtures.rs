//! `cargo xtask check-fixtures` against a repository in the state
//! Copperpour/Copperpour#48 found: a fixture checked out with CRLF before its
//! `-text` rule existed, which git still reports as unchanged.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::time::{Duration, SystemTime};

/// A hook running `cargo test` exports these, which would point every git call
/// here at the outer repository.
fn command(program: &str) -> Command {
    let mut cmd = Command::new(program);
    for var in ["GIT_DIR", "GIT_INDEX_FILE", "GIT_WORK_TREE"] {
        cmd.env_remove(var);
    }
    cmd
}

fn git(dir: &Path, args: &[&str]) {
    let out = command("git")
        .args(["-c", "commit.gpgsign=false"])
        .args(args)
        .current_dir(dir)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

fn xtask(dir: &Path, args: &[&str]) -> Output {
    command(env!("CARGO_BIN_EXE_xtask"))
        .arg("check-fixtures")
        .args(args)
        .current_dir(dir)
        .output()
        .unwrap()
}

fn backdate(path: &Path) {
    let old = SystemTime::now() - Duration::from_secs(24 * 3600);
    std::fs::File::options()
        .write(true)
        .open(path)
        .unwrap()
        .set_modified(old)
        .unwrap();
}

/// `fx/stale.sch` is CRLF on disk and LF in the index, `fx/edited.sch` carries
/// a local edit, and `notes.txt` is CRLF on disk as `core.autocrlf` intends.
fn stale_checkout(name: &str) -> PathBuf {
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR")).join(name);
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("fx")).unwrap();
    git(&dir, &["init", "-q"]);
    git(&dir, &["config", "user.name", "t"]);
    git(&dir, &["config", "user.email", "t@t"]);
    git(&dir, &["config", "core.autocrlf", "false"]);
    std::fs::write(dir.join(".gitattributes"), "* text=auto\n").unwrap();
    for f in ["fx/stale.sch", "fx/edited.sch", "notes.txt"] {
        std::fs::write(dir.join(f), "a\nb\n").unwrap();
    }
    git(&dir, &["add", "."]);
    git(&dir, &["commit", "-qm", "before the rule"]);

    // A Windows checkout from before the rule: every text file gets CRLF.
    git(&dir, &["config", "core.autocrlf", "true"]);
    let files = ["fx/stale.sch", "fx/edited.sch", "notes.txt"];
    for f in files {
        std::fs::remove_file(dir.join(f)).unwrap();
    }
    git(&dir, &[&["checkout", "--"][..], &files].concat());
    for f in files {
        backdate(&dir.join(f));
    }
    git(&dir, &["update-index", "--really-refresh"]);

    std::fs::write(dir.join(".gitattributes"), "* text=auto\nfx/** -text\n").unwrap();
    git(&dir, &["add", ".gitattributes"]);
    git(&dir, &["commit", "-qm", "fixtures are byte-exact"]);
    std::fs::write(dir.join("fx/edited.sch"), "a\nchanged\n").unwrap();

    assert_eq!(
        std::fs::read(dir.join("fx/stale.sch")).unwrap(),
        b"a\r\nb\r\n"
    );
    assert_eq!(std::fs::read(dir.join("notes.txt")).unwrap(), b"a\r\nb\r\n");
    dir
}

#[test]
fn reports_only_the_stale_byte_exact_file() {
    let dir = stale_checkout("check_fixtures_report");
    let out = xtask(&dir, &[]);
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert_eq!(out.status.code(), Some(1), "{stdout}");
    assert!(
        stdout.contains("1 of 2 byte-exact files differ"),
        "{stdout}"
    );
    assert!(stdout.contains("  fx/stale.sch\n"), "{stdout}");
    assert!(!stdout.contains("edited.sch"), "{stdout}");
    assert!(!stdout.contains("notes.txt"), "{stdout}");
    assert_eq!(
        std::fs::read(dir.join("fx/stale.sch")).unwrap(),
        b"a\r\nb\r\n"
    );
}

#[test]
fn fix_rewrites_the_stale_file_and_keeps_local_edits() {
    let dir = stale_checkout("check_fixtures_fix");
    let out = xtask(&dir, &["--fix"]);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(std::fs::read(dir.join("fx/stale.sch")).unwrap(), b"a\nb\n");
    assert_eq!(
        std::fs::read(dir.join("fx/edited.sch")).unwrap(),
        b"a\nchanged\n"
    );
    assert_eq!(std::fs::read(dir.join("notes.txt")).unwrap(), b"a\r\nb\r\n");

    let again = xtask(&dir, &[]);
    let stdout = String::from_utf8_lossy(&again.stdout);
    assert!(again.status.success(), "{stdout}");
    assert!(
        stdout.contains("no stale file among 2 byte-exact files"),
        "{stdout}"
    );
}
