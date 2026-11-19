use super::*;
use crate::router::ToolRouter;
use crate::tools::{ServerConfig, ToolContext};
use std::sync::Arc;

// KiCad 10.0.8 wrote each of these from the stock Hirose FH12-24S; provenance
// in tests/fixtures/rename_footprint_kicad10.README.md.
const SOURCE: &str = include_str!("../../tests/fixtures/rename_footprint_source_kicad10.kicad_mod");
const RENAMED: &str =
    include_str!("../../tests/fixtures/rename_footprint_renamed_kicad10.kicad_mod");
const KEPT_SOURCE: &str =
    include_str!("../../tests/fixtures/rename_footprint_kept_source_kicad10.kicad_mod");
const KEPT: &str = include_str!("../../tests/fixtures/rename_footprint_kept_kicad10.kicad_mod");
const OLD: &str = "Hirose_FH12-24S-0.5SH_1x24-1MP_P0.50mm_Horizontal";
const NEW: &str = "FFC_Hirose_FH12-24S_Renamed";

const OTHER: &str = "(footprint \"Other\"\n\t(version 20260206)\n\t(layer \"F.Cu\")\n)\n";

fn test_ctx() -> ToolContext {
    ToolContext::new(
        ServerConfig {
            kicad_cli: String::new(),
            kicad_binary: String::new(),
            ipc_address: String::new(),
            project_dir: None,
            jlcpcb_db_path: None,
            auto_load_toolsets: false,
            eager_toolsets: false,
        },
        Arc::new(ToolRouter::new()),
    )
}

/// A library holding `files` (name without extension, content).
fn library(files: &[(&str, &str)]) -> (tempfile::TempDir, PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    let library = dir.path().join("Connectors.pretty");
    std::fs::create_dir(&library).unwrap();
    for (name, content) in files {
        std::fs::write(library.join(format!("{name}.kicad_mod")), content).unwrap();
    }
    (dir, library)
}

/// The library's files, sorted by name.
fn files(library: &Path) -> Vec<(String, String)> {
    let mut files: Vec<(String, String)> = std::fs::read_dir(library)
        .unwrap()
        .map(|entry| {
            let path = entry.unwrap().path();
            let name = path.file_name().unwrap().to_str().unwrap().to_string();
            (name, std::fs::read_to_string(&path).unwrap())
        })
        .collect();
    files.sort();
    files
}

async fn rename(library: &Path, old: &str, new: &str) -> (bool, serde_json::Value) {
    let result = handle_rename_footprint(
        &json!({ "library_path": library, "footprint_name": old, "new_name": new }),
        &test_ctx(),
    )
    .await
    .unwrap();
    let text = match result.content.first() {
        Some(crate::mcp::protocol::ToolContent::Text { text }) => text.clone(),
        other => panic!("expected text content, got {other:?}"),
    };
    (result.is_error, serde_json::from_str(&text).unwrap())
}

fn without_uuids(text: &str) -> String {
    regex::Regex::new(r#"\(uuid "[^"]*"\)"#)
        .unwrap()
        .replace_all(text, "(uuid)")
        .into_owned()
}

#[tokio::test]
async fn a_footprint_is_renamed_as_kicad_renames_it() {
    let (_dir, library) = library(&[(OLD, SOURCE), ("Other", OTHER)]);
    let (is_error, body) = rename(&library, OLD, NEW).await;
    assert!(!is_error, "{body}");

    let files = files(&library);
    assert_eq!(
        files
            .iter()
            .map(|(name, _)| name.as_str())
            .collect::<Vec<_>>(),
        [format!("{NEW}.kicad_mod").as_str(), "Other.kicad_mod"]
    );
    assert_eq!(
        without_uuids(&files[0].1),
        without_uuids(RENAMED),
        "the name and the Value change as KiCad changes them, and nothing else"
    );
    assert_eq!(files[1].1, OTHER);
    assert_eq!(body["previous_name"], OLD);
    assert_eq!(body["name"], NEW);
    assert_eq!(body["value"], json!({ "previous": OLD, "current": NEW }));
    assert!(
        body["kept_references"]
            .as_str()
            .unwrap()
            .contains("set_symbol_properties"),
        "{body}"
    );
}

#[tokio::test]
async fn a_value_that_is_not_the_old_name_is_kept() {
    let (_dir, library) = library(&[(OLD, KEPT_SOURCE)]);
    let (is_error, body) = rename(&library, OLD, NEW).await;
    assert!(!is_error, "{body}");
    let written = std::fs::read_to_string(library.join(format!("{NEW}.kicad_mod"))).unwrap();
    assert_eq!(without_uuids(&written), without_uuids(KEPT));
    assert_eq!(body.get("value"), None, "{body}");
}

#[tokio::test]
async fn a_rename_that_changes_only_the_case_is_allowed() {
    let lower = SOURCE.replace(OLD, "ffc_fh12");
    let (_dir, library) = library(&[("ffc_fh12", &lower)]);
    let (is_error, body) = rename(&library, "ffc_fh12", "FFC_FH12").await;
    assert!(!is_error, "{body}");
    // The name and the Value change; the 3D model path, which holds the old
    // name too, does not.
    let expected = lower
        .replacen("(footprint \"ffc_fh12\"", "(footprint \"FFC_FH12\"", 1)
        .replacen(
            "(property \"Value\" \"ffc_fh12\"",
            "(property \"Value\" \"FFC_FH12\"",
            1,
        );
    let files = files(&library);
    assert_eq!(files.len(), 1);
    assert_eq!(files[0].0, "FFC_FH12.kicad_mod");
    assert!(files[0].1 == expected, "the renamed file differs");
    assert!(files[0].1.contains("ffc_fh12.step"));
}

#[tokio::test]
async fn a_name_already_in_the_library_is_refused() {
    let (_dir, library) = library(&[(OLD, SOURCE), ("Other", OTHER)]);
    let before = files(&library);
    for taken in ["Other", "OTHER", "other"] {
        let (is_error, body) = rename(&library, OLD, taken).await;
        assert!(is_error, "{taken}");
        assert_eq!(body["error"]["kind"], "invalid_argument", "{body}");
        assert_eq!(body["error"]["field"], "new_name", "{body}");
        assert_eq!(files(&library), before, "{taken}: nothing may change");
    }
}

#[tokio::test]
async fn a_name_kicad_cannot_use_is_refused() {
    let (_dir, library) = library(&[(OLD, SOURCE)]);
    let before = files(&library);
    let mut names = vec![
        "", " FFC", "FFC ", "a/b", "a\\b", "a:b", "a%b", "a$b", "a<b", "a>b", "a\"b", "a\tb",
        "a\nb",
    ];
    if cfg!(windows) {
        names.extend(["a*b", "a?b", "a|b", "NUL", "con.x", "Com1"]);
    }
    for name in names {
        let (is_error, body) = rename(&library, OLD, name).await;
        assert!(is_error, "{name:?}");
        assert_eq!(body["error"]["field"], "new_name", "{name:?}: {body}");
        assert_eq!(files(&library), before, "{name:?}: nothing may change");
    }
}

#[tokio::test]
async fn a_footprint_or_library_that_is_not_there_is_refused() {
    let (dir, library) = library(&[(OLD, SOURCE)]);
    let before = files(&library);

    let (_, body) = rename(&library, "Missing", NEW).await;
    assert_eq!(body["error"]["field"], "footprint_name", "{body}");
    let (_, body) = rename(&library, &OLD.to_lowercase(), NEW).await;
    assert!(
        body["error"]["reason"]
            .as_str()
            .unwrap()
            .contains("differs only in case"),
        "{body}"
    );
    let (_, body) = rename(&library, OLD, OLD).await;
    assert_eq!(body["error"]["field"], "new_name", "{body}");

    let (_, body) = rename(&dir.path().join("Missing.pretty"), OLD, NEW).await;
    assert_eq!(body["error"]["kind"], "file_not_found", "{body}");
    let (_, body) = rename(dir.path(), OLD, NEW).await;
    assert_eq!(body["error"]["kind"], "handler_error", "{body}");
    assert_eq!(files(&library), before);
}

#[tokio::test]
async fn a_pre_kicad_6_footprint_is_refused() {
    let module = "(module Old (layer F.Cu) (tedit 5B307E4C))\n";
    let (_dir, library) = library(&[("Old", module)]);
    let (is_error, body) = rename(&library, "Old", "New").await;
    assert!(is_error);
    assert!(body.to_string().contains("kicad-cli fp upgrade"), "{body}");
    assert_eq!(
        files(&library),
        [("Old.kicad_mod".to_string(), module.to_string())]
    );
}

/// The old file changed after it was read: the new file is taken back, and
/// the old one is left as the other program saved it.
#[tokio::test]
async fn a_footprint_saved_during_the_rename_is_left_and_nothing_renamed() {
    let (_dir, library) = library(&[(OLD, SOURCE)]);
    let saved = SOURCE.replace("(layer \"F.Cu\")", "(layer \"B.Cu\")");
    copperpour_sexp::writer::test_hooks::save_before_compare(
        &library.join(format!("{OLD}.kicad_mod")),
        saved.clone(),
    );
    let (is_error, body) = rename(&library, OLD, NEW).await;
    assert!(is_error);
    assert_eq!(body["error"]["kind"], "conflict", "{body}");
    assert_eq!(files(&library), [(format!("{OLD}.kicad_mod"), saved)]);
}

/// A rename that stopped after writing the new file is finished by the next
/// call, which removes the old file.
#[tokio::test]
async fn an_interrupted_rename_is_finished() {
    let renamed = renamed_source(SOURCE, OLD, NEW).unwrap().text;
    let (_dir, library) = library(&[(OLD, SOURCE), (NEW, &renamed)]);
    let (is_error, body) = rename(&library, OLD, NEW).await;
    assert!(!is_error, "{body}");
    assert_eq!(body["finished_earlier_rename"], true, "{body}");
    assert_eq!(files(&library), [(format!("{NEW}.kicad_mod"), renamed)]);

    // A new file that holds something else is a name in use.
    let (_other_dir, taken) = self::library(&[(OLD, SOURCE), (NEW, OTHER)]);
    let (_, body) = rename(&taken, OLD, NEW).await;
    assert_eq!(body["error"]["field"], "new_name", "{body}");
}

#[test]
fn an_older_footprint_s_value_text_is_renamed_too() {
    let old = "(footprint \"A\" (version 20210108) (layer \"F.Cu\")\n  (fp_text reference \"REF**\" (at 0 0) (layer \"F.SilkS\"))\n  (fp_text value \"A\" (at 0 1) (layer \"F.Fab\"))\n)\n";
    let renamed = renamed_source(old, "A", "B").unwrap();
    assert_eq!(
        renamed.text,
        old.replace("(footprint \"A\"", "(footprint \"B\"")
            .replace("(fp_text value \"A\"", "(fp_text value \"B\"")
    );
    assert_eq!(renamed.value, Some(("A".to_string(), "B".to_string())));
}
