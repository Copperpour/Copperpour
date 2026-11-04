//! `proto-manifest.toml` records which KiCad tag the vendored protocol in
//! `proto/` was copied from (#67). These tests check offline that the two
//! still agree: `proto/` holds exactly the files the manifest lists, each with
//! the SHA-256 it records. A hand edit, a stray file or a partial sync fails.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};

fn crate_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn manifest() -> toml::Table {
    let text = std::fs::read_to_string(crate_dir().join("proto-manifest.toml"))
        .expect("read proto-manifest.toml");
    text.parse().expect("proto-manifest.toml is TOML")
}

/// The manifest's `[files]`: path under `proto/` to SHA-256, lower-case hex.
fn listed_files(manifest: &toml::Table) -> BTreeMap<String, String> {
    manifest["files"]
        .as_table()
        .expect("[files] is a table")
        .iter()
        .map(|(path, hash)| {
            let hash = hash.as_str().expect("each hash is a string");
            (path.clone(), hash.to_string())
        })
        .collect()
}

/// Every file under `root`, by its path relative to `root` with `/`
/// separators, and its SHA-256 in lower-case hex.
fn hash_tree(root: &Path) -> BTreeMap<String, String> {
    fn walk(root: &Path, dir: &Path, out: &mut BTreeMap<String, String>) {
        for entry in std::fs::read_dir(dir).expect("read proto directory") {
            let path = entry.expect("directory entry").path();
            if path.is_dir() {
                walk(root, &path, out);
                continue;
            }
            let relative = path
                .strip_prefix(root)
                .expect("under root")
                .components()
                .map(|part| part.as_os_str().to_str().expect("UTF-8 path"))
                .collect::<Vec<_>>()
                .join("/");
            let bytes = std::fs::read(&path).expect("read proto file");
            let hash = Sha256::digest(&bytes)
                .iter()
                .map(|byte| format!("{byte:02x}"))
                .collect();
            out.insert(relative, hash);
        }
    }
    let mut out = BTreeMap::new();
    walk(root, root, &mut out);
    out
}

/// One line per file that is missing, unlisted, or hashes differently.
fn differences(listed: &BTreeMap<String, String>, found: &BTreeMap<String, String>) -> Vec<String> {
    let mut out = Vec::new();
    for (path, hash) in listed {
        match found.get(path) {
            None => out.push(format!("{path}: listed but missing")),
            Some(actual) if actual != hash => {
                out.push(format!("{path}: hash {actual}, manifest {hash}"))
            }
            Some(_) => {}
        }
    }
    for path in found.keys().filter(|path| !listed.contains_key(*path)) {
        out.push(format!("{path}: present but not listed"));
    }
    out
}

#[test]
fn the_vendored_proto_tree_is_the_one_the_manifest_records() {
    let manifest = manifest();
    for key in ["upstream", "tag", "commit_date", "synced", "source_root"] {
        assert!(
            manifest[key]
                .as_str()
                .is_some_and(|value| !value.is_empty()),
            "manifest names its {key}"
        );
    }
    let commit = manifest["commit"].as_str().expect("commit is a string");
    assert!(
        commit.len() == 40
            && commit
                .bytes()
                .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase()),
        "commit is a full lower-case SHA-1, not {commit:?}"
    );
    let supported = manifest["supported"]
        .as_table()
        .expect("[supported] is a table");
    for key in ["oldest", "newest"] {
        assert!(
            supported[key].as_str().is_some(),
            "[supported] names its {key}"
        );
    }

    let listed = listed_files(&manifest);
    assert!(!listed.is_empty(), "the manifest lists the vendored files");
    let found = hash_tree(&crate_dir().join("proto"));
    assert_eq!(differences(&listed, &found), Vec::<String>::new());
}

#[test]
fn an_edited_missing_or_unlisted_file_is_reported() {
    let listed = listed_files(&manifest());
    let copy = tempfile::tempdir().expect("temp dir");
    let mut found = hash_tree(&crate_dir().join("proto"));
    assert_eq!(differences(&listed, &found), Vec::<String>::new());

    // Hash a real tree, so the walk itself is under test too.
    let edited = copy.path().join("board");
    std::fs::create_dir_all(&edited).expect("create dir");
    let mut bytes = std::fs::read(crate_dir().join("proto/board/board.proto")).expect("read");
    bytes.push(b'\n');
    std::fs::write(edited.join("board.proto"), &bytes).expect("write");
    std::fs::write(edited.join("extra.proto"), b"syntax = \"proto3\";\n").expect("write");
    let copied = hash_tree(copy.path());
    assert_ne!(copied["board/board.proto"], listed["board/board.proto"]);

    found.insert(
        "board/board.proto".into(),
        copied["board/board.proto"].clone(),
    );
    found.insert(
        "board/extra.proto".into(),
        copied["board/extra.proto"].clone(),
    );
    found.remove("common/envelope.proto");
    let report = differences(&listed, &found);
    assert_eq!(report.len(), 3, "{report:#?}");
    assert!(
        report[0].starts_with("board/board.proto: hash "),
        "{report:#?}"
    );
    assert_eq!(report[1], "common/envelope.proto: listed but missing");
    assert_eq!(report[2], "board/extra.proto: present but not listed");
}
