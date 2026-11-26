use super::*;

async fn edit(handler: &McpHandler, sheet: &Sheet, mut args: Value) {
    args["reference"] = json!("R1");
    ok(handler, sheet, "edit_schematic_component", args).await;
}

async fn assert_property_change(was: &Sheet, now: &Sheet, handler: &McpHandler, property: &str) {
    let result = compare(handler, was, now, &[]).await;
    assert_verdict(&result, "blocked");
    assert_eq!(
        result["reasons"],
        json!([format!(
            "symbol {} (R1) changed {property}",
            symbol_uuid(was, "R1")
        )])
    );
}

macro_rules! blocked_edit {
    ($test:ident, $args:expr, $property:literal) => {
        #[tokio::test]
        async fn $test() {
            let handler = handler().await;
            let was = Sheet::input(SPARSE);
            let now = was.fork();
            edit(&handler, &now, $args).await;
            assert_property_change(&was, &now, &handler, $property).await;
        }
    };
}

blocked_edit!(
    changing_footprint,
    json!({"footprint": "Resistor_SMD:R_0805_2012Metric"}),
    "field \"Footprint\""
);
blocked_edit!(changing_value, json!({"value": "22k"}), "field \"Value\"");
blocked_edit!(changing_dnp, json!({"dnp": true}), "attribute dnp");
blocked_edit!(
    changing_bom_membership,
    json!({"in_bom": false}),
    "attribute in_bom"
);
blocked_edit!(
    changing_datasheet,
    json!({"datasheet": "https://example.invalid/r1.pdf"}),
    "field \"Datasheet\""
);
blocked_edit!(
    changing_description,
    json!({"fields": {"Description": "Audit description"}}),
    "field \"Description\""
);
blocked_edit!(
    adding_mpn,
    json!({"fields": {"MPN": "AUDIT-10K"}}),
    "field \"MPN\""
);
blocked_edit!(
    adding_manufacturer,
    json!({"fields": {"Manufacturer": "Audit Components"}}),
    "field \"Manufacturer\""
);
blocked_edit!(
    adding_bom_comments,
    json!({"fields": {"BOM Comments": "Audit assembly note"}}),
    "field \"BOM Comments\""
);
blocked_edit!(
    adding_custom_field_named_like_native_sim_flag,
    json!({"fields": {"EXCLUDE_FROM_SIM": "yes"}}),
    "field \"EXCLUDE_FROM_SIM\""
);

#[tokio::test]
async fn custom_field_value_change_and_removal_are_not_layout() {
    let handler = handler().await;
    let was = Sheet::input(SPARSE);
    edit(&handler, &was, json!({"fields": {"MPN": "AUDIT-10K"}})).await;
    for change in [
        json!({"fields": {"MPN": "AUDIT-22K"}}),
        json!({"remove_fields": ["MPN"]}),
    ] {
        let now = was.fork();
        edit(&handler, &now, change).await;
        assert_property_change(&was, &now, &handler, "field \"MPN\"").await;
    }
}

#[tokio::test]
async fn field_text_changes_do_not_change_exported_properties() {
    let handler = handler().await;
    let was = Sheet::input(SPARSE);
    let now = was.fork();
    edit(
        &handler,
        &now,
        json!({"field_placements": {
            "Value": {"x": 40.64, "y": 38.1, "rotation": 90},
            "Reference": {"hide": true}
        }}),
    )
    .await;
    assert_eq!(
        compare(&handler, &was, &now, &[]).await["reasons"],
        json!([])
    );
}

fn native_sheet(name: &str) -> Sheet {
    let basename = if name.starts_with("parent-") {
        "nested_pages"
    } else {
        "sparse"
    };
    Sheet::input(&format!(
        "layout_properties_kicad10/{name}/{basename}.kicad_sch"
    ))
}

fn replace_once(sheet: &Sheet, from: &str, to: &str) {
    let source = fs::read_to_string(&sheet.path).unwrap();
    assert_eq!(source.matches(from).count(), 1, "{from}");
    fs::write(&sheet.path, source.replacen(from, to, 1)).unwrap();
}

#[tokio::test]
async fn changing_only_the_saved_instance_reference_keeps_identity_refusal() {
    let was = native_sheet("base");
    let now = was.fork();
    replace_once(&now, "(reference \"R1\")", "(reference \"R9\")");
    let result = compare(&handler().await, &was, &now, &[]).await;
    assert_verdict(&result, "blocked");
    assert_eq!(
        result["reasons"],
        json!([format!(
            "symbol {} changed its lib_id or instances",
            symbol_uuid(&was, "R1")
        )])
    );
}

#[tokio::test]
async fn changing_native_position_file_membership() {
    assert_property_change(
        &native_sheet("base"),
        &native_sheet("pos"),
        &handler().await,
        "attribute in_pos_files",
    )
    .await;
}

#[tokio::test]
async fn changing_native_simulation_membership() {
    let was = native_sheet("before-sim");
    let result = compare(&handler().await, &was, &native_sheet("base-sim"), &[]).await;
    assert_verdict(&result, "blocked");
    assert_eq!(
        result["reasons"],
        json!([format!(
            "symbol {} (R1) changed attribute exclude_from_sim",
            symbol_uuid(&was, "R1"),
        )])
    );
}

const PATH: &str = "/b7704a08-6d46-4716-b137-fec3fc7158cf";

async fn assert_variant_change(fixture: &str, expected: &str) {
    let was = native_sheet("base");
    let now = native_sheet(fixture);
    let result = compare(&handler().await, &was, &now, &[]).await;
    assert_verdict(&result, "blocked");
    assert_eq!(
        result["reasons"],
        json!([format!(
        "symbol {} (R1) in project \"sparse\", path {PATH:?}, variant \"Lite\" changed {expected}",
        symbol_uuid(&was, "R1"),
    )])
    );
}

#[tokio::test]
async fn changing_variant_dnp() {
    assert_variant_change("variant-dnp", "attribute dnp").await;
}

#[tokio::test]
async fn changing_variant_value() {
    assert_variant_change("variant-value", "field \"Value\"").await;
}

// This exact one-edit fixture pair is also used for the native export oracle.
fn move_r1_reference(sheet: &Sheet) {
    replace_once(
        sheet,
        "(property \"Reference\" \"R1\"\n\t\t\t(at 40.132 38.1 90)",
        "(property \"Reference\" \"R1\"\n\t\t\t(at 40.64 38.1 90)",
    );
}

#[tokio::test]
async fn field_move_on_a_literal_variant_override_is_not_blocked() {
    let was = native_sheet("variant-value");
    let now = was.fork();
    move_r1_reference(&now);
    assert_eq!(
        crate::hex::sha256_hex(&was.bytes()),
        "5787cf396a4ae81d8942c07ad0b65e7a014ce09394065de9a2c692ba0954249c",
    );
    assert_eq!(
        crate::hex::sha256_hex(&now.bytes()),
        "3ba595b421f0f29e5c33f7a892ca56f968b575d1ddcb99cf836063219f6a8da6",
    );
    let result = compare(&handler().await, &was, &now, &[]).await;
    assert_ne!(result["verdict"], "blocked", "{result}");
    assert_eq!(result["reasons"], json!([]));
}

#[tokio::test]
async fn changing_a_variant_with_repeated_dnp_is_not_layout() {
    let was = native_sheet("variant-dnp");
    let now = was.fork();
    replace_once(
        &now,
        "(name \"Lite\")\n\t\t\t\t\t\t(dnp yes)",
        "(name \"Lite\")\n\t\t\t\t\t\t(dnp yes)\n\t\t\t\t\t\t(dnp no)",
    );
    let result = compare(&handler().await, &was, &now, &[]).await;
    assert_verdict(&result, "blocked");
    assert_eq!(result["reasons"], json!([format!(
        "symbol {} (R1) in project \"sparse\", path {PATH:?}, variant \"Lite\" changed ambiguous saved variant records",
        symbol_uuid(&was, "R1"),
    )]));
}

#[tokio::test]
async fn formatting_and_reordering_variant_records_is_allowed() {
    let was = native_sheet("variant-value");
    let now = was.fork();
    replace_once(
        &now,
        "(name \"Value\")\n\t\t\t\t\t\t\t(value \"1\")",
        "(value \"1\") (name \"Value\")",
    );
    let result = compare(&handler().await, &was, &now, &[]).await;
    assert_verdict(&result, "unchanged");
    assert_eq!(result["reasons"], json!([]));
}

const PARENT: &str = "annotate_nested_hierarchy/nested_pages.kicad_sch";
const SOUTH: &str =
    "sheet instance ed5b7508-65bd-450d-b053-99de75ca07ff (\"south\"; affects its children)";

#[tokio::test]
async fn parent_dnp_is_checked_without_opening_child_files() {
    let was = Sheet::input(PARENT);
    let now = native_sheet("parent-dnp");
    for input in [&was, &now] {
        assert!(!input.dir.path().join("bank.kicad_sch").exists());
        assert!(!input.dir.path().join("cell.kicad_sch").exists());
    }
    let result = compare(&handler().await, &was, &now, &[]).await;
    assert_verdict(&result, "blocked");
    assert_eq!(
        result["reasons"],
        json!([format!("{SOUTH} changed attribute dnp")])
    );
}

#[tokio::test]
async fn parent_custom_fields_feed_child_exports() {
    let result = compare(
        &handler().await,
        &Sheet::input(PARENT),
        &native_sheet("parent-field"),
        &[],
    )
    .await;
    assert_verdict(&result, "blocked");
    assert_eq!(
        result["reasons"],
        json!([
            format!("{SOUTH} changed field \"AssemblyNote\""),
            format!("{SOUTH} changed field \"Field2\""),
        ])
    );
}

#[tokio::test]
async fn parent_variant_dnp_affects_its_children() {
    let was = Sheet::input(PARENT);
    let now = was.fork();
    replace_once(
        &now,
        "(page \"2\")",
        "(page \"2\") (variant (name \"Lite\") (dnp yes))",
    );
    let result = compare(&handler().await, &was, &now, &[]).await;
    assert_verdict(&result, "blocked");
    assert_eq!(result["reasons"], json!([format!(
        "{SOUTH} in project \"nested_pages\", path \"/970f1adb-65bd-4ca9-bbaf-b939cab8549c\", variant \"Lite\" changed attribute dnp"
    )]));
}

#[tokio::test]
async fn cleanup_gate_keeps_its_existing_property_behavior() {
    let handler = handler().await;
    let was = native_sheet("base");
    let now = was.fork();
    edit(
        &handler,
        &now,
        json!({"fields": {"BOM Comments": "Changed"}}),
    )
    .await;
    assert_property_change(&was, &now, &handler, "field \"BOM Comments\"").await;
    assert!(super::super::cleanup_gate(&was.tree(), &now.tree(), &[]).is_empty());
}

#[tokio::test]
async fn placement_move_with_possible_rule_area_effects_blocks() {
    let handler = handler().await;
    let was = native_sheet("area-dnp");
    let now = was.fork();
    edit(
        &handler,
        &now,
        json!({"field_placements": {"Reference": {"x": 31.75, "y": 38.1}}}),
    )
    .await;
    let result = compare(&handler, &was, &now, &[]).await;
    assert_verdict(&result, "blocked");
    assert_eq!(result["reasons"], json!([format!(
        "symbol {} (R1) cannot establish unchanged export flags or Component Class: saved rule-area reach is not evaluated",
        symbol_uuid(&was, "R1"),
    )]));
}

#[tokio::test]
async fn edit_with_a_possible_component_class_dependency_blocks() {
    let handler = handler().await;
    let was = native_sheet("base");
    edit(
        &handler,
        &was,
        json!({"fields": {"Component Class": "AssemblyA"}}),
    )
    .await;
    let now = was.fork();
    move_r1_reference(&now);
    let result = compare(&handler, &was, &now, &[]).await;
    assert_verdict(&result, "blocked");
    assert_eq!(
        result["reasons"],
        json!([format!(
            "symbol {} (R1) cannot establish unchanged Component Class after a saved edit",
            symbol_uuid(&was, "R1"),
        )])
    );
}

#[tokio::test]
async fn changed_saved_input_to_exported_expression_blocks() {
    let handler = handler().await;
    let was = native_sheet("base");
    edit(
        &handler,
        &was,
        json!({"fields": {"BOM Comments": "Page ${#}"}}),
    )
    .await;
    let now = was.fork();
    replace_once(&now, "(page \"1\")", "(page \"2\")");
    let result = compare(&handler, &was, &now, &[]).await;
    assert_verdict(&result, "blocked");
    assert_eq!(result["reasons"], json!([format!(
        "symbol {} (R1) cannot establish unchanged field \"BOM Comments\": exported text uses a substitution",
        symbol_uuid(&was, "R1"),
    )]));
    assert_eq!(
        compare(&handler, &was, &was.fork(), &[]).await["reasons"],
        json!([])
    );
}

#[tokio::test]
async fn field_move_with_a_substituted_variant_override_is_not_proven_layout() {
    let was = native_sheet("variant-value");
    replace_once(&was, "(value \"1\")", "(value \"${#}\")");
    let now = was.fork();
    move_r1_reference(&now);
    let result = compare(&handler().await, &was, &now, &[]).await;
    assert_verdict(&result, "blocked");
    assert_eq!(result["reasons"], json!([format!(
        "symbol {} (R1) cannot establish unchanged field \"Value\": exported text uses a substitution",
        symbol_uuid(&was, "R1"),
    )]));
}
