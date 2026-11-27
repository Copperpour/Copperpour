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

// The native audit requests copies of the exact inputs used by the served test.
fn capture_native_pair(name: &str, before: &Sheet, after: &Sheet) {
    let Some(root) = std::env::var_os("COPPERPOUR_LAYOUT_NATIVE_INPUTS") else {
        return;
    };
    let folder = PathBuf::from(root).join(name);
    fs::create_dir(&folder).unwrap();
    for (side, sheet) in [("before", before), ("after", after)] {
        fs::write(folder.join(format!("{side}.kicad_sch")), sheet.bytes()).unwrap();
    }
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
async fn changing_placed_reference_without_instances_keeps_identity_refusal() {
    let was = native_sheet("base");
    let mut text = fs::read_to_string(&was.path).unwrap();
    let anchor = text.find("(reference \"R1\")").unwrap();
    let start = text[..anchor].rfind("\n\t\t(instances").unwrap();
    let end = anchor + text[anchor..].find("\n\t\t)\n").unwrap() + "\n\t\t)".len();
    text.replace_range(start..end, "");
    fs::write(&was.path, text).unwrap();
    let uuid = symbol_uuid(&was, "R1");
    let tree = was.tree();
    assert!(tree.find_all("symbol").into_iter().any(|item| {
        item.find_str("uuid") == Some(uuid.as_str()) && item.find("instances").is_none()
    }));

    let now = was.fork();
    replace_once(
        &now,
        "(property \"Reference\" \"R1\"",
        "(property \"Reference\" \"R9\"",
    );
    let result = compare(&handler().await, &was, &now, &[]).await;
    assert_verdict(&result, "blocked");
    assert_eq!(
        result["reasons"],
        json!([format!("symbol {uuid} changed its placed Reference")])
    );
    assert!(super::super::cleanup_gate(&was.tree(), &now.tree(), &[]).is_empty());
}

#[tokio::test]
async fn placed_reference_edit_does_not_override_a_saved_instance() {
    let was = native_sheet("base");
    let now = was.fork();
    replace_once(
        &now,
        "(property \"Reference\" \"R1\"",
        "(property \"Reference\" \"R9\"",
    );
    let result = compare(&handler().await, &was, &now, &[]).await;
    assert_ne!(result["verdict"], "blocked", "{result}");
    assert_eq!(result["reasons"], json!([]));
}

#[tokio::test]
async fn repeated_saved_reference_does_not_hide_a_placed_reference_edit() {
    let was = native_sheet("base");
    replace_once(
        &was,
        "(reference \"R1\")",
        "(reference \"R1\") (reference \"\")",
    );
    let now = was.fork();
    replace_once(
        &now,
        "(property \"Reference\" \"R1\"",
        "(property \"Reference\" \"R9\"",
    );
    let result = compare(&handler().await, &was, &now, &[]).await;
    assert_verdict(&result, "blocked");
    assert_eq!(
        result["reasons"],
        json!([format!(
            "symbol {} changed its placed Reference",
            symbol_uuid(&was, "R1")
        )])
    );
}

#[tokio::test]
async fn competing_saved_root_references_do_not_hide_a_placed_reference_edit() {
    let was = native_sheet("base");
    let root = was.tree().find_str("uuid").unwrap().to_owned();
    replace_once(
        &was,
        "(reference \"R1\")\n\t\t\t\t\t(unit 1)\n\t\t\t\t)",
        &format!(
            "(reference \"R1\")\n\t\t\t\t\t(unit 1)\n\t\t\t\t)\n\
             (path \"/{root}\" (reference \"R2\") (unit 1))"
        ),
    );
    let now = was.fork();
    replace_once(
        &now,
        "(property \"Reference\" \"R1\"",
        "(property \"Reference\" \"R9\"",
    );
    let result = compare(&handler().await, &was, &now, &[]).await;
    assert_verdict(&result, "blocked");
    assert_eq!(
        result["reasons"],
        json!([format!(
            "symbol {} changed its placed Reference",
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

#[tokio::test]
async fn root_uuid_change_can_select_a_different_saved_variant() {
    let was = native_sheet("variant-dnp");
    let second = "51a1b7a4-05e0-4a28-ad40-188cb4a0bd76";
    let start = format!("(path {PATH:?}\n\t\t\t\t\t(reference \"R1\")");
    replace_once(
        &was,
        &start,
        &format!("(path \"/{second}\" (reference \"R1\") (unit 1))\n{start}"),
    );
    let now = was.fork();
    replace_once(
        &now,
        &format!("\n\t(uuid {:?})", &PATH[1..]),
        &format!("\n\t(uuid {second:?})"),
    );
    let result = compare(&handler().await, &was, &now, &[]).await;
    assert_verdict(&result, "blocked");
    assert!(
        result["reasons"]
            .as_array()
            .unwrap()
            .contains(&json!(format!(
                "symbol {} (R1) changed the root UUID used to select saved instances",
                symbol_uuid(&was, "R1"),
            ))),
        "{result}"
    );
}

#[tokio::test]
async fn competing_instance_records_keep_their_order_and_multiplicity() {
    let handler = handler().await;
    let was = native_sheet("variant-dnp");
    let old_path = format!("(path {PATH:?}\n\t\t\t\t\t(reference \"R1\")");
    let stale = r#"(path "/51a1b7a4-05e0-4a28-ad40-188cb4a0bd76" (reference "R1") (unit 1))"#;
    let other = r#"(path "/63c64c07-7a85-4f6f-9775-b085c7fbd912""#;
    let selected = format!("{other}\n\t\t\t\t\t(reference \"R1\")");
    replace_once(&was, &old_path, &format!("{stale}\n{selected}"));
    let now = was.fork();
    replace_once(&now, &format!("{stale}\n{selected}"), &selected);
    replace_once(
        &now,
        "(dnp yes)\n\t\t\t\t\t)\n\t\t\t\t)",
        &format!("(dnp yes)\n\t\t\t\t\t)\n\t\t\t\t)\n{stale}",),
    );
    assert_property_change(
        &was,
        &now,
        &handler,
        "ambiguous saved instance-path records",
    )
    .await;

    let was = native_sheet("base");
    let now = was.fork();
    replace_once(&now, &old_path, &format!("{old_path} (reference \"R9\")"));
    assert_property_change(
        &was,
        &now,
        &handler,
        "ambiguous saved instance-path records",
    )
    .await;
}

#[tokio::test]
async fn repeated_child_instances_do_not_hide_an_order_change() {
    let was = Sheet::input(PARENT);
    let mut saved = fs::read_to_string(&was.path).unwrap();
    let anchor = saved.find("(page \"2\")").unwrap();
    let start = saved[..anchor].rfind("(instances").unwrap();
    let end = anchor + saved[anchor..].find("\n\t)").unwrap();
    let block = saved[start..end].to_owned();
    saved.replace_range(start..end, &format!("{block}\n\t\t(instances)"));
    fs::write(&was.path, saved).unwrap();
    let now = was.fork();
    replace_once(
        &now,
        &format!("{block}\n\t\t(instances)"),
        &format!("(instances)\n\t\t{block}"),
    );
    let result = compare(&handler().await, &was, &now, &[]).await;
    assert_verdict(&result, "blocked");
    assert_eq!(
        result["reasons"],
        json!([format!(
            "{SOUTH} changed ambiguous saved instance-path records"
        )])
    );
}

#[tokio::test]
async fn legacy_and_default_instance_export_inputs_are_guarded() {
    let handler = handler().await;
    let was = native_sheet("base");
    let now = was.fork();
    let marker = "(reference \"R1\")";
    let insert = |sheet: &Sheet, text: &str| {
        let mut body = fs::read_to_string(&sheet.path).unwrap();
        let at = body.find(marker).unwrap();
        let before_instances = body[..at].rfind("(instances").unwrap();
        body.insert_str(before_instances, text);
        fs::write(&sheet.path, body).unwrap();
    };
    insert(&now, "(default_instance (value \"22k\"))\n\t\t");
    assert_property_change(&was, &now, &handler, "its saved default instance").await;
    let now = was.fork();
    insert(&now, "(property private \"Sim.Enable\" \"0\" (at 38.1 38.1 0) (effects (font (size 1 1))))\n\t\t");
    assert_property_change(
        &was,
        &now,
        &handler,
        "legacy simulation attribute Sim.Enable",
    )
    .await;

    insert(&was, "(default_instance (value \"Page ${#}\"))\n\t\t");
    let now = was.fork();
    replace_once(&now, "(page \"1\")", "(page \"2\")");
    let result = compare(&handler, &was, &now, &[]).await;
    assert_verdict(&result, "blocked");
    assert_eq!(result["reasons"], json!([format!(
        "symbol {} (R1) cannot establish unchanged field \"value\": exported text uses a substitution",
        symbol_uuid(&was, "R1"),
    )]));
}

#[tokio::test]
async fn variant_overrides_cannot_be_normalized_against_a_superseded_default_value() {
    let was = native_sheet("variant-dnp");
    let start = "(instances\n\t\t\t(project \"sparse\"\n\t\t\t\t(path";
    let body = fs::read_to_string(&was.path).unwrap();
    let r1 = body.find("(property \"Reference\" \"R1\"").unwrap();
    let at = r1 + body[r1..].find(start).unwrap();
    let mut with_default = body;
    with_default.insert_str(at, "(default_instance (value \"22k\"))\n\t\t");
    fs::write(&was.path, with_default).unwrap();
    let now = was.fork();
    replace_once(
        &now,
        "(name \"Lite\")\n\t\t\t\t\t\t(dnp yes)",
        "(name \"Lite\")\n\t\t\t\t\t\t(dnp yes) (field (name \"Value\") (value \"10k\"))",
    );
    assert_property_change(&was, &now, &handler().await, "its saved default instance").await;
}

#[tokio::test]
async fn older_simulation_flag_and_legacy_flag_order_are_guarded() {
    let handler = handler().await;
    let was = native_sheet("variant-dnp");
    let marker = "(reference \"R1\")";
    let insert = |sheet: &Sheet, name: &str, value: &str, before: bool| {
        let mut body = fs::read_to_string(&sheet.path).unwrap();
        let at = body.find(marker).unwrap();
        let offset = if before {
            body[..at].rfind("(instances").unwrap()
        } else {
            at + body[at..].find("\n\t)\n").unwrap()
        };
        body.insert_str(offset, &format!("\n\t\t(property private {name:?} {value:?} (at 38.1 38.1 0) (effects (font (size 1 1))))\n\t\t"));
        fs::write(&sheet.path, body).unwrap();
    };
    let now = was.fork();
    insert(&now, "Spice_Netlist_Enabled", "N", true);
    assert_property_change(
        &was,
        &now,
        &handler,
        "legacy simulation attribute Spice_Netlist_Enabled",
    )
    .await;

    let now = was.fork();
    insert(&now, "Sim.Enable", "0", false);
    insert(&was, "Sim.Enable", "0", true);
    assert_property_change(
        &was,
        &now,
        &handler,
        "legacy simulation attribute Sim.Enable",
    )
    .await;
}

#[tokio::test]
async fn a_repeated_saved_instance_value_can_contain_an_exported_expression() {
    let was = native_sheet("base");
    replace_once(
        &was,
        "(reference \"R1\")",
        "(reference \"R1\") (value \"10k\") (value \"Page ${#}\")",
    );
    let now = was.fork();
    replace_once(&now, "(page \"1\")", "(page \"2\")");
    let result = compare(&handler().await, &was, &now, &[]).await;
    assert_verdict(&result, "blocked");
    assert_eq!(result["reasons"], json!([format!(
        "symbol {} (R1) cannot establish unchanged field \"value\": exported text uses a substitution",
        symbol_uuid(&was, "R1"),
    )]));
}

#[tokio::test]
async fn duplicate_field_suffixes_cannot_hide_reordered_exports() {
    let handler = handler().await;
    let was = native_sheet("base");
    let mut body = fs::read_to_string(&was.path).unwrap();
    let r1 = body.find("(property \"Reference\" \"R1\"").unwrap();
    let at = r1 + body[r1..].find("\n\t\t(pin ").unwrap();
    let field = |name, value| {
        format!(
            "\n\t\t(property \"{name}\" \"{value}\" (at 38.1 38.1 0) (effects (font (size 1 1))))"
        )
    };
    let a = field("MPN", "A");
    let b = field("MPN", "B");
    let suffix = field("MPN_1", "C");
    let ordered = format!("{a}{b}{suffix}");
    body.insert_str(at, &ordered);
    fs::write(&was.path, body).unwrap();
    let now = was.fork();
    replace_once(&now, &ordered, &format!("{a}{suffix}{b}"));
    assert_property_change(&was, &now, &handler, "ambiguous saved field records").await;

    replace_once(&was, &a, &field("MPN", "~"));
    replace_once(&was, "(version 20260306)", "(version 20250317)");
    let now = was.fork();
    replace_once(&now, "(version 20250317)", "(version 20260306)");
    assert_property_change(&was, &now, &handler, "ambiguous saved field records").await;
}

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
async fn placement_move_with_effective_rule_area_change_blocks() {
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
    let reason = format!(
        "symbol {} (R1) changed attribute dnp (rule area ",
        symbol_uuid(&was, "R1")
    );
    assert!(
        result["reasons"]
            .as_array()
            .unwrap()
            .iter()
            .any(|value| { value.as_str().is_some_and(|text| text.starts_with(&reason)) }),
        "{result}"
    );
    let inside = now.fork();
    edit(
        &handler,
        &inside,
        json!({"field_placements": {"Reference": {"x": 33.02, "y": 38.1}}}),
    )
    .await;
    assert_eq!(
        compare(&handler, &now, &inside, &[]).await["reasons"],
        json!([])
    );
    capture_native_pair("inside-dnp-area", &now, &inside);
}

#[tokio::test]
async fn a_literal_component_class_allows_moving_field_text() {
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
    assert_ne!(result["verdict"], "blocked", "{result}");
    assert_eq!(result["reasons"], json!([]));
    capture_native_pair("literal-component-class", &was, &now);
}

#[tokio::test]
async fn rule_area_bounds_use_the_saved_root_reference() {
    let was = native_sheet("area-dnp");
    replace_once(
        &was,
        "(xy 29.21 34.29) (xy 34.29 34.29) (xy 34.29 41.91) (xy 29.21 41.91)",
        "(xy 38.1 48.26) (xy 43.18 48.26) (xy 43.18 53.34) (xy 38.1 53.34)",
    );
    let now = was.fork();
    replace_once(
        &now,
        r#"(property "Reference" "R1""#,
        r#"(property "Reference" "R1111111111111111111111111111111111111111""#,
    );
    let result = compare(&handler().await, &was, &now, &[]).await;
    assert_ne!(result["verdict"], "blocked", "{result}");
    assert_eq!(result["reasons"], json!([]));
    capture_native_pair("saved-reference-near-dnp-area", &was, &now);
}

fn set_fixture_r1_dnp(sheet: &Sheet, flags: &str) {
    replace_once(
        sheet,
        "(dnp no)\n\t\t(uuid \"da18b095-bbe4-4648-853f-9bfe789c3a44\")",
        &format!("{flags}\n\t\t(uuid \"da18b095-bbe4-4648-853f-9bfe789c3a44\")"),
    );
}

fn set_fixture_lite_dnp(sheet: &Sheet, flags: &str) {
    replace_once(
        sheet,
        "(name \"Lite\")\n\t\t\t\t\t\t(field",
        &format!("(name \"Lite\") {flags}\n\t\t\t\t\t\t(field"),
    );
}

fn move_r1_reference_into_area(sheet: &Sheet) {
    replace_once(
        sheet,
        "(property \"Reference\" \"R1\"\n\t\t\t(at 40.132 38.1 90)",
        "(property \"Reference\" \"R1\"\n\t\t\t(at 31.75 38.1 90)",
    );
}

#[tokio::test]
async fn literal_variant_value_does_not_hide_load_time_area_membership() {
    let was = native_sheet("area-dnp");
    set_fixture_r1_dnp(&was, "(dnp yes)");
    set_fixture_lite_dnp(&was, "(dnp no)");
    replace_once(
        &was,
        "(xy 29.21 34.29) (xy 34.29 34.29) (xy 34.29 41.91) (xy 29.21 41.91)",
        "(xy 43.18 34.29) (xy 55.88 34.29) (xy 55.88 41.91) (xy 43.18 41.91)",
    );
    let value = "WWWWWWWWWWWWWWWWWWWWWWWWWWWW";
    replace_once(
        &was,
        "(property \"Value\" \"10k\"\n\t\t\t(at 38.1 38.1 90)",
        &format!("(property \"Value\" \"{value}\"\n\t\t\t(at 12.7 38.1 0)"),
    );
    let now = was.fork();
    replace_once(&now, "(at 12.7 38.1 0)", "(at 38.1 38.1 0)");
    assert_area_refusal(
        &was,
        &now,
        "variant \"Lite\" has unknown effective attribute dnp",
    )
    .await;
    capture_native_pair("load-time-geometry-with-lite-value", &was, &now);
}

#[tokio::test]
async fn repeated_default_dnp_does_not_hide_area_entry() {
    let was = native_sheet("area-dnp");
    set_fixture_r1_dnp(&was, "(dnp yes) (dnp no)");
    replace_once(
        &was,
        concat!(
            "\n\t\t\t\t\t(variant\n\t\t\t\t\t\t(name \"Lite\")",
            "\n\t\t\t\t\t\t(field\n\t\t\t\t\t\t\t(name \"Value\")",
            "\n\t\t\t\t\t\t\t(value \"1\")\n\t\t\t\t\t\t)\n\t\t\t\t\t)"
        ),
        "",
    );
    let now = was.fork();
    move_r1_reference_into_area(&now);
    assert_area_refusal(&was, &now, "(R1) changed attribute dnp").await;
    capture_native_pair("repeated-default-dnp-area-entry", &was, &now);
}

#[tokio::test]
async fn repeated_variant_dnp_does_not_hide_area_entry() {
    let was = native_sheet("area-dnp");
    set_fixture_r1_dnp(&was, "(dnp yes)");
    set_fixture_lite_dnp(&was, "(dnp yes) (dnp no)");
    let now = was.fork();
    move_r1_reference_into_area(&now);
    assert_area_refusal(&was, &now, "variant \"Lite\" changed attribute dnp").await;
    capture_native_pair("repeated-lite-dnp-area-entry", &was, &now);
}

#[tokio::test]
async fn competing_variant_names_do_not_hide_area_entry() {
    let was = native_sheet("area-dnp");
    set_fixture_r1_dnp(&was, "(dnp yes)");
    set_fixture_lite_dnp(&was, "(dnp no) (name \"Other\") (dnp yes)");
    let now = was.fork();
    move_r1_reference_into_area(&now);
    assert_area_refusal(&was, &now, "competing saved variant names").await;
    capture_native_pair("competing-variant-names", &was, &now);
}

#[tokio::test]
async fn a_repeated_field_position_does_not_prove_area_separation() {
    let was = native_sheet("area-dnp");
    let now = was.fork();
    replace_once(
        &now,
        "(property \"Reference\" \"R1\"\n\t\t\t(at 40.132 38.1 90)",
        "(property \"Reference\" \"R1\"\n\t\t\t(at 40.132 38.1 90) (at 31.75 38.1 90)",
    );
    assert_area_refusal(&was, &now, "(R1) has unknown effective attribute dnp").await;
    capture_native_pair("repeated-reference-position-area-entry", &was, &now);
}

#[tokio::test]
async fn repeated_visibility_setters_cannot_hide_area_entry() {
    let was = native_sheet("area-dnp");
    let placed = "(property \"Reference\" \"R1\"\n\t\t\t(at 31.75 38.1 90)";
    move_r1_reference_into_area(&was);
    replace_once(&was, placed, &format!("{placed} (hide yes)"));
    let now = was.fork();
    let mut source = fs::read_to_string(&now.path).unwrap();
    let start = source.find(placed).unwrap();
    let end = start + source[start..].find("\n\t\t)\n").unwrap();
    source.insert_str(end, " (hide no)");
    fs::write(&now.path, source).unwrap();
    assert_area_refusal(&was, &now, "(R1) has unknown effective attribute dnp").await;
    capture_native_pair("repeated-visibility-area-entry", &was, &now);
}

#[tokio::test]
async fn repeated_rectangle_coordinates_cannot_hide_area_entry() {
    let was = native_sheet("area-dnp");
    let now = was.fork();
    replace_once(
        &now,
        "(start -1.016 -2.54)",
        "(start -1.016 -2.54) (start -7.62 -2.54)",
    );
    assert_area_refusal(&was, &now, "(R1) has unknown effective attribute dnp").await;
    capture_native_pair("repeated-rectangle-coordinate-area-entry", &was, &now);
}

#[tokio::test]
async fn legacy_simulation_enable_does_not_mask_an_area_exclusion() {
    let was = native_sheet("area-dnp");
    replace_once(
        &was,
        "(rule_area\n\t\t(exclude_from_sim no)\n\t\t(in_bom yes)\n\t\t(on_board yes)\n\t\t(dnp yes)",
        "(rule_area\n\t\t(exclude_from_sim yes)\n\t\t(in_bom yes)\n\t\t(on_board yes)\n\t\t(dnp no)",
    );
    let mut source = fs::read_to_string(&was.path).unwrap();
    let reference = source.find("(property \"Reference\" \"R1\"").unwrap();
    let start = source[..reference].rfind("\n\t(symbol\n").unwrap();
    let end = reference + source[reference..].find("\n\t\t(instances").unwrap();
    let placement = source[start..end].replace("(exclude_from_sim no)", "(exclude_from_sim yes)");
    source.replace_range(
        start..end,
        &format!("{placement}\n\t\t(property \"Sim.Enable\" \"1\" (at 38.1 38.1 0) (hide yes) (effects (font (size 1.27 1.27))))"),
    );
    fs::write(&was.path, source).unwrap();
    let now = was.fork();
    move_r1_reference_into_area(&now);
    assert_area_refusal(&was, &now, "(R1) changed attribute exclude_from_sim").await;
    capture_native_pair("legacy-sim-enable-area-entry", &was, &now);
}

#[tokio::test]
async fn a_superseded_field_does_not_prove_dnp_in_both_snapshots() {
    let was = native_sheet("area-dnp");
    let mut source = fs::read_to_string(&was.path).unwrap();
    let start = source.find("\t\t(property \"Reference\" \"R1\"").unwrap();
    let end = start + source[start..].find("\t\t(property \"Value\"").unwrap();
    let superseded = source[start..end].replace("(at 40.132 38.1 90)", "(at 31.75 38.1 90)");
    source.insert_str(start, &superseded);
    fs::write(&was.path, source).unwrap();
    let now = was.fork();
    move_r1_reference_into_area(&now);
    assert_area_refusal(&was, &now, "(R1) has unknown effective attribute dnp").await;
    capture_native_pair("superseded-reference-area-entry", &was, &now);
}

#[tokio::test]
async fn a_duplicate_library_definition_does_not_prove_area_separation() {
    let was = native_sheet("area-dnp");
    let now = was.fork();
    let mut source = fs::read_to_string(&now.path).unwrap();
    let start = source.find("\t\t(symbol \"Device:R\"").unwrap();
    let end = source.find("\t\t(symbol \"power:+5V\"").unwrap();
    let original = &source[start..end];
    assert_eq!(original.matches("(start -1.016 -2.54)").count(), 1);
    let extended = original.replace("(start -1.016 -2.54)", "(start -7.62 -2.54)");
    source.insert_str(end, &extended);
    fs::write(&now.path, source).unwrap();
    assert_area_refusal(&was, &now, "(R1) has unknown effective attribute dnp").await;
    capture_native_pair("duplicate-library-geometry-area-entry", &was, &now);
}

#[tokio::test]
async fn a_second_library_block_cannot_hide_changed_area_geometry() {
    let was = native_sheet("area-dnp");
    let now = was.fork();
    let mut source = fs::read_to_string(&now.path).unwrap();
    let start = source.find("\t\t(symbol \"Device:R\"").unwrap();
    let next = source.find("\t\t(symbol \"power:+5V\"").unwrap();
    let end = source.find("\t(junction\n").unwrap();
    let extended = source[start..next].replace("(start -1.016 -2.54)", "(start -7.62 -2.54)");
    source.insert_str(end, &format!("\t(lib_symbols\n{extended}\t)\n"));
    fs::write(&now.path, source).unwrap();
    assert_area_refusal(&was, &now, "(R1) has unknown effective attribute dnp").await;
    capture_native_pair("second-library-block-area-entry", &was, &now);
}

fn insert_directive(sheet: &Sheet, x: &str, class: &str, extra: &str) {
    let mut text = fs::read_to_string(&sheet.path).unwrap();
    text.insert_str(
        text.rfind(')').unwrap(),
        &format!(
            r#"
        (directive_label "" (length 2.54) (shape round) (at {x} 38.1 0)
            (effects (font (size 1.27 1.27)) (justify left bottom))
            (uuid "bceff5a3-b8d2-4065-990b-ced5e643596b")
            (property "Component Class" "{class}" (at {x} 38.1 0)
                (effects (font (size 1.27 1.27))))
            {extra})
        "#,
        ),
    );
    fs::write(&sheet.path, text).unwrap();
}

fn insert_r1_field(sheet: &Sheet, field: &str) {
    let mut text = fs::read_to_string(&sheet.path).unwrap();
    let anchor = text.find(r#"(property "Reference" "R1""#).unwrap();
    let at = anchor + text[anchor..].find("\n\t\t(pin ").unwrap();
    text.insert_str(at, field);
    fs::write(&sheet.path, text).unwrap();
}

async fn assert_area_refusal(was: &Sheet, now: &Sheet, name: &str) {
    let result = compare(&handler().await, was, now, &[]).await;
    assert_verdict(&result, "blocked");
    assert!(
        result["reasons"]
            .as_array()
            .unwrap()
            .iter()
            .any(|reason| { reason.as_str().is_some_and(|reason| reason.contains(name)) }),
        "{result}"
    );
}

fn in_first_area() -> Sheet {
    let sheet = native_sheet("area-flagless");
    replace_once(
        &sheet,
        "(property \"Reference\" \"R1\"\n\t\t\t(at 40.132 38.1 90)",
        "(property \"Reference\" \"R1\"\n\t\t\t(at 31.75 38.1 90)",
    );
    sheet
}

#[tokio::test]
async fn a_changed_literal_rule_area_netclass_blocks_without_an_expression() {
    let was = native_sheet("area-flagless");
    replace_once(
        &was,
        "(xy 29.21 34.29) (xy 34.29 34.29) (xy 34.29 41.91) (xy 29.21 41.91)",
        "(xy 41.91 38.1) (xy 45.72 38.1) (xy 45.72 45.72) (xy 41.91 45.72)",
    );
    insert_directive(&was, "41.91", "ClassA", "");
    replace_once(
        &was,
        r#""Component Class" "ClassA""#,
        r#""Netclass" "ClassA""#,
    );
    let now = was.fork();
    replace_once(&now, r#""Netclass" "ClassA""#, r#""Netclass" "ClassB""#);
    assert_area_refusal(
        &was,
        &now,
        "cannot establish unchanged Netclass assignments (rule area",
    )
    .await;
    capture_native_pair("literal-rule-area-netclass-change", &was, &now);
}

#[tokio::test]
async fn a_custom_global_power_value_is_still_a_net_name() {
    let was = native_sheet("area-flagless");
    replace_once(
        &was,
        "(property \"Value\" \"+5V\"\n\t\t\t(at 50.8 30.734 0)",
        "(property \"Value\" \"+24V_ISO\"\n\t\t\t(at 50.8 30.734 0)",
    );
    let moved = was.fork();
    replace_once(&moved, "(at 50.8 30.734 0)", "(at 50.8 30.48 0)");
    let handler = handler().await;
    let result = compare(&handler, &was, &moved, &[]).await;
    assert_ne!(result["verdict"], "blocked", "{result}");
    assert_eq!(result["reasons"], json!([]));

    let renamed = moved.fork();
    replace_once(
        &renamed,
        "(property \"Value\" \"+24V_ISO\"",
        "(property \"Value\" \"+24V_AUX\"",
    );
    let result = compare(&handler, &moved, &renamed, &[]).await;
    assert_verdict(&result, "blocked");
    let reasons = result["reasons"].as_array().unwrap();
    for reason in [
        "net +24V_ISO no longer has its baseline name and scope on the sheet",
        "net +24V_AUX has a new name or scope on the sheet",
    ] {
        assert!(reasons.contains(&json!(reason)), "{result}");
    }
}

#[tokio::test]
async fn repeated_rule_area_point_lists_do_not_prove_separation() {
    let was = native_sheet("area-dnp");
    let prefix = "(xy 29.21 41.91) (xy 29.21 34.29) (xy 34.29 34.29) (xy 34.29 41.91)";
    replace_once(
        &was,
        "(xy 29.21 34.29) (xy 34.29 34.29) (xy 34.29 41.91) (xy 29.21 41.91)",
        prefix,
    );
    let now = was.fork();
    replace_once(
        &now,
        &format!("{prefix}\n\t\t\t)"),
        &format!("{prefix}\n\t\t\t) (pts (xy 43.18 29.21) (xy 43.18 45.72) (xy 29.21 45.72))"),
    );
    let result = compare(&handler().await, &was, &now, &[]).await;
    assert_verdict(&result, "blocked");
    let reason = format!(
        "symbol {} (R1) has unknown effective attribute dnp (rule area 19fd901d-1fef-4757-84ca-8613c1908b96)",
        symbol_uuid(&was, "R1")
    );
    assert!(
        result["reasons"]
            .as_array()
            .unwrap()
            .contains(&json!(reason)),
        "{result}"
    );
}

#[tokio::test]
async fn repeated_directive_positions_do_not_prove_attachment() {
    let was = in_first_area();
    insert_directive(&was, "29.21", "AssemblyA", "");
    let now = was.fork();
    replace_once(
        &now,
        "(shape round) (at 29.21 38.1 0)",
        "(shape round) (at 29.21 38.1 0) (at 27.94 38.1 0)",
    );
    assert_area_refusal(&was, &now, "unknown effective Component Class").await;
}

#[tokio::test]
async fn crossing_the_legacy_empty_field_cutoff_can_change_a_directive_class() {
    let was = in_first_area();
    insert_directive(&was, "29.21", "~", "");
    replace_once(&was, "(version 20260306)", "(version 20250317)");
    let now = was.fork();
    replace_once(&now, "(version 20250317)", "(version 20250318)");
    assert_area_refusal(&was, &now, r#"changed Component Class from [] to ["~"]"#).await;
}

#[tokio::test]
async fn changing_an_uncertain_directive_anchor_can_change_the_class() {
    let was = in_first_area();
    insert_directive(&was, "29.2104", "AssemblyA", "");
    let now = was.fork();
    replace_once(
        &now,
        "(shape round) (at 29.2104 38.1 0)",
        "(shape round) (at 29.2106 38.1 0)",
    );
    assert_area_refusal(&was, &now, "Component Class").await;
}

#[tokio::test]
async fn encoded_class_substitutions_stay_opaque() {
    let was = native_sheet("base");
    insert_r1_field(
        &was,
        r#"
        (property private "Component Class" "{dollar}{CLASSNAME}" (at 38.1 38.1 0)
            (hide yes) (effects (font (size 1.27 1.27))))
        (property private "CLASSNAME" "AssemblyA" (at 38.1 38.1 0)
            (hide yes) (effects (font (size 1.27 1.27))))
    "#,
    );
    let now = was.fork();
    replace_once(
        &now,
        r#""CLASSNAME" "AssemblyA""#,
        r#""CLASSNAME" "AssemblyB""#,
    );
    assert_area_refusal(&was, &now, "Component Class").await;

    let was = in_first_area();
    insert_directive(
        &was,
        "29.21",
        "{dollar}{CLASSNAME}",
        r#"(property "CLASSNAME" "AssemblyA" (at 29.21 38.1 0)
        (hide yes) (effects (font (size 1.27 1.27))))"#,
    );
    let now = was.fork();
    replace_once(
        &now,
        r#""CLASSNAME" "AssemblyA""#,
        r#""CLASSNAME" "AssemblyB""#,
    );
    assert_area_refusal(&was, &now, "rule-area effects").await;
}

#[tokio::test]
async fn a_duplicate_private_class_cannot_mask_removal_of_a_directive_class() {
    let was = in_first_area();
    insert_r1_field(
        &was,
        r#"
        (property private "Component Class" "A" (at 38.1 38.1 0)
            (hide yes) (effects (font (size 1.27 1.27))))
        (property private "Component Class" "B" (at 38.1 38.1 0)
            (hide yes) (effects (font (size 1.27 1.27))))
    "#,
    );
    insert_directive(&was, "29.21", "B", "");
    let now = was.fork();
    replace_once(
        &now,
        "(shape round) (at 29.21 38.1 0)",
        "(shape round) (at 27.94 38.1 0)",
    );
    assert_area_refusal(&was, &now, "Component Class").await;
}

#[tokio::test]
async fn repeated_area_tokens_follow_saved_order() {
    let was = in_first_area();
    let now = was.fork();
    let mut source = fs::read_to_string(&now.path).unwrap();
    let area = source.find("(rule_area").unwrap();
    let flag = area + source[area..].find("(dnp no)").unwrap() + "(dnp no)".len();
    source.insert_str(flag, " (dnp yes)");
    fs::write(&now.path, source).unwrap();
    assert_area_refusal(&was, &now, "attribute dnp").await;

    let was = native_sheet("area-dnp");
    let now = was.fork();
    let mut source = fs::read_to_string(&now.path).unwrap();
    let area = source.find("(rule_area").unwrap();
    let at = area + source[area..].find("\n\t)\n").unwrap();
    source.insert_str(
        at,
        r#"
        (polyline (pts (xy 35.56 34.29) (xy 43.18 34.29) (xy 43.18 41.91) (xy 35.56 41.91))
            (stroke (width 0) (type dash)) (fill (type none))
            (uuid "19fd901d-1fef-4757-84ca-8613c1908b96"))"#,
    );
    fs::write(&now.path, source).unwrap();
    assert_area_refusal(&was, &now, "attribute dnp").await;
}

#[tokio::test]
async fn unmodelled_unit_and_legacy_field_inputs_cannot_prove_area_separation() {
    let was = native_sheet("area-dnp");
    replace_once(
        &was,
        "(symbol \"R_1_1\"",
        "(symbol \"R_1_1\" (unit_name \"A\")",
    );
    let now = was.fork();
    replace_once(
        &now,
        "(unit_name \"A\")",
        "(unit_name \"Very long custom unit name\")",
    );
    assert_area_refusal(&was, &now, "attribute dnp").await;

    let was = native_sheet("area-dnp");
    let anchor = format!("(path {PATH:?}\n\t\t\t\t\t(reference \"R1\")\n\t\t\t\t\t(unit 1)");
    replace_once(&was, &anchor, &anchor.replace("(unit 1)", "(unit 2)"));
    let now = was.fork();
    replace_once(&now, "(start -1.016 -2.54)", "(start -1.1 -2.54)");
    assert_area_refusal(&was, &now, "attribute dnp").await;

    let was = native_sheet("area-dnp");
    insert_r1_field(
        &was,
        "\n\t\t(default_instance (value \"long legacy value\"))",
    );
    let now = was.fork();
    replace_once(&now, "(xy 29.21 34.29) ", "(xy 27.94 34.29) ");
    assert_area_refusal(&was, &now, "attribute dnp").await;
}

#[tokio::test]
async fn old_private_tilde_can_become_a_literal_class() {
    let was = native_sheet("base");
    insert_r1_field(
        &was,
        r#"
        (property private "Component Class" "~" (at 38.1 38.1 0)
            (hide yes) (effects (font (size 1.27 1.27))))
    "#,
    );
    replace_once(&was, "(version 20260306)", "(version 20250317)");
    let now = was.fork();
    replace_once(&now, "(version 20250317)", "(version 20260306)");
    assert_area_refusal(&was, &now, "changed Component Class").await;
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
