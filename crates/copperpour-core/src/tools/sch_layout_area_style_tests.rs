use super::*;

fn area_sheet() -> Sheet {
    Sheet::input("layout_properties_kicad10/area-dnp/sparse.kicad_sch")
}

fn replace(sheet: &Sheet, before: &str, after: &str) {
    let data = fs::read_to_string(&sheet.path).unwrap();
    assert_eq!(data.matches(before).count(), 1, "{before}");
    fs::write(&sheet.path, data.replacen(before, after, 1)).unwrap();
}

async fn check(before: &Sheet, after: &Sheet, flag: &str, native_case: &str) {
    let result = compare(&handler().await, before, after, &[]).await;
    assert_verdict(&result, "blocked");
    let name = format!("symbol {} (R1) ", symbol_uuid(before, "R1"));
    assert!(
        result["reasons"].as_array().unwrap().iter().any(|reason| {
            reason
                .as_str()
                .is_some_and(|line| line.starts_with(&name) && line.contains(flag))
        }),
        "{result}"
    );
    if let Some(root) = std::env::var_os("COPPERPOUR_LAYOUT_AREA_NATIVE_INPUTS") {
        let folder = PathBuf::from(root).join(native_case);
        fs::create_dir(&folder).unwrap();
        for (name, sheet) in [("before", before), ("after", after)] {
            fs::write(folder.join(format!("{name}.kicad_sch")), sheet.bytes()).unwrap();
        }
    }
}

#[tokio::test]
async fn a_discarded_simulation_field_cannot_prove_area_membership() {
    let before = area_sheet();
    replace(
        &before,
        "(rule_area\n\t\t(exclude_from_sim no)\n\t\t(in_bom yes)\n\t\t(on_board yes)\n\t\t(dnp yes)",
        "(rule_area\n\t\t(exclude_from_sim yes)\n\t\t(in_bom yes)\n\t\t(on_board yes)\n\t\t(dnp no)",
    );
    let mut data = fs::read_to_string(&before.path).unwrap();
    let field = data.find("(property \"Reference\" \"R1\"").unwrap();
    let start = data[..field].rfind("\n\t(symbol\n").unwrap();
    let end = field + data[field..].find("\n\t\t(instances").unwrap();
    let placement = data[start..end].replace("(exclude_from_sim no)", "(exclude_from_sim yes)");
    data.replace_range(
        start..end,
        &format!("{placement}\n\t\t(property \"Sim.Enable\" \"1\" (at 31.75 38.1 0) (effects (font (size 1.27 1.27))))"),
    );
    fs::write(&before.path, data).unwrap();
    let after = before.fork();
    replace(
        &after,
        "(property \"Reference\" \"R1\"\n\t\t\t(at 40.132 38.1 90)",
        "(property \"Reference\" \"R1\"\n\t\t\t(at 31.75 38.1 90)",
    );
    check(
        &before,
        &after,
        "attribute exclude_from_sim",
        "discarded-sim-field",
    )
    .await;
}

#[tokio::test]
async fn a_repeated_stroke_width_cannot_prove_area_separation() {
    let before = area_sheet();
    let after = before.fork();
    replace(&after, "(width 0.254)", "(width 0.254) (width 10)");
    check(
        &before,
        &after,
        "attribute dnp",
        "repeated-shape-stroke-width",
    )
    .await;
}

#[tokio::test]
async fn a_repeated_font_size_cannot_prove_area_separation() {
    let before = area_sheet();
    let after = before.fork();
    let mut data = fs::read_to_string(&after.path).unwrap();
    let value = data
        .find("(property \"Value\" \"10k\"\n\t\t\t(at 38.1 38.1 90)")
        .unwrap();
    let size = value + data[value..].find("(size 1.27 1.27)").unwrap() + "(size 1.27 1.27)".len();
    data.insert_str(size, " (size 80 80)");
    fs::write(&after.path, data).unwrap();
    check(&before, &after, "attribute dnp", "repeated-field-font-size").await;
}

#[tokio::test]
async fn a_case_distinct_simulation_field_can_affect_area_membership() {
    let before = area_sheet();
    replace(
        &before,
        "\t\t(property \"Reference\" \"R1\"",
        "\t\t(property \"sim.enable\" \"X\" (at 40.132 38.1 90) (effects (font (size 1.27 1.27))))\n\t\t(property \"Reference\" \"R1\"",
    );
    let after = before.fork();
    replace(
        &after,
        "\"sim.enable\" \"X\" (at 40.132",
        "\"sim.enable\" \"X\" (at 31.75",
    );
    check(&before, &after, "attribute dnp", "case-distinct-sim-field").await;
}

async fn check_repeated_pin_text_style(tag: &str, text: &str, insert_before: &str) {
    let before = area_sheet();
    replace(
        &before,
        "(pin passive line\n\t\t\t\t\t(at 0 3.81 270)",
        "(pin passive inverted_clock\n\t\t\t\t\t(at 0 3.81 270)",
    );
    let after = before.fork();
    let mut data = fs::read_to_string(&after.path).unwrap();
    let pin = data.find("(pin passive inverted").unwrap();
    let end = pin + data[pin..].find(insert_before).unwrap();
    data.insert_str(
        end,
        &format!("\n\t\t\t\t\t({tag} \"{text}\" (effects (font (size 12 12))))"),
    );
    fs::write(&after.path, data).unwrap();
    check(
        &before,
        &after,
        "attribute dnp",
        &format!("repeated-pin-{tag}-style"),
    )
    .await;
}

#[tokio::test]
async fn a_repeated_pin_number_style_cannot_prove_area_separation() {
    check_repeated_pin_text_style("number", "1", "\n\t\t\t\t)").await;
}

#[tokio::test]
async fn a_repeated_pin_name_style_cannot_prove_area_separation() {
    check_repeated_pin_text_style("name", "", "\n\t\t\t\t\t(number \"1\"").await;
}

#[tokio::test]
async fn competing_body_style_spellings_cannot_prove_area_membership() {
    let before = area_sheet();
    replace(
        &before,
        "(symbol \"Device:R\"\n",
        "(symbol \"Device:R\"\n\t\t\t(body_styles demorgan)\n",
    );
    let mut data = fs::read_to_string(&before.path).unwrap();
    let start = data.find("\t\t\t(symbol \"R_0_1\"").unwrap();
    let end = data.find("\t\t\t(symbol \"R_1_1\"").unwrap();
    let normal = &data[start..end];
    let wide = normal.replace("(start -1.016 -2.54)", "(start -7 -2.54)");
    let alternate = normal.replace("R_0_1", "R_0_2");
    data.replace_range(start..end, &format!("{wide}{alternate}"));
    fs::write(&before.path, data).unwrap();
    replace(&before, "(symbol \"R_1_1\"", "(symbol \"R_1_0\"");
    let after = before.fork();
    replace(
        &after,
        "(lib_id \"Device:R\")\n\t\t(at 38.1 38.1 0)\n\t\t(unit 1)\n\t\t(body_style 1)",
        "(lib_id \"Device:R\")\n\t\t(at 38.1 38.1 0)\n\t\t(unit 1)\n\t\t(body_style 1)\n\t\t(convert 2)",
    );
    check(
        &before,
        &after,
        "attribute dnp",
        "competing-body-style-spellings",
    )
    .await;
}

#[tokio::test]
async fn mixed_pin_visibility_setters_cannot_hide_area_entry() {
    let before = area_sheet();
    replace(
        &before,
        "(pin passive line\n\t\t\t\t\t(at 0 3.81 270)",
        "(pin passive inverted_clock hide\n\t\t\t\t\t(at 0 3.81 270)",
    );
    let mut data = fs::read_to_string(&before.path).unwrap();
    let pin = data.find("(pin passive inverted_clock hide").unwrap();
    let size = pin + data[pin..].find("(size 1.27 1.27)").unwrap();
    data.replace_range(size..size + "(size 1.27 1.27)".len(), "(size 12 12)");
    fs::write(&before.path, data).unwrap();
    let after = before.fork();
    replace(
        &after,
        "(pin passive inverted_clock hide\n",
        "(pin passive inverted_clock hide (hide no)\n",
    );
    check(&before, &after, "attribute dnp", "mixed-pin-visibility").await;
}

fn sheet_with_two_resistor_libraries(power_alias: bool) -> Sheet {
    let before = area_sheet();
    let mut data = fs::read_to_string(&before.path).unwrap();
    let start = data.find("\t\t(symbol \"Device:R\"\n").unwrap();
    let end = data.find("\t\t(symbol \"power:+5V\"\n").unwrap();
    let mut library = data[start..end]
        .replace("\"Device:R\"", "\"Device:R_Wide\"")
        .replace("(symbol \"R_", "(symbol \"R_Wide_");
    if power_alias {
        library = library.replace(
            "(symbol \"Device:R_Wide\"\n",
            "(symbol \"Device:R_Wide\"\n\t\t\t(power global)\n",
        );
    }
    data.insert_str(end, &library);
    fs::write(&before.path, data).unwrap();
    before
}

fn widen_resistor_body(sheet: &Sheet, body: &str) {
    replace(
        sheet,
        &format!("(symbol \"{body}\"\n\t\t\t\t(rectangle\n\t\t\t\t\t(start -1.016 -2.54)"),
        &format!("(symbol \"{body}\"\n\t\t\t\t(rectangle\n\t\t\t\t\t(start -7.62 -2.54)"),
    );
}

#[tokio::test]
async fn escaped_library_alias_cannot_hide_area_entry() {
    let before = sheet_with_two_resistor_libraries(false);
    let data = fs::read_to_string(&before.path)
        .unwrap()
        .replace("\"Device:R\"", "\"Device:R/Area\"")
        .replace("\"Device:R_Wide\"", "\"Device:R{slash}Area\"")
        .replace("(symbol \"R_Wide_", "(symbol \"R{slash}Area_")
        .replace("(symbol \"R_", "(symbol \"R/Area_");
    fs::write(&before.path, data).unwrap();
    let after = before.fork();
    widen_resistor_body(&after, "R{slash}Area_0_1");
    check(&before, &after, "attribute dnp", "escaped-library-alias").await;
}

#[tokio::test]
async fn repeated_library_selection_cannot_hide_area_entry() {
    let before = sheet_with_two_resistor_libraries(false);
    replace(
        &before,
        "(lib_id \"Device:R\")\n\t\t(at 38.1 38.1 0)",
        "(lib_id \"Device:R\")\n\t\t(lib_name \"Device:R\")\n\t\t(lib_name \"Device:R_Wide\")\n\t\t(at 38.1 38.1 0)",
    );
    let after = before.fork();
    widen_resistor_body(&after, "R_Wide_0_1");
    check(
        &before,
        &after,
        "attribute dnp",
        "repeated-library-selection",
    )
    .await;
}

#[tokio::test]
async fn repeated_library_ids_cannot_hide_area_entry() {
    let before = sheet_with_two_resistor_libraries(false);
    replace(
        &before,
        "(lib_id \"Device:R\")\n\t\t(at 38.1 38.1 0)",
        "(lib_id \"Device:R\")\n\t\t(lib_id \"Device:R_Wide\")\n\t\t(at 38.1 38.1 0)",
    );
    let after = before.fork();
    widen_resistor_body(&after, "R_Wide_0_1");
    check(&before, &after, "attribute dnp", "repeated-library-ids").await;
}

#[tokio::test]
async fn ambiguous_library_selection_cannot_exempt_a_power_symbol() {
    let before = sheet_with_two_resistor_libraries(true);
    replace(
        &before,
        "(lib_id \"Device:R\")\n\t\t(at 38.1 38.1 0)",
        "(lib_id \"Device:R\")\n\t\t(lib_name \"Device:R_Wide\")\n\t\t(lib_name \"Device:R\")\n\t\t(at 38.1 38.1 0)",
    );
    let after = before.fork();
    widen_resistor_body(&after, "R_0_1");
    check(
        &before,
        &after,
        "attribute dnp",
        "ambiguous-power-exemption",
    )
    .await;
}

fn resistor_with_a_wide_alternate_body() -> Sheet {
    let before = area_sheet();
    replace(
        &before,
        "(symbol \"Device:R\"\n",
        "(symbol \"Device:R\"\n\t\t\t(body_styles demorgan)\n",
    );
    let mut data = fs::read_to_string(&before.path).unwrap();
    let start = data.find("\t\t\t(symbol \"R_0_1\"").unwrap();
    let end = data.find("\t\t\t(symbol \"R_1_1\"").unwrap();
    let wide = data[start..end].replace("R_0_1", "R_0_2");
    data.insert_str(end, &wide);
    fs::write(&before.path, data).unwrap();
    widen_resistor_body(&before, "R_0_2");
    replace(&before, "(symbol \"R_1_1\"", "(symbol \"R_1_0\"");
    replace(
        &before,
        "(lib_id \"Device:R\")\n\t\t(at 38.1 38.1 0)\n\t\t(unit 1)\n\t\t(body_style 1)",
        "(lib_id \"Device:R\")\n\t\t(at 38.1 38.1 0)\n\t\t(unit 1)\n\t\t(body_style 2)",
    );
    before
}

#[tokio::test]
async fn a_pruned_body_style_cannot_prove_area_membership() {
    let before = resistor_with_a_wide_alternate_body();
    let after = before.fork();
    replace(&after, "\t\t\t(body_styles demorgan)\n", "");
    check(&before, &after, "attribute dnp", "pruned-body-style").await;
}

#[tokio::test]
async fn a_root_library_drawing_cannot_prove_alternate_area_membership() {
    let before = resistor_with_a_wide_alternate_body();
    let after = before.fork();
    let mut data = fs::read_to_string(&after.path).unwrap();
    let start = data.find("\t\t\t(symbol \"R_0_2\"").unwrap();
    let drawing = start + data[start..].find("\n\t\t\t\t(rectangle").unwrap();
    let close = drawing + data[drawing..].find("\n\t\t\t)\n").unwrap();
    let direct_drawing = data[drawing..close].to_owned();
    data.replace_range(start..close + "\n\t\t\t)\n".len(), "");
    let pins = data.find("\t\t\t(symbol \"R_1_0\"").unwrap();
    let end = pins + data[pins..].find("\n\t\t\t(embedded_fonts no)").unwrap();
    data.insert_str(end, &direct_drawing);
    fs::write(&after.path, data).unwrap();
    check(&before, &after, "attribute dnp", "root-library-drawing").await;
}

#[tokio::test]
async fn a_field_before_the_mirror_cannot_prove_area_separation() {
    let before = area_sheet();
    replace(
        &before,
        "(at 38.1 38.1 0)\n\t\t(unit 1)",
        "(at 38.1 38.1 0)\n\t\t(mirror y)\n\t\t(unit 1)",
    );
    replace(
        &before,
        "(property \"Reference\" \"R1\"\n\t\t\t(at 40.132 38.1 90)",
        "(property \"Reference\" \"R1\"\n\t\t\t(at 44.45 38.1 90)",
    );
    let after = before.fork();
    replace(&after, "\t\t(mirror y)\n", "");
    replace(
        &after,
        "\t\t(property \"Value\" \"10k\"\n\t\t\t(at 38.1 38.1 90)",
        "\t\t(mirror y)\n\t\t(property \"Value\" \"10k\"\n\t\t\t(at 38.1 38.1 90)",
    );
    check(&before, &after, "attribute dnp", "field-before-mirror").await;
}

#[tokio::test]
async fn a_mirror_reset_by_position_cannot_prove_area_separation() {
    let before = area_sheet();
    widen_resistor_body(&before, "R_0_1");
    replace(
        &before,
        "(at 38.1 38.1 0)\n\t\t(unit 1)",
        "(at 38.1 38.1 0)\n\t\t(mirror y)\n\t\t(unit 1)",
    );
    let after = before.fork();
    replace(
        &after,
        "(at 38.1 38.1 0)\n\t\t(mirror y)\n\t\t(unit 1)",
        "(mirror y)\n\t\t(at 38.1 38.1 0)\n\t\t(unit 1)",
    );
    check(&before, &after, "attribute dnp", "mirror-before-position").await;
}

#[tokio::test]
async fn embedded_metadata_and_rule_area_changes_are_both_reported() {
    let before = area_sheet();
    let after = before.fork();
    replace(
        &after,
        "(property \"Reference\" \"R1\"\n\t\t\t(at 40.132 38.1 90)",
        "(property \"Reference\" \"R1\"\n\t\t\t(at 31.75 38.1 90)",
    );
    replace(
        &after,
        "(duplicate_pin_numbers_are_jumpers no)\n\t\t\t(property \"Reference\" \"R\"",
        "(duplicate_pin_numbers_are_jumpers yes)\n\t\t\t(property \"Reference\" \"R\"",
    );
    let result = compare(&handler().await, &before, &after, &[]).await;
    assert_verdict(&result, "blocked");
    let reasons = reasons(&result);
    assert!(reasons.contains("(R1) changed attribute dnp"), "{result}");
    assert!(
        reasons.contains("changed duplicate_pin_numbers_are_jumpers"),
        "{result}"
    );
}
