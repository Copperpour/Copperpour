//! `get_symbol_info` lists every pin with its unit and body style, counts pins
//! as KiCad does, and warns about duplicates by KiCad's checker rule (#712).

use super::*;
use crate::router::ToolRouter;
use crate::tools::{ServerConfig, ToolContext};
use std::sync::Arc;

// kicad-cli 10.0.8 saved it from symbols drawn for these tests (no stock
// symbol); provenance in tests/fixtures/symbol_pins_kicad10.README.md.
const LIBRARY: &str = include_str!("../../tests/fixtures/symbol_pins_kicad10.kicad_sym");

async fn info_in(library: &std::path::Path, symbol: &str) -> serde_json::Value {
    let ctx = ToolContext::new(ServerConfig::default(), Arc::new(ToolRouter::new()));
    let result = handle_get_symbol_info(
        &json!({ "library_path": library, "symbol_name": symbol }),
        &ctx,
    )
    .await
    .unwrap();
    assert!(!result.is_error, "{:?}", result.content);
    serde_json::from_str(&result_text(&result)).unwrap()
}

async fn info(symbol: &str) -> serde_json::Value {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("symbol_pins.kicad_sym");
    std::fs::write(&path, LIBRARY).unwrap();
    info_in(&path, symbol).await
}

fn result_text(result: &CallToolResult) -> String {
    match result.content.first() {
        Some(crate::mcp::protocol::ToolContent::Text { text }) => text.clone(),
        other => panic!("expected text, got {other:?}"),
    }
}

fn pins_numbered<'a>(body: &'a serde_json::Value, number: &str) -> Vec<&'a serde_json::Value> {
    body["pins"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|pin| pin["number"] == number)
        .collect()
}

/// A De Morgan symbol lists each pin once per body style, and its power unit
/// once for both; KiCad counts body style 1 and 0 only.
#[tokio::test]
async fn a_de_morgan_symbol_lists_every_pin_and_counts_one_body_style() {
    let body = info("GATE").await;
    assert_eq!(body["pin_count"], 5, "3 in body style 1, 2 common: {body}");
    assert_eq!(body["pins"].as_array().unwrap().len(), 8);
    let pin_1 = pins_numbered(&body, "1");
    let places: Vec<_> = pin_1
        .iter()
        .map(|pin| (&pin["unit"], &pin["body_style"]))
        .collect();
    assert_eq!(places, [(&json!(1), &json!(1)), (&json!(1), &json!(2))]);
    let supply = pins_numbered(&body, "14");
    assert_eq!(
        (&supply[0]["unit"], &supply[0]["body_style"]),
        (&json!(2), &json!(0))
    );
    assert!(body.get("warnings").is_none(), "{body}");
}

/// A derived symbol shows its root's pins, as KiCad does.
#[tokio::test]
async fn a_derived_symbol_shows_its_roots_pins() {
    let (derived, root) = (info("GATE_ALT").await, info("GATE").await);
    assert_eq!(derived["pins"], root["pins"]);
    assert_eq!(derived["pin_count"], 5);
}

/// A pin drawn in two units under one number counts once per drawing, as
/// KiCad's `GetPinCount` counts it, and its checker reports it.
#[tokio::test]
async fn a_pin_shared_by_two_units_counts_twice_and_is_warned() {
    let body = info("SHARED").await;
    assert_eq!(body["pin_count"], 6, "{body}");
    assert_eq!(pins_numbered(&body, "1").len(), 2);
    assert_eq!(
        body["warnings"],
        json!([
            "duplicate pin number 1: pin 1 'C' at (-7.62, 0) in unit 1, body style 1 and \
             pin 1 'C' at (-7.62, 0) in unit 2, body style 1; KiCad's symbol checker reports them"
        ])
    );
}

/// A stacked number counts once for each pin it stands for.
#[tokio::test]
async fn a_stacked_number_counts_each_pin() {
    let body = info("STACKED").await;
    assert_eq!(body["pin_count"], 7, "1 + [2-4] + 5 + [6,8]: {body}");
    assert_eq!(body["pins"].as_array().unwrap().len(), 4);
    assert!(body.get("warnings").is_none(), "{body}");
}

/// Two pins with one number in one unit and body style: both are listed and
/// counted, and the duplicate is warned (it was hidden: one pin, a count of 3).
#[tokio::test]
async fn a_duplicate_pin_is_listed_counted_and_warned() {
    let body = info("DUP").await;
    assert_eq!(body["pin_count"], 4, "{body}");
    let pin_2 = pins_numbered(&body, "2");
    let found: Vec<_> = pin_2
        .iter()
        .map(|pin| (pin["x"].as_f64().unwrap(), pin["type"].clone()))
        .collect();
    assert_eq!(
        found,
        [(5.08, json!("no_connect")), (6.35, json!("passive"))]
    );
    assert_eq!(
        body["warnings"],
        json!([
            "duplicate pin number 2: pin 2 'NC' at (5.08, 2.54) in unit 1, body style 1 and \
             pin 2 'NC' at (6.35, 2.54) in unit 1, body style 1; KiCad's symbol checker reports them"
        ])
    );

    let clean = info("PLAIN").await;
    assert_eq!(clean["pin_count"], 2);
    assert!(clean.get("warnings").is_none(), "{clean}");
}

/// The examples agreed on #712, on KiCad's own installed libraries, read
/// rather than committed because they are CC BY-SA: `74LS00` keeps 14,
/// `74LS78` (clock and clear in both units) reports 16 with two warnings,
/// `TL431D` (`[2,3,6,7]`) reports 6. Skips without an installed KiCad.
#[tokio::test]
async fn kicads_stock_examples_count_as_agreed() {
    let Some(dir) = super::super::symbol_fields::tests::installed_symbol_dir() else {
        eprintln!("SKIP: no installed KiCad symbol library");
        return;
    };
    let (gates, voltage) = (
        dir.join("74xx.kicad_sym"),
        dir.join("Reference_Voltage.kicad_sym"),
    );
    if !gates.is_file() || !voltage.is_file() {
        eprintln!("SKIP: 74xx or Reference_Voltage is not installed");
        return;
    }
    let ls00 = info_in(&gates, "74LS00").await;
    assert_eq!(
        (&ls00["pin_count"], ls00["pins"].as_array().unwrap().len()),
        (&json!(14), 26)
    );
    assert!(ls00.get("warnings").is_none(), "{ls00}");
    let ls78 = info_in(&gates, "74LS78").await;
    assert_eq!(ls78["pin_count"], 16, "{ls78}");
    let warned: Vec<&str> = ls78["warnings"]
        .as_array()
        .unwrap()
        .iter()
        .map(|w| w.as_str().unwrap())
        .collect();
    assert_eq!(warned.len(), 2, "{warned:?}");
    assert!(
        warned[0].starts_with("duplicate pin number 1: "),
        "{warned:?}"
    );
    assert!(
        warned[1].starts_with("duplicate pin number 5: "),
        "{warned:?}"
    );
    let tl431 = info_in(&voltage, "TL431D").await;
    assert_eq!(tl431["pin_count"], 6, "{tl431}");
}

/// KiCad's `CountStackedPinNotation`: a run counts its length, invalid
/// notation counts 1.
#[test]
fn stacked_numbers_count_as_kicad_counts_them() {
    for (number, count) in [
        ("5", 1),
        ("[2,3,6,7]", 4),
        ("[1-3]", 3),
        ("[A1-A3, B2]", 4),
        ("[1,1]", 2),
        ("[1-1000000000]", 1_000_000_000),
        ("[3-1]", 1),
        ("[A1-B3]", 1),
        ("[1,2", 1),
        ("[]", 1),
        ("[ , ]", 1),
    ] {
        assert_eq!(stacked_pin_count(number), count, "{number}");
    }
}

/// KiCad's checker: one number in two pins is a duplicate unless both body
/// styles are set and differ; a stacked number takes part with each pin.
#[test]
fn duplicates_follow_kicads_checker_rule() {
    let warned = |pins: &[(&str, u32, u32)]| {
        let nodes: Vec<SexpNode> = pins
            .iter()
            .map(|(number, _, _)| {
                parse_sexp(&format!(
                    "(pin passive line (at 0 0 0) (name \"P\") (number \"{number}\"))"
                ))
                .unwrap()
            })
            .collect();
        let pins: Vec<SymbolPin> = nodes
            .iter()
            .zip(pins)
            .map(|(node, (_, unit, body_style))| SymbolPin {
                node,
                unit: Some(*unit),
                body_style: Some(*body_style),
            })
            .collect();
        duplicate_pin_warnings(&pins)
    };
    assert!(
        warned(&[("1", 1, 1), ("1", 1, 2)]).is_empty(),
        "two body styles"
    );
    assert_eq!(
        warned(&[("1", 1, 0), ("1", 1, 2)]).len(),
        1,
        "0 is in every style"
    );
    assert_eq!(
        warned(&[("1", 1, 1), ("1", 2, 1)]).len(),
        1,
        "units make no difference"
    );
    let stacked = warned(&[("[1-3]", 1, 1), ("2", 1, 1)]);
    assert_eq!(stacked.len(), 1, "{stacked:?}");
    assert!(stacked[0].contains("pin 2 ([1-3])"), "{stacked:?}");
    assert!(warned(&[("1", 1, 1), ("2", 1, 1)]).is_empty());
    // KiCad skips a pair of one pin with itself (`prev.pin == next.pin`).
    assert!(warned(&[("[1,1]", 1, 1)]).is_empty(), "one pin with itself");
    // Runs are compared as intervals: one warning per overlapping range.
    let runs = warned(&[
        ("[1-1000000000]", 1, 1),
        ("[500-2000000000]", 1, 1),
        ("7", 1, 1),
    ]);
    assert_eq!(runs.len(), 2, "{runs:?}");
    assert!(
        runs[0].contains("duplicate pin numbers 500-1000000000:"),
        "{runs:?}"
    );
    assert!(runs[1].contains("duplicate pin number 7:"), "{runs:?}");
    // A leading zero is not a number KiCad's runs write.
    assert!(warned(&[("[1-3]", 1, 1), ("02", 1, 1)]).is_empty());
}

/// A pin written directly under a symbol is in unit 1, body style 1, as KiCad's
/// parser reads it, so it clashes with a pin of that number in `NAME_1_1`.
#[test]
fn a_pin_directly_under_the_symbol_is_in_unit_one_body_style_one() {
    let library = parse_sexp(
        "(kicad_symbol_lib (symbol \"S\" \
           (pin passive line (at 0 0 0) (name \"A\") (number \"1\")) \
           (symbol \"S_1_1\" (pin passive line (at 2.54 0 0) (name \"B\") (number \"1\")))))",
    )
    .unwrap();
    let symbol = &library.find_all("symbol")[0];
    let pins = resolve_symbol_pins(&library, symbol);
    assert_eq!(
        pins.iter()
            .map(|pin| (pin.unit, pin.body_style))
            .collect::<Vec<_>>(),
        [(Some(1), Some(1)), (Some(1), Some(1))]
    );
    assert_eq!(duplicate_pin_warnings(&pins).len(), 1);
}

/// A long stacked run answers at once: nothing is expanded (#827's review).
#[tokio::test]
async fn a_long_stacked_run_is_counted_without_expanding() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("symbol_pins.kicad_sym");
    assert_eq!(LIBRARY.matches("(number \"[2-4]\"").count(), 1);
    std::fs::write(
        &path,
        LIBRARY.replace("(number \"[2-4]\"", "(number \"[1-1000000000]\""),
    )
    .unwrap();
    let started = std::time::Instant::now();
    let body = info_in(&path, "STACKED").await;
    assert_eq!(
        body["pin_count"], 1_000_000_004_u64,
        "1 + [1-1000000000] + 5 + [6,8]"
    );
    assert!(
        started.elapsed() < std::time::Duration::from_secs(10),
        "{:?}",
        started.elapsed()
    );
}
