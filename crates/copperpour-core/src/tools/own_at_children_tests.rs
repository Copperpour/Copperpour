//! Editors that change an item's position edit the item's own `(at …)`,
//! `(effects …)` and `(justify …)`, never the first text match (#94).
//!
//! `rotate_schematic_label` and `bulk_move_schematic_components` found these
//! by searching the item's raw text, so a name or field value holding `(at `
//! was rewritten in place of the position, and the call reported success.
//! Driven through served `tools/call`. The sheet is the KiCad-saved
//! `label_move_kicad10.kicad_sch` (see its README): a label named `(at 1 2)`,
//! a `PWR` global label with its Intersheetrefs field, and R1.

use crate::tools::project_file_writer_tests::{body_of, handler_talking_to, served_call};
use copperpour_sexp::SexpNode;
use serde_json::{json, Value};
use std::path::{Path, PathBuf};

const SHEET: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/tests/fixtures/label_move_kicad10.kicad_sch"
);

fn sheet_copy(edit: impl FnOnce(String) -> String) -> (tempfile::TempDir, PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    let sheet = dir.path().join("label_move_kicad10.kicad_sch");
    let source = std::fs::read_to_string(SHEET).unwrap();
    let edited = edit(source.clone());
    std::fs::write(&sheet, edited).unwrap();
    (dir, sheet)
}

async fn call(tool: &str, sheet: &Path, mut arguments: Value) -> Value {
    arguments["schematic"] = json!(sheet.to_string_lossy());
    let result = served_call(&handler_talking_to("").await, tool, arguments).await;
    let body = body_of(&result);
    assert!(!result.is_error, "{tool}: {body}");
    body
}

fn tree(sheet: &Path) -> SexpNode {
    copperpour_sexp::parse_sexp(&std::fs::read_to_string(sheet).unwrap()).unwrap()
}

/// The direct `(at …)` child of a node, as `(x, y, rotation)`.
fn at_of(node: &SexpNode) -> (f64, f64, f64) {
    let at = node.find("at").unwrap();
    (
        at.get_f64(1).unwrap(),
        at.get_f64(2).unwrap(),
        at.get_f64(3).unwrap_or(0.0),
    )
}

/// The first word of a node's own `(effects (justify …))`, if it has one.
fn justify_of(node: &SexpNode) -> Option<String> {
    let justify = node.find("effects")?.find("justify")?;
    justify.get(1)?.as_str().map(str::to_string)
}

/// A label named `(at 1 2)`: rotating it turns the label and keeps the name.
#[tokio::test]
async fn rotate_label_keeps_a_name_that_holds_at() {
    let (_dir, sheet) = sheet_copy(|source| source);

    call(
        "rotate_schematic_label",
        &sheet,
        json!({ "net": "(at 1 2)", "x": 114.3, "y": 97.79, "rotation": 180 }),
    )
    .await;

    let tree = tree(&sheet);
    let labels = tree.find_all("label");
    let label = labels
        .iter()
        .find(|label| label.get(1).and_then(SexpNode::as_str) == Some("(at 1 2)"))
        .expect("the label kept its name");
    assert_eq!(at_of(label), (114.3, 97.79, 180.0));
    assert_eq!(justify_of(label).as_deref(), Some("right"));
}

/// The label's name holds `(effects (justify left))`: the justify written is
/// the one in the label's own effects, and the name is kept.
#[tokio::test]
async fn rotate_label_keeps_a_name_that_holds_effects() {
    let name = "(effects (justify left))";
    let (_dir, sheet) = sheet_copy(|source| {
        source.replacen("(label \"(at 1 2)\"", &format!("(label \"{name}\""), 1)
    });

    call(
        "rotate_schematic_label",
        &sheet,
        json!({ "net": name, "x": 114.3, "y": 97.79, "rotation": 180 }),
    )
    .await;

    let tree = tree(&sheet);
    let labels = tree.find_all("label");
    let label = labels
        .iter()
        .find(|label| label.get(1).and_then(SexpNode::as_str) == Some(name))
        .expect("the label kept its name");
    assert_eq!(at_of(label), (114.3, 97.79, 180.0));
    assert_eq!(justify_of(label).as_deref(), Some("right"));
}

/// A label with no effects of its own, the shape `add_schematic_net_label`
/// used to write, named `(uuid 1)`: the effects block the rotation adds goes
/// before the label's own `(uuid …)`, not into its name.
#[tokio::test]
async fn rotate_label_without_effects_keeps_a_name_that_holds_uuid() {
    let name = "(uuid 1)";
    let (_dir, sheet) = sheet_copy(|source| {
        source.replacen(
            "(label \"(at 1 2)\"\n\t\t(at 114.3 97.79 0)\n\t\t(effects\n\t\t\t(font\n\t\t\t\t(size 1.27 1.27)\n\t\t\t)\n\t\t\t(justify left bottom)\n\t\t)\n",
            &format!("(label \"{name}\"\n\t\t(at 114.3 97.79 0)\n"),
            1,
        )
    });
    let before = tree(&sheet);
    let label = before
        .find_all("label")
        .into_iter()
        .find(|label| label.get(1).and_then(SexpNode::as_str) == Some(name))
        .expect("the fixture edit renamed the label");
    assert!(
        label.find("effects").is_none(),
        "the fixture edit removed them"
    );

    call(
        "rotate_schematic_label",
        &sheet,
        json!({ "net": name, "x": 114.3, "y": 97.79, "rotation": 180 }),
    )
    .await;

    let tree = tree(&sheet);
    let labels = tree.find_all("label");
    let label = labels
        .iter()
        .find(|label| label.get(1).and_then(SexpNode::as_str) == Some(name))
        .expect("the label kept its name");
    assert_eq!(at_of(label), (114.3, 97.79, 180.0));
    assert_eq!(justify_of(label).as_deref(), Some("right"));
    assert_eq!(
        label
            .find("uuid")
            .and_then(|uuid| uuid.get(1))
            .and_then(SexpNode::as_str),
        Some("3031ddf4-4f23-44fc-bceb-58bba2ec8575")
    );
}

/// A global label whose own effects carry no justify, beside an Intersheetrefs
/// field whose effects do: the justify goes into the label's own effects, and
/// the field's is left as it was.
#[tokio::test]
async fn rotate_global_label_turns_its_own_text_not_its_intersheetrefs() {
    let (_dir, sheet) = sheet_copy(|source| {
        let label = source.find("(global_label \"PWR\"").unwrap();
        let (head, tail) = source.split_at(label);
        let tail = tail.replacen("\t\t\t(justify left)\n", "", 1).replacen(
            "\t\t\t\t(size 1.27 1.27)\n\t\t\t\t)\n\t\t\t)\n\t\t)\n\t)",
            "\t\t\t\t(size 1.27 1.27)\n\t\t\t\t)\n\t\t\t\t(justify left)\n\t\t\t)\n\t\t)\n\t)",
            1,
        );
        format!("{head}{tail}")
    });
    let before = tree(&sheet);
    let global = before.find("global_label").unwrap();
    assert_eq!(justify_of(global), None, "the fixture edit removed it");
    assert_eq!(
        justify_of(global.find("property").unwrap()).as_deref(),
        Some("left"),
        "the fixture edit moved it into Intersheetrefs"
    );

    call(
        "rotate_schematic_label",
        &sheet,
        json!({ "net": "PWR", "x": 114.3, "y": 105.41, "rotation": 180 }),
    )
    .await;

    let after = tree(&sheet);
    let global = after.find("global_label").unwrap();
    assert_eq!(at_of(global), (114.3, 105.41, 180.0));
    assert_eq!(justify_of(global).as_deref(), Some("right"));
    let refs = global.find("property").unwrap();
    assert_eq!(justify_of(refs).as_deref(), Some("left"));
    assert_eq!(at_of(refs), (124.46, 101.6, 0.0));
}

/// R1's Value is `(at 1 2)`: a move shifts the field and keeps the value.
#[tokio::test]
async fn bulk_move_keeps_a_value_that_holds_at() {
    let (_dir, sheet) = sheet_copy(|source| {
        source.replacen(
            "(property \"Value\" \"10k\"",
            "(property \"Value\" \"(at 1 2)\"",
            1,
        )
    });
    let r1 = |sheet: &Path| {
        let tree = tree(sheet);
        let symbol = tree
            .find_all("symbol")
            .into_iter()
            .find(|symbol| symbol.find("lib_id").is_some())
            .unwrap()
            .clone();
        let value = symbol
            .find_all("property")
            .into_iter()
            .find(|p| p.get(1).and_then(SexpNode::as_str) == Some("Value"))
            .unwrap()
            .clone();
        let text = value.get(2).and_then(SexpNode::as_str).unwrap().to_string();
        (at_of(&symbol), text, at_of(&value))
    };
    let (symbol_before, _, field_before) = r1(&sheet);

    let body = call(
        "bulk_move_schematic_components",
        &sheet,
        json!({ "references": ["R1"], "dx": 2.54, "dy": 0 }),
    )
    .await;

    assert_eq!(body["moved_count"], 1, "{body}");
    let (symbol_after, value, field_after) = r1(&sheet);
    assert_eq!(value, "(at 1 2)");
    assert!((symbol_after.0 - symbol_before.0 - 2.54).abs() < 1e-6);
    assert!((field_after.0 - field_before.0 - 2.54).abs() < 1e-6);
    assert_eq!(field_after.1, field_before.1);
    assert_eq!(field_after.2, field_before.2);
}

/// R1's `lib_id` holds `(at 1 2)` ahead of the symbol's own `(at …)`: a move
/// shifts the symbol and keeps the `lib_id`.
#[tokio::test]
async fn bulk_move_keeps_a_lib_id_that_holds_at() {
    let lib_id = "Device:R(at 1 2)";
    let (_dir, sheet) =
        sheet_copy(|source| source.replace("\"Device:R\"", &format!("\"{lib_id}\"")));
    let r1 = |sheet: &Path| {
        let tree = tree(sheet);
        let symbol = tree
            .find_all("symbol")
            .into_iter()
            .find(|symbol| symbol.find("lib_id").is_some())
            .unwrap()
            .clone();
        let id = symbol.find_str("lib_id").unwrap().to_string();
        (at_of(&symbol), id)
    };
    let (before, id) = r1(&sheet);
    assert_eq!(id, lib_id, "the fixture edit renamed it");

    let body = call(
        "bulk_move_schematic_components",
        &sheet,
        json!({ "references": ["R1"], "dx": 2.54, "dy": 0 }),
    )
    .await;

    assert_eq!(body["moved_count"], 1, "{body}");
    let (after, id) = r1(&sheet);
    assert_eq!(id, lib_id);
    assert!(
        (after.0 - before.0 - 2.54).abs() < 1e-6,
        "{before:?} -> {after:?}"
    );
    assert_eq!(after.1, before.1);
}
