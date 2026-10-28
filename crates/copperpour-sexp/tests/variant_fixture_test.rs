//! Guards the KiCad 10 variant-grammar fixture (fixtures/variants/) — the
//! evidence base for the upcoming variants toolset. The grammar was
//! adjudicated by kicad-cli acceptance (see the fixture README); these tests
//! keep the fixture parseable and its facts pinned, and the `#[ignore]` test
//! re-runs the adjudication against a live kicad-cli.

use copperpour_sexp::parse_sexp;
use std::path::PathBuf;

fn fixture(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/variants")
        .join(name)
}

fn schematic_source() -> String {
    std::fs::read_to_string(fixture("ecc83-pp.kicad_sch")).unwrap()
}

#[test]
fn variant_fixture_parses() {
    let tree = parse_sexp(&schematic_source()).expect("KiCad-authored fixture must parse");
    assert_eq!(tree.head(), Some("kicad_sch"));
}

/// The variant clause sits inside R1's instance path block, carrying the two
/// verified override kinds: population (dnp) and a field override (Value).
#[test]
fn fixture_carries_the_verified_variant_clause() {
    let source = schematic_source();
    let clause_start = source
        .find("(variant (name \"Lite\")")
        .expect("R1's Lite clause present");
    let clause = &source[clause_start..clause_start + 200];
    assert!(clause.contains("(dnp yes)"), "population override present");
    assert!(
        clause.contains("(field (name \"Value\") (value \"22k\"))"),
        "field override present — native variants carry field overrides"
    );

    // And it is inside R1's instances path, not floating elsewhere.
    let r1_path = source
        .find("(reference \"R1\")")
        .expect("R1 instance present");
    assert!(
        clause_start > r1_path && clause_start - r1_path < 300,
        "clause belongs to R1's instance block"
    );
}

#[test]
fn project_file_registers_the_variant_name() {
    let pro = std::fs::read_to_string(fixture("ecc83-pp.kicad_pro")).unwrap();
    let json: serde_json::Value = serde_json::from_str(&pro).unwrap();
    assert_eq!(json["variants"], serde_json::json!(["Lite"]));
}

/// Golden facts from the kicad-cli adjudication, pinned so a fixture edit
/// cannot silently invalidate the evidence the README cites.
#[test]
fn goldens_pin_the_adjudicated_facts() {
    let default = std::fs::read_to_string(fixture("bom_default.golden.csv")).unwrap();
    let lite = std::fs::read_to_string(fixture("bom_lite.golden.csv")).unwrap();
    assert!(
        default.contains("\"R1\",\"1.5K\",\"\""),
        "default: fitted, base value"
    );
    assert!(
        lite.contains("\"R1\",\"22k\",\"DNP\""),
        "Lite: DNP + overridden value"
    );
}

/// Live re-adjudication: kicad-cli must still honor the hand-authored clause.
/// Skips silently when kicad-cli is absent (conformance-oracle pattern).
#[test]
#[ignore = "requires an installed kicad-cli; run via e2e-kicad or locally"]
fn kicad_cli_still_honors_the_variant_clause() {
    let cli = ["C:/KiCad/10.0/bin/kicad-cli.exe", "kicad-cli"]
        .iter()
        .find(|c| {
            std::process::Command::new(c)
                .arg("--version")
                .output()
                .is_ok()
        })
        .copied();
    let Some(cli) = cli else {
        eprintln!("kicad-cli not found; skipping");
        return;
    };

    let dir = tempfile::tempdir().unwrap();
    for name in ["ecc83-pp.kicad_sch", "ecc83-pp.kicad_pro"] {
        std::fs::copy(fixture(name), dir.path().join(name)).unwrap();
    }
    let out = dir.path().join("bom.csv");
    let status = std::process::Command::new(cli)
        .args(["sch", "export", "bom", "--variant", "Lite", "--fields"])
        .arg("Reference,Value,${DNP}")
        .arg("--output")
        .arg(&out)
        .arg(dir.path().join("ecc83-pp.kicad_sch"))
        .status()
        .unwrap();
    assert!(status.success(), "kicad-cli refused the fixture");
    let bom = std::fs::read_to_string(&out).unwrap();
    assert!(
        bom.contains("\"R1\",\"22k\",\"DNP\""),
        "variant overrides no longer honored: {bom}"
    );
}
