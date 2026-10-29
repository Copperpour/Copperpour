//! `cargo xtask check-fixtures [--fix]` finds byte-exact files whose working
//! copy differs from the index although git calls them unchanged.
//!
//! Fixtures are `-text` (see `.gitattributes`), so their bytes on disk must be
//! the committed bytes. A checkout made before a file became `-text` keeps
//! whatever line endings `core.autocrlf` gave it, and git never rewrites it:
//! the index stat still matches, so `git status` is clean and
//! `git checkout -- <file>` skips it. Tests then fail on that machine only
//! (Copperpour/Copperpour#48). Deleting the file first is what forces git to write it again.

use std::collections::HashSet;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

/// Pathspec for files whose `text` attribute is unset.
const EXACT: &str = ":(attr:-text)";

pub fn check(args: &[String]) {
    let fix = match args {
        [] => false,
        [flag] if flag == "--fix" => true,
        _ => {
            eprintln!("usage: cargo xtask check-fixtures [--fix]");
            std::process::exit(2);
        }
    };
    // The checkout we run in, not `repo_root()`: the tests run this in a scratch repo.
    let root = PathBuf::from(
        String::from_utf8(git(Path::new("."), &["rev-parse", "--show-toplevel"], None))
            .expect("toplevel is UTF-8")
            .trim_end(),
    );

    let (checked, stale) = stale_files(&root);
    if stale.is_empty() {
        println!("no stale file among {checked} byte-exact files");
        return;
    }
    println!(
        "{} of {checked} byte-exact files differ from the index:",
        stale.len()
    );
    for path in &stale {
        println!("  {path}");
    }
    if !fix {
        println!("run `cargo xtask check-fixtures --fix` to check them out again");
        std::process::exit(1);
    }

    // Deleted tracked files show in `git status`, so a failure here is recoverable
    // with `git checkout -- <file>`.
    for path in &stale {
        std::fs::remove_file(root.join(path)).expect("remove stale file");
    }
    let paths: String = stale.iter().map(|p| format!("{p}\0")).collect();
    git(
        &root,
        &[
            "--literal-pathspecs",
            "checkout",
            "--pathspec-from-file=-",
            "--pathspec-file-nul",
        ],
        Some(&paths),
    );

    let (_, left) = stale_files(&root);
    if !left.is_empty() {
        eprintln!("still differ after checkout: {left:?}");
        std::process::exit(1);
    }
    println!("checked out {} file(s) again", stale.len());
}

/// Tracked files with `text` unset whose bytes on disk differ from their index
/// blob while git reports no change. Returns the number of byte-exact files
/// examined and the stale ones. A file git already shows as modified is a
/// local edit and is left alone.
fn stale_files(root: &Path) -> (usize, Vec<String>) {
    let listing = git(root, &["ls-files", "-s", "-z", "--", EXACT], None);
    let mut exact: Vec<(String, String)> = Vec::new();
    for entry in split_z(&listing) {
        // "<mode> <sha> <stage>\t<path>"
        let Some((meta, path)) = entry.split_once('\t') else {
            continue;
        };
        let mut fields = meta.split(' ');
        let (Some(mode), Some(sha), Some("0")) = (fields.next(), fields.next(), fields.next())
        else {
            continue;
        };
        if mode.starts_with("100") && root.join(path).is_file() {
            exact.push((path.to_string(), sha.to_string()));
        }
    }

    let modified = git(root, &["diff", "--name-only", "-z", "--", EXACT], None);
    let modified: HashSet<&str> = split_z(&modified).into_iter().collect();

    let stdin: String = exact.iter().map(|(p, _)| format!("{p}\n")).collect();
    let hashes = git(
        root,
        &["hash-object", "--no-filters", "--stdin-paths"],
        Some(&stdin),
    );
    let hashes = String::from_utf8(hashes).expect("hashes are ASCII");

    let stale = exact
        .iter()
        .zip(hashes.lines())
        .filter(|((path, sha), disk)| disk != sha && !modified.contains(&path.as_str()))
        .map(|((path, _), _)| path.clone())
        .collect();
    (exact.len(), stale)
}

fn split_z(bytes: &[u8]) -> Vec<&str> {
    std::str::from_utf8(bytes)
        .expect("git output is UTF-8")
        .split('\0')
        .filter(|s| !s.is_empty())
        .collect()
}

fn git(dir: &Path, args: &[&str], stdin: Option<&str>) -> Vec<u8> {
    let mut child = Command::new("git")
        .args(["-c", "core.quotepath=off"])
        .args(args)
        .current_dir(dir)
        .stdin(if stdin.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stdout(Stdio::piped())
        .spawn()
        .expect("run git");
    // Write stdin from its own thread: git answers while it reads, and once both
    // pipes fill, writing everything before reading would deadlock.
    let out = std::thread::scope(|scope| {
        if let (Some(input), Some(mut pipe)) = (stdin, child.stdin.take()) {
            scope.spawn(move || pipe.write_all(input.as_bytes()).expect("write git stdin"));
        }
        child.wait_with_output().expect("wait for git")
    });
    if !out.status.success() {
        eprintln!("git {} failed", args.join(" "));
        std::process::exit(1);
    }
    out.stdout
}
