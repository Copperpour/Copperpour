use super::{assert_verdict, compare, handler, Sheet};
use serde_json::json;

fn fixture(name: &str) -> Sheet {
    Sheet::input(&format!("layout_library_kicad10/{name}/metadata.kicad_sch"))
}

async fn expect_change(before: &str, after: &str, reason: &str) {
    let result = compare(&handler().await, &fixture(before), &fixture(after), &[]).await;
    assert_eq!(
        result["reasons"],
        json!([format!(
            "symbol cfd52ef9-c5e5-42c8-8e6e-96fd2851ffb8 (U1) {reason}"
        )]),
        "{result}"
    );
    assert_verdict(&result, "blocked");
}

macro_rules! case {
    ($test:ident, $before:literal => $after:literal, $reason:literal) => {
        #[tokio::test]
        async fn $test() {
            expect_change($before, $after, $reason).await;
        }
    };
    ($test:ident, $after:literal, $reason:literal) => {
        case!($test, "baseline" => $after, $reason);
    };
}

case!(
    duplicate_jumper_flag,
    "duplicate-jumper",
    "changed duplicate_pin_numbers_are_jumpers"
);
case!(jumper_groups, "jumper-group", "changed jumper pin groups");
case!(
    unplaced_unit_name,
    "unit-name",
    "changed embedded unit name in \"LM358_2_1\""
);
case!(
    unplaced_pin_number,
    "unit-pin-number",
    "changed embedded pin records in \"LM358_2_1\""
);
case!(
    unplaced_pin_position,
    "pin-unit-x-order",
    "changed embedded pin records in \"LM358_3_1\""
);
case!(
    unit_membership,
    "pin-unit-membership",
    "changed embedded unit or body style records"
);
case!(
    library_field,
    "libpart-field",
    "changed embedded library fields"
);
case!(
    base_pin_name,
    "libpart-pin-name",
    "changed embedded pin records in \"LM358_3_1\""
);
case!(
    lib_name_identity,
    "lib-name",
    "changed embedded library identity (lib_name)"
);
case!(
    alias_pin_type,
    "lib-name" => "alias-pin-type",
    "changed embedded pin records in \"LM358_1_3_1\""
);

#[tokio::test]
async fn drawing_and_pin_text_keep_metadata_comparable() {
    let handler = handler().await;
    let before = fixture("baseline");
    for candidate in ["drawing", "pin-text-style"] {
        let result = compare(&handler, &before, &fixture(candidate), &[]).await;
        assert_eq!(result["reasons"], json!([]), "{candidate}: {result}");
        assert_ne!(result["verdict"], "blocked", "{candidate}: {result}");
    }
}

#[test]
fn cleanup_gate_does_not_adopt_the_layout_export_guard() {
    let before = fixture("baseline").tree();
    let after = fixture("unit-name").tree();
    assert!(super::super::cleanup_gate(&before, &after, &[]).is_empty());
}

#[tokio::test]
async fn different_schematic_formats_cannot_certify_equal_embedded_metadata() {
    let baseline = fixture("mixed-version-legacy");
    let candidate = fixture("mixed-version-modern");
    let result = compare(&handler().await, &baseline, &candidate, &[]).await;
    assert_eq!(
        result["reasons"],
        json!(["cannot compare embedded symbol metadata across schematic format versions (baseline 20250114, candidate 20260306)"]),
        "{result}"
    );
    assert_verdict(&result, "blocked");
}

#[tokio::test]
async fn schematic_format_refusal_is_reported_once_for_multiple_symbols() {
    let baseline = Sheet::input("block_placement_kicad10/opamp_stage/opamp_stage.kicad_sch");
    assert!(baseline.tree().find_all("symbol").len() > 1);
    let candidate = baseline.fork();
    // Isolate the diagnostic count. The native pair above tests pin-name semantics.
    let saved = String::from_utf8(baseline.bytes()).unwrap();
    let edited = saved.replacen("(version 20260306)", "(version 20260305)", 1);
    assert_ne!(saved, edited);
    std::fs::write(&candidate.path, edited).unwrap();
    let result = compare(&handler().await, &baseline, &candidate, &[]).await;
    assert_eq!(
        result["reasons"],
        json!(["cannot compare embedded symbol metadata across schematic format versions (baseline 20260306, candidate 20260305)"]),
        "{result}"
    );
    assert_verdict(&result, "blocked");
}
