use super::*;

// Except for the committed NET_NAME pair, these are served comparator
// probes. The test inputs do not assert new facts about native exports.
async fn comment(handler: &McpHandler, sheet: &Sheet, value: &str) {
    edit(handler, sheet, json!({"fields": {"BOM Comments": value}})).await;
}

fn private(sheet: &Sheet, name: &str, value: &str, hidden: bool) {
    let hide = if hidden { "(hide yes)" } else { "" };
    insert_r1_field(
        sheet,
        &format!(
            "\n\t\t(property private {name:?} {value:?} (at 38.1 38.1 0) \
             (effects (font (size 1.27 1.27)) (justify right) {hide}))"
        ),
    );
}

fn net_name(side: &str) -> Sheet {
    Sheet::input(&format!(
        "layout_properties_kicad10/net-name/{side}/sparse.kicad_sch"
    ))
}

#[tokio::test]
async fn moving_a_resolvable_exported_field_is_layout_only() {
    let handler = handler().await;
    let was = native_sheet("base");
    comment(&handler, &was, "Page ${#}").await;
    let now = was.fork();
    edit(
        &handler,
        &now,
        json!({"field_placements":{"BOM Comments":{"x": 40.64,"y": 38.1}}}),
    )
    .await;
    assert_ne!(was.bytes(), now.bytes());
    let result = compare(&handler, &was, &now, &[]).await;
    assert_verdict(&result, "unchanged");
    assert_eq!(result["reasons"], json!([]));
}

#[tokio::test]
async fn a_private_alias_changes_only_the_expression_that_reads_it() {
    let handler = handler().await;
    let was = native_sheet("base");
    private(&was, "Part", "10k", true);
    private(&was, "Unused", "A", true);
    comment(&handler, &was, "${Part}").await;
    let now = was.fork();
    replace_once(&now, r#""Unused" "A""#, r#""Unused" "B""#);
    assert_verdict(&compare(&handler, &was, &now, &[]).await, "unchanged");
    replace_once(&now, r#""Part" "10k""#, r#""Part" "22k""#);
    assert_property_change(&was, &now, &handler, r#"field "BOM Comments""#).await;
}

#[tokio::test]
async fn private_fields_can_shadow_footprint_builtins() {
    let handler = handler().await;
    for (token, capture) in [
        ("FOOTPRINT_NAME", "private-footprint-name"),
        ("FOOTPRINT_LIBRARY", "private-footprint-library"),
    ] {
        let was = native_sheet("base");
        private(&was, token, "A", true);
        comment(&handler, &was, &format!("${{{token}}}")).await;
        let now = was.fork();
        replace_once(
            &now,
            &format!("{token:?} \"A\""),
            &format!("{token:?} \"B\""),
        );
        assert_property_change(&was, &now, &handler, r#"field "BOM Comments""#).await;
        capture_review_probe(capture, &was, &now);
    }
}

// Disposable inputs for review, not a committed fixture or fixture inventory.
fn capture_review_probe(name: &str, was: &Sheet, now: &Sheet) {
    if let Some(root) = std::env::var_os("COPPERPOUR_LAYOUT_EXPRESSION_REVIEW_INPUTS") {
        let folder = PathBuf::from(root).join(name);
        fs::create_dir(&folder).unwrap();
        fs::write(folder.join("before.kicad_sch"), was.bytes()).unwrap();
        fs::write(folder.join("after.kicad_sch"), now.bytes()).unwrap();
    }
}

#[tokio::test]
async fn a_private_self_reference_can_interrupt_pin_function_lookup() {
    let handler = handler().await;
    let was = native_sheet("base");
    private(&was, "Scratch", "A", true);
    comment(&handler, &was, "${PIN_BASE_NAME(1)}").await;
    let now = was.fork();
    replace_once(&now, r#""Scratch" "A""#, r#""Scratch" "${Scratch}""#);
    assert_area_refusal(&was, &now, r#"unchanged field "BOM Comments""#).await;
}

#[tokio::test]
async fn private_alias_text_loads_using_each_saved_format_version() {
    let handler = handler().await;
    let was = native_sheet("base");
    replace_once(&was, "(version 20260306)", "(version 20250317)");
    comment(&handler, &was, "Note:${Scratch}").await;
    private(&was, "Scratch", "~", true);
    let now = was.fork();
    replace_once(&now, "(version 20250317)", "(version 20250318)");
    assert_area_refusal(&was, &now, r#"changed field "BOM Comments""#).await;
}

#[tokio::test]
async fn saved_title_fields_are_expression_inputs() {
    let handler = handler().await;
    let was = native_sheet("base");
    comment(&handler, &was, "Rev ${REVISION}").await;
    let now = was.fork();
    replace_once(
        &now,
        r#"(paper "A3")"#,
        r#"(paper "A3") (title_block (rev "A"))"#,
    );
    assert_property_change(&was, &now, &handler, r#"field "BOM Comments""#).await;
}

#[tokio::test]
async fn an_equal_saved_page_expression_does_not_prove_equal_exports() {
    let handler = handler().await;
    let was = native_sheet("base");
    comment(&handler, &was, "Page ${#}").await;
    replace_once(&was, r#"(page "1")"#, r#"(page "${REVISION}")"#);
    replace_once(
        &was,
        r#"(paper "A3")"#,
        r#"(paper "A3") (title_block (rev "A"))"#,
    );
    let now = was.fork();
    replace_once(&now, r#"(rev "A")"#, r#"(rev "B")"#);
    let result = compare(&handler, &was, &now, &[]).await;
    assert_verdict(&result, "blocked");
    assert_eq!(
        result["reasons"],
        json!([format!(
            "symbol {} (R1) cannot establish unchanged field \"BOM Comments\": an unresolved expression has changed saved inputs",
            symbol_uuid(&was, "R1")
        )])
    );
    capture_review_probe("saved-page-expression", &was, &now);
}

#[tokio::test]
async fn a_saved_cross_reference_can_read_another_symbols_private_field() {
    let handler = handler().await;
    let was = native_sheet("base");
    private(&was, "Part", "10k", true);
    ok(
        &handler,
        &was,
        "edit_schematic_component",
        json!({"reference":"R2","fields":{"BOM Comments":"${R1:Part}"}}),
    )
    .await;
    let now = was.fork();
    replace_once(&now, r#""Part" "10k""#, r#""Part" "22k""#);
    assert_area_refusal(
        &was,
        &now,
        &format!(
            r#"symbol {} (R2) changed field "BOM Comments""#,
            symbol_uuid(&was, "R2")
        ),
    )
    .await;
}

#[tokio::test]
async fn a_saved_cross_reference_can_read_a_child_page_by_uuid() {
    let handler = handler().await;
    let was = Sheet::input(PARENT);
    comment(
        &handler,
        &was,
        "Page ${ed5b7508-65bd-450d-b053-99de75ca07ff:#}",
    )
    .await;
    let now = was.fork();
    replace_once(&now, "(page \"2\")", "(page \"3\")");
    assert_property_change(&was, &now, &handler, r#"field "BOM Comments""#).await;
}

#[tokio::test]
async fn a_variant_qualified_lookup_checks_the_override_context() {
    let handler = handler().await;
    let was = native_sheet("variant-value");
    private(&was, "Part", "Target", true);
    replace_once(&was, r#"(value "1")"#, r#"(value "${Part}")"#);
    ok(
        &handler,
        &was,
        "edit_schematic_component",
        json!({"reference":"R2","fields":{"BOM Comments":"${R1:Value:Lite}", "Part":"Caller"}}),
    )
    .await;
    replace_once(
        &was,
        r#"(property "Part" "Caller""#,
        r#"(property private "Part" "Caller""#,
    );
    let now = was.fork();
    replace_once(&now, r#""Part" "Caller""#, r#""Part" "Edited""#);
    assert_area_refusal(
        &was,
        &now,
        &format!(
            r#"symbol {} (R2) changed field "BOM Comments""#,
            symbol_uuid(&was, "R2")
        ),
    )
    .await;
}

#[tokio::test]
async fn net_name_survives_a_netclass_directive_move_but_not_a_net_rename() {
    let handler = handler().await;
    let was = net_name("before");
    let now = net_name("after");
    let result = compare(&handler, &was, &now, &[]).await;
    assert_verdict(&result, "unchanged");
    assert_eq!(result["reasons"], json!([]));
    capture_native_pair("net-name", &was, &now);

    let renamed = was.fork();
    replace_once(&renamed, "(label \"SIG_C\"", "(label \"RENAMED\"");
    let result = compare(&handler, &was, &renamed, &[]).await;
    assert_verdict(&result, "blocked");
    assert!(
        reasons(&result).contains("would be renamed from SIG_C to RENAMED"),
        "{result}"
    );
}

#[tokio::test]
async fn net_class_reads_a_directive_move_that_preserves_pin_nets() {
    let was = net_name("before");
    let now = net_name("after");
    for sheet in [&was, &now] {
        replace_once(sheet, "${NET_NAME(1)}", "${NET_CLASS(1)}");
    }
    assert_area_refusal(&was, &now, r#"changed field "BOM Comments""#).await;
}

#[tokio::test]
async fn nested_and_later_pass_expressions_refuse_unproved_edit() {
    let handler = handler().await;
    for value in [
        "${NET_CLASS(${Number})}",
        "${Dollar}${Tail}",
        "${Missing}",
        "${R99:NET_CLASS(1)}",
    ] {
        let was = native_sheet("base");
        comment(&handler, &was, value).await;
        private(&was, "Number", "1", true);
        private(&was, "Dollar", "$", true);
        private(&was, "Tail", "{R2:Secret}", true);
        assert_verdict(
            &compare(&handler, &was, &was.fork(), &[]).await,
            "unchanged",
        );
        let now = was.fork();
        move_r1_reference(&now);
        assert_area_refusal(
            &was,
            &now,
            "an unresolved expression has changed saved inputs",
        )
        .await;
    }
}

#[tokio::test]
async fn an_alternate_selection_is_a_pin_function_input() {
    let handler = handler().await;
    let was = native_sheet("base");
    comment(&handler, &was, "${PIN_ALT_LIST(1)}").await;
    let moved = was.fork();
    move_r1_reference(&moved);
    assert_verdict(&compare(&handler, &was, &moved, &[]).await, "unchanged");
    let changed = was.fork();
    let source = fs::read_to_string(&changed.path).unwrap();
    let reference = source.find("(property \"Reference\" \"R1\"").unwrap();
    let pin = reference + source[reference..].find("(pin \"1\"").unwrap();
    let mut modified = source;
    modified.insert_str(pin + "(pin \"1\"".len(), " (alternate \"New\")");
    fs::write(&changed.path, modified).unwrap();
    assert_property_change(&was, &changed, &handler, r#"field "BOM Comments""#).await;
}

#[tokio::test]
async fn an_expression_in_an_unchanged_placed_alternate_is_not_proven() {
    let handler = handler().await;
    let was = native_sheet("base");
    private(&was, "Scratch", "A", true);
    comment(&handler, &was, "${PIN_NAME(1)}").await;
    let source = fs::read_to_string(&was.path).unwrap();
    let reference = source.find("(property \"Reference\" \"R1\"").unwrap();
    let pin = reference + source[reference..].find("(pin \"1\"").unwrap();
    let mut modified = source;
    modified.insert_str(pin + "(pin \"1\"".len(), r#" (alternate "${Scratch}")"#);
    fs::write(&was.path, modified).unwrap();
    let now = was.fork();
    replace_once(&now, r#""Scratch" "A""#, r#""Scratch" "B""#);
    assert_property_change(&was, &now, &handler, r#"field "BOM Comments""#).await;
    capture_review_probe("placed-alternate-expression", &was, &now);
}

#[tokio::test]
async fn another_symbols_pin_inputs_can_change_a_net_name_expression() {
    let was = net_name("before");
    let now = was.fork();
    let source = fs::read_to_string(&now.path).unwrap();
    let reference = source.find("(property \"Reference\" \"R1\"").unwrap();
    let pin = reference + source[reference..].find("(pin \"1\"").unwrap();
    let mut modified = source;
    modified.insert_str(pin + "(pin \"1\"".len(), " (alternate \"New\")");
    fs::write(&now.path, modified).unwrap();
    assert_area_refusal(&was, &now, r#"changed field "BOM Comments""#).await;
}

#[tokio::test]
async fn indirect_visible_field_text_can_change_rule_area_membership() {
    let handler = handler().await;
    let was = native_sheet("area-dnp");
    private(&was, "Display", "${R2:Scratch}", false);
    ok(
        &handler,
        &was,
        "edit_schematic_component",
        json!({"reference":"R2","fields":{"Scratch":"A"}}),
    )
    .await;
    replace_once(
        &was,
        r#"(property "Scratch""#,
        r#"(property private "Scratch""#,
    );
    let now = was.fork();
    replace_once(
        &now,
        r#""Scratch" "A""#,
        r#""Scratch" "A long private value""#,
    );
    assert_area_refusal(
        &was,
        &now,
        &format!(
            "symbol {} (R1) has unknown effective attribute dnp",
            symbol_uuid(&was, "R1")
        ),
    )
    .await;
}

#[tokio::test]
async fn an_attached_class_directive_can_read_saved_title_inputs() {
    let was = in_first_area();
    insert_directive(&was, "29.21", "${TITLE}", "");
    let now = was.fork();
    replace_once(
        &now,
        r#"(paper "A3")"#,
        r#"(paper "A3") (title_block (title "Changed"))"#,
    );
    assert_area_refusal(&was, &now, "directive expression inputs changed").await;
}

#[tokio::test]
async fn a_private_component_class_can_read_saved_title_inputs() {
    let was = native_sheet("base");
    private(&was, "Component Class", "${TITLE}", true);
    let now = was.fork();
    replace_once(
        &now,
        r#"(paper "A3")"#,
        r#"(paper "A3") (title_block (title "Changed"))"#,
    );
    assert_area_refusal(
        &was,
        &now,
        "effective Component Class (source direct field)",
    )
    .await;
}

#[tokio::test]
async fn a_saved_sheet_area_flag_can_feed_an_exported_child_field() {
    let was = native_sheet("parent-field");
    replace_once(
        &was,
        r#"(property "AssemblyNote" """#,
        r#"(property "AssemblyNote" "Population ${DNP}""#,
    );
    let now = was.fork();
    let mut source = fs::read_to_string(&now.path).unwrap();
    source.insert_str(
        source.rfind(')').unwrap(),
        r#"
        (rule_area (in_bom yes) (on_board yes) (dnp yes)
          (polyline (pts (xy 40 114) (xy 50 114) (xy 50 119) (xy 40 119))
            (stroke (width 0.1524) (type default)) (fill (type none))
            (uuid "7e067cc1-ec67-499c-afdd-75a4bd7677d0")))
        "#,
    );
    fs::write(&now.path, source).unwrap();
    assert_area_refusal(&was, &now, r#"changed field "AssemblyNote""#).await;
}
