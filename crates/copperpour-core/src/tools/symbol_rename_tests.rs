use super::*;
use crate::router::ToolRouter;
use crate::tools::{ServerConfig, ToolContext};
use std::path::{Path, PathBuf};
use std::sync::Arc;

// KiCad 10.0.8 saved each of these; provenance in
// tests/fixtures/rename_symbol_kicad10.README.md.
const SOURCE: &str = include_str!("../../tests/fixtures/rename_symbol_source_kicad10.kicad_sym");
const RENAMED_R: &str = include_str!("../../tests/fixtures/rename_symbol_r_kicad10.kicad_sym");
const RENAMED_PARENT: &str =
    include_str!("../../tests/fixtures/rename_symbol_parent_kicad10.kicad_sym");
const RENAMED_UNITS: &str =
    include_str!("../../tests/fixtures/rename_symbol_units_kicad10.kicad_sym");

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

fn library(content: &str) -> (tempfile::TempDir, PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("Parts.kicad_sym");
    std::fs::write(&path, content).unwrap();
    (dir, path)
}

async fn rename(path: &Path, old: &str, new: &str) -> (bool, serde_json::Value) {
    let result = handle_rename_symbol(
        &json!({ "library_path": path, "symbol_name": old, "new_name": new }),
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

/// Each rename writes the library exactly as KiCad saves it once renamed.
#[tokio::test]
async fn a_symbol_is_renamed_as_kicad_saves_it() {
    let cases = [
        ("R", "A_Resistor", RENAMED_R),
        ("C_Feedthrough", "Z_Feedthrough", RENAMED_PARENT),
        ("74LS00", "SN74LS00", RENAMED_UNITS),
    ];
    for (old, new, kicad) in cases {
        let (_dir, path) = library(SOURCE);
        let (is_error, body) = rename(&path, old, new).await;
        assert!(!is_error, "{old}: {body}");
        let written = std::fs::read_to_string(&path).unwrap();
        assert!(
            written == kicad,
            "{old}: the library is not as KiCad saves it"
        );
        assert_eq!(body["previous_name"], old);
        assert_eq!(body["name"], new);
        assert_eq!(body["value"], json!({ "previous": old, "current": new }));
    }
}

#[tokio::test]
async fn derived_symbols_follow_and_the_answer_says_what_moved() {
    let (_dir, path) = library(SOURCE);
    let (_, body) = rename(&path, "C_Feedthrough", "Z_Feedthrough").await;
    assert_eq!(body["derived_symbols"], json!(["Filter_EMI_C"]));
    assert!(body["moved"].is_string(), "{body}");
    assert!(
        body["kept_references"]
            .as_str()
            .unwrap()
            .contains("C_Feedthrough"),
        "{body}"
    );

    // Digits sort before letters, so 74LS00 moves after R as SN74LS00.
    let (_dir, path) = library(SOURCE);
    let (_, body) = rename(&path, "74LS00", "SN74LS00").await;
    assert_eq!(body["derived_symbols"], json!([]));
    assert!(body["moved"].is_string(), "{body}");

    let (_dir, path) = library(SOURCE);
    let (_, body) = rename(&path, "R", "R_Renamed").await;
    assert_eq!(body.get("moved"), None, "{body}");
}

#[tokio::test]
async fn a_value_that_is_not_the_old_name_is_kept() {
    let source = SOURCE.replacen(
        "(property \"Value\" \"R\"",
        "(property \"Value\" \"10k\"",
        1,
    );
    let (_dir, path) = library(&source);
    let (is_error, body) = rename(&path, "R", "A_Resistor").await;
    assert!(!is_error, "{body}");
    assert_eq!(body.get("value"), None, "{body}");
    let written = std::fs::read_to_string(&path).unwrap();
    assert!(written.contains("(property \"Value\" \"10k\""));
    assert!(written.contains("(symbol \"A_Resistor_0_1\""));
}

#[tokio::test]
async fn a_name_is_stored_escaped_as_kicad_stores_it() {
    let (_dir, path) = library(SOURCE);
    let (is_error, body) = rename(&path, "R", "R:1<2>").await;
    assert!(!is_error, "{body}");
    assert_eq!(body["name"], "R{colon}1{lt}2{gt}");
    assert!(body["escaped"].is_string(), "{body}");
    let written = std::fs::read_to_string(&path).unwrap();
    assert!(written.contains("(symbol \"R{colon}1{lt}2{gt}\""));
    assert!(written.contains("(symbol \"R{colon}1{lt}2{gt}_0_1\""));
    assert!(written.contains("(property \"Value\" \"R{colon}1{lt}2{gt}\""));
}

#[tokio::test]
async fn names_in_use_or_unusable_are_refused() {
    let (dir, path) = library(SOURCE);
    for (old, new, field) in [
        ("R", "74LS00", "new_name"),
        ("R", "R", "new_name"),
        ("R", "", "new_name"),
        ("R", " R2", "new_name"),
        ("R", "R2 ", "new_name"),
        ("R", "R\t2", "new_name"),
        ("R", "\n", "new_name"),
        ("Missing", "R2", "symbol_name"),
    ] {
        let (is_error, body) = rename(&path, old, new).await;
        assert!(is_error, "{old:?} -> {new:?}");
        assert_eq!(body["error"]["field"], field, "{old:?} -> {new:?}: {body}");
        assert_eq!(std::fs::read_to_string(&path).unwrap(), SOURCE);
    }

    // The symbol's own name is not a name in use by another symbol.
    let (_, body) = rename(&path, "R", "R").await;
    assert!(
        body["error"]["reason"]
            .as_str()
            .unwrap()
            .contains("already the symbol's name"),
        "{body}"
    );

    let (_, body) = rename(&dir.path().join("Missing.kicad_sym"), "R", "R2").await;
    assert_eq!(body["error"]["kind"], "file_not_found", "{body}");
    let (_, body) = rename(&dir.path().join("Parts.lib"), "R", "R2").await;
    assert_eq!(body["error"]["kind"], "handler_error", "{body}");
}

/// A library KiCad did not save in its order keeps the order it has.
#[tokio::test]
async fn a_library_in_another_order_keeps_it() {
    let unordered = "(kicad_symbol_lib\n\t(version 20251024)\n\t(symbol \"Z\"\n\t\t(symbol \"Z_0_1\")\n\t)\n\t(symbol \"A\"\n\t\t(symbol \"A_0_1\")\n\t)\n)\n";
    let (_dir, path) = library(unordered);
    let (is_error, body) = rename(&path, "Z", "B").await;
    assert!(!is_error, "{body}");
    assert_eq!(
        std::fs::read_to_string(&path).unwrap(),
        unordered
            .replace("\"Z\"", "\"B\"")
            .replace("\"Z_0_1\"", "\"B_0_1\"")
    );
    assert_eq!(body.get("moved"), None, "{body}");

    let ordered = unordered
        .replace("\"Z\"", "\"Y\"")
        .replace("\"Z_0_1\"", "\"Y_0_1\"")
        .replace("\"A\"", "\"Z\"")
        .replace("\"A_0_1\"", "\"Z_0_1\"");
    let (_dir, path) = library(&ordered);
    let (_, body) = rename(&path, "Z", "A").await;
    assert!(body["moved"].is_string(), "{body}");
    assert_eq!(
        std::fs::read_to_string(&path).unwrap(),
        "(kicad_symbol_lib\n\t(version 20251024)\n\t(symbol \"A\"\n\t\t(symbol \"A_0_1\")\n\t)\n\t(symbol \"Y\"\n\t\t(symbol \"Y_0_1\")\n\t)\n)\n"
    );
}

#[tokio::test]
async fn a_unit_block_not_named_after_its_symbol_is_refused() {
    let broken = "(kicad_symbol_lib\n\t(version 20251024)\n\t(symbol \"A\"\n\t\t(symbol \"B_0_1\")\n\t)\n)\n";
    let (_dir, path) = library(broken);
    let (is_error, body) = rename(&path, "A", "C").await;
    assert!(is_error);
    assert_eq!(body["error"]["kind"], "handler_error", "{body}");
    assert_eq!(std::fs::read_to_string(&path).unwrap(), broken);
}

#[test]
fn names_are_escaped_as_kicad_escapes_them() {
    assert_eq!(stored_name("a\\b").unwrap(), "a{backslash}b");
    assert_eq!(stored_name("a\"b").unwrap(), "a{dblquote}b");
    assert_eq!(stored_name("a/b").unwrap(), "a/b");
    assert_eq!(stored_name("a\r\nb").unwrap(), "ab");
    assert!(stored_name("\r\n").is_err());
}
