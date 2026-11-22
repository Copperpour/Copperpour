//! KiCad's native do-not-populate attribute through the component tools
//! (#415), and its Exclude from BOM and Exclude from board flags (#34).
//! The fixture is KiCad-saved; see its README.
use super::*;
use crate::mcp::{error::extract_error_kind, protocol::ToolContent};
use crate::tools::ServerConfig;
use copperpour_sexp::SexpNode;
use std::sync::Arc;

const SHEET: &str = include_str!("../../../tests/fixtures/dnp_kicad10.kicad_sch");

fn context() -> Arc<ToolContext> {
    Arc::new(ToolContext::new(
        ServerConfig {
            kicad_cli: String::new(),
            kicad_binary: String::new(),
            ipc_address: String::new(),
            project_dir: None,
            jlcpcb_db_path: None,
            auto_load_toolsets: false,
            eager_toolsets: false,
        },
        Arc::new(crate::tools::ToolRouter::new()),
    ))
}

fn fixture(content: &str) -> (tempfile::TempDir, std::path::PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("dnp.kicad_sch");
    std::fs::write(&path, content).unwrap();
    (dir, path)
}

/// A call straight into the handler, for the fault hooks (thread-local)
/// and for the handler's own checks behind the served schema.
async fn call_direct(
    tool: &str,
    path: &std::path::Path,
    mut args: serde_json::Value,
) -> CallToolResult {
    args["schematic"] = json!(path.display().to_string());
    let def = tools()
        .into_iter()
        .chain(crate::tools::sch_batch::tools())
        .find(|def| def.name == tool)
        .unwrap();
    (def.handler)(&args, context()).await.unwrap()
}

/// A served `tools/call`, as a client sends it: the dispatch validates the
/// advertised schema before the handler runs.
async fn call(tool: &str, path: &std::path::Path, mut args: serde_json::Value) -> CallToolResult {
    args["schematic"] = json!(path.display().to_string());
    let handler = crate::mcp::handler::McpHandler::new(ServerConfig {
        kicad_cli: String::new(),
        kicad_binary: String::new(),
        ipc_address: String::new(),
        project_dir: None,
        jlcpcb_db_path: None,
        auto_load_toolsets: false,
        eager_toolsets: true,
    })
    .await
    .unwrap();
    let response = handler
        .handle_message(json!({
            "jsonrpc": "2.0",
            "id": 415,
            "method": "tools/call",
            "params": { "name": tool, "arguments": args }
        }))
        .await
        .expect("tools/call receives a response");
    serde_json::from_value(response.result.expect("a JSON-RPC result")).unwrap()
}

fn text(result: &CallToolResult) -> String {
    match result.content.first() {
        Some(ToolContent::Text { text }) => text.clone(),
        other => panic!("expected text, got {other:?}"),
    }
}

fn body(result: &CallToolResult) -> serde_json::Value {
    assert!(!result.is_error, "{}", text(result));
    serde_json::from_str(&text(result)).unwrap()
}

/// Each placed unit of `reference` as (unit, the value of its direct
/// `(dnp …)` child), read with the generic S-expression parser rather than
/// the symbol model the tool writes through. `None` is a unit with no
/// token.
fn native_dnp(content: &str, reference: &str) -> Vec<(u32, Option<String>)> {
    let tree = parse_sexp(content).unwrap();
    let mut units = tree
        .children()
        .unwrap()
        .iter()
        .filter(|node| node.head() == Some("symbol") && node.find("lib_id").is_some())
        .filter(|node| {
            node.find_all("property").iter().any(|property| {
                property.get(1).and_then(SexpNode::as_str) == Some("Reference")
                    && property.get(2).and_then(SexpNode::as_str) == Some(reference)
            })
        })
        .map(|node| {
            let unit = node.find_str("unit").unwrap().parse().unwrap();
            let dnp = node
                .children()
                .unwrap()
                .iter()
                .filter(|child| child.head() == Some("dnp"))
                .map(|child| child.get(1).and_then(SexpNode::as_str).unwrap().to_owned())
                .collect::<Vec<_>>();
            assert!(dnp.len() <= 1, "one (dnp …) per placed unit");
            (unit, dnp.into_iter().next())
        })
        .collect::<Vec<_>>();
    units.sort();
    units
}

fn all_units(content: &str, reference: &str, value: &str) -> bool {
    let units = native_dnp(content, reference);
    !units.is_empty() && units.iter().all(|(_, dnp)| dnp.as_deref() == Some(value))
}

/// Byte ranges of the placed `(symbol …)` blocks carrying `reference`,
/// found by KiCad's own layout — a placed symbol opens on a line
/// `\t(symbol` and closes on the next line `\t)` — rather than by the
/// block finder the tool writes through.
fn kicad_blocks(content: &str, reference: &str) -> Vec<(usize, usize)> {
    let mut blocks = Vec::new();
    let mut from = 0;
    while let Some(rel) = content[from..].find("\n\t(symbol\n") {
        let start = from + rel + 2;
        let end = start + content[start..].find("\n\t)\n").expect("the block closes") + 3;
        if content[start..end].contains(&format!("\n\t\t(property \"Reference\" \"{reference}\"")) {
            blocks.push((start, end));
        }
        from = end;
    }
    blocks
}

/// The fixture with one placed unit's token flipped, as KiCad itself
/// writes it: the expected file is built from KiCad's text, not from the
/// tool's.
fn kicad_with(content: &str, reference: &str, from: &str, to: &str) -> String {
    let mut out = content.to_owned();
    for (start, end) in kicad_blocks(content, reference) {
        let block = &content[start..end];
        let flipped = block.replacen(
            &format!("\n\t\t(dnp {from})"),
            &format!("\n\t\t(dnp {to})"),
            1,
        );
        assert_ne!(block, flipped, "{reference} carries (dnp {from})");
        out = out.replacen(block, &flipped, 1);
    }
    out
}

#[tokio::test]
async fn false_true_false_round_trips_to_kicads_own_bytes() {
    let (_dir, path) = fixture(SHEET);
    let on = body(
        &call(
            "edit_schematic_component",
            &path,
            json!({ "reference": "R1", "dnp": true }),
        )
        .await,
    );
    let saved = std::fs::read_to_string(&path).unwrap();
    assert_eq!(saved, kicad_with(SHEET, "R1", "no", "yes"));
    assert_eq!(on["dnp"], true, "{on}");
    assert_eq!(on["units"][0]["dnp"], true, "{on}");

    let off = body(
        &call(
            "edit_schematic_component",
            &path,
            json!({ "reference": "R1", "dnp": false }),
        )
        .await,
    );
    assert_eq!(
        std::fs::read_to_string(&path).unwrap(),
        SHEET,
        "back to KiCad's bytes"
    );
    assert_eq!(off["dnp"], false, "{off}");
}

/// R1's KiCad 10 design-variant clause carries its own `(dnp yes)` inside
/// `(instances …)`. It is a per-variant override, not the attribute, and
/// must survive both directions untouched.
#[tokio::test]
async fn a_variant_override_is_not_the_attribute() {
    let variant = "(variant\n\t\t\t\t\t\t(name \"Lite\")\n\t\t\t\t\t\t(dnp yes)\n\t\t\t\t\t)";
    assert!(SHEET.contains(variant));
    let (_dir, path) = fixture(SHEET);
    for dnp in [true, false] {
        body(
            &call(
                "edit_schematic_component",
                &path,
                json!({ "reference": "R1", "dnp": dnp }),
            )
            .await,
        );
        let saved = std::fs::read_to_string(&path).unwrap();
        assert!(saved.contains(variant), "variant clause intact:\n{saved}");
        let expected = if dnp { "yes" } else { "no" };
        assert_eq!(
            native_dnp(&saved, "R1"),
            vec![(1, Some(expected.to_owned()))]
        );
    }
}

#[tokio::test]
async fn every_placed_unit_takes_the_attribute() {
    let (_dir, path) = fixture(SHEET);
    let result = body(
        &call(
            "edit_schematic_component",
            &path,
            json!({ "reference": "U1", "dnp": true }),
        )
        .await,
    );
    let saved = std::fs::read_to_string(&path).unwrap();
    assert_eq!(saved, kicad_with(SHEET, "U1", "no", "yes"));
    assert_eq!(native_dnp(&saved, "U1").len(), 3);
    assert!(all_units(&saved, "U1", "yes"));
    let units = result["units"].as_array().unwrap();
    assert_eq!(units.len(), 3, "{result}");
    assert!(units.iter().all(|unit| unit["dnp"] == true), "{result}");
    assert!(text(
        &call(
            "edit_schematic_component",
            &path,
            json!({ "reference": "U1", "dnp": true })
        )
        .await
    )
    .contains("3 unit(s)"));
}

/// Readback comes from the file: C1 is DNP in the KiCad-saved fixture
/// with nothing written, and a write shows up in both readers.
/// The list leaves out properties each row already carries, empty ones
/// and the library's Datasheet and Description; verbose keeps them all.
#[tokio::test]
async fn list_properties_are_compact_unless_verbose() {
    let (_dir, path) = fixture(SHEET);
    body(
        &call(
            "edit_schematic_component",
            &path,
            json!({ "reference": "R1", "fields": { "MPN": "RC0805FR-0710KL" } }),
        )
        .await,
    );
    let row = |list: &serde_json::Value| {
        list["components"]
            .as_array()
            .unwrap()
            .iter()
            .find(|row| row["reference"] == "R1")
            .cloned()
            .unwrap()
    };
    let compact = row(&body(
        &call("list_schematic_components", &path, json!({})).await,
    ));
    assert_eq!(compact["properties"]["MPN"], "RC0805FR-0710KL", "{compact}");
    for absent in [
        "Reference",
        "Value",
        "Footprint",
        "Datasheet",
        "Description",
    ] {
        assert!(
            compact["properties"].get(absent).is_none(),
            "{absent} in {compact}"
        );
    }
    assert!(compact["value"].is_string());

    let full = row(&body(
        &call(
            "list_schematic_components",
            &path,
            json!({ "verbose": true }),
        )
        .await,
    ));
    assert_eq!(full["properties"]["Reference"], "R1", "{full}");
    assert_eq!(full["properties"]["MPN"], "RC0805FR-0710KL");
}

#[tokio::test]
async fn get_and_list_report_the_stored_attribute() {
    let (_dir, path) = fixture(SHEET);
    let c1 = body(
        &call(
            "get_schematic_component",
            &path,
            json!({ "reference": "C1" }),
        )
        .await,
    );
    assert_eq!(c1["dnp"], true, "{c1}");
    assert_eq!(c1["units"][0]["dnp"], true, "{c1}");

    body(
        &call(
            "edit_schematic_component",
            &path,
            json!({ "reference": "U1", "dnp": true }),
        )
        .await,
    );
    let u1 = body(
        &call(
            "get_schematic_component",
            &path,
            json!({ "reference": "U1" }),
        )
        .await,
    );
    assert_eq!(u1["dnp"], true, "{u1}");
    assert!(
        u1["units"]
            .as_array()
            .unwrap()
            .iter()
            .all(|unit| unit["dnp"] == true),
        "{u1}"
    );

    let list = body(&call("list_schematic_components", &path, json!({})).await);
    let rows = list["components"].as_array().unwrap();
    let dnp_of = |reference: &str| {
        rows.iter()
            .filter(|row| row["reference"] == reference)
            .map(|row| row["dnp"].as_bool().unwrap())
            .collect::<Vec<_>>()
    };
    assert_eq!(dnp_of("R1"), [false]);
    assert_eq!(dnp_of("C1"), [true]);
    assert_eq!(dnp_of("U1"), [true, true, true]);
}

#[tokio::test]
async fn fields_and_dnp_land_in_one_write() {
    let (_dir, path) = fixture(SHEET);
    let result = body(
            &call(
                "edit_schematic_component",
                &path,
                json!({ "reference": "U1", "value": "TL072", "fields": { "MPN": "MPN-415" }, "dnp": true }),
            )
            .await,
        );
    let saved = std::fs::read_to_string(&path).unwrap();
    assert!(all_units(&saved, "U1", "yes"));
    assert_eq!(saved.matches("(property \"Value\" \"TL072\"").count(), 3);
    assert_eq!(saved.matches("(property \"MPN\" \"MPN-415\"").count(), 3);
    assert_eq!(result["value"], "TL072", "{result}");
    assert_eq!(result["dnp"], true, "{result}");
}

#[tokio::test]
async fn a_missing_reference_writes_nothing() {
    let (_dir, path) = fixture(SHEET);
    let result = call(
        "edit_schematic_component",
        &path,
        json!({ "reference": "R99", "dnp": true }),
    )
    .await;
    assert!(result.is_error, "{}", text(&result));
    assert_eq!(std::fs::read_to_string(&path).unwrap(), SHEET);
}

/// Served, the schema refuses a non-boolean `dnp`; the handler refuses it
/// too, for direct callers. Neither writes.
#[tokio::test]
async fn a_malformed_dnp_is_refused_before_writing() {
    let (_dir, path) = fixture(SHEET);
    let args = json!({ "reference": "R1", "dnp": "yes", "value": "1k" });
    for result in [
        call("edit_schematic_component", &path, args.clone()).await,
        call_direct("edit_schematic_component", &path, args).await,
    ] {
        assert!(result.is_error);
        assert_eq!(
            extract_error_kind(&result).as_deref(),
            Some("invalid_argument")
        );
        assert_eq!(std::fs::read_to_string(&path).unwrap(), SHEET);
    }
}

/// The false workaround #415 reports: a property named DNP. It is refused
/// in any case, creates nothing, does not touch the attribute, and takes
/// the rest of the request down with it.
#[tokio::test]
async fn a_dnp_property_refuses_the_whole_request() {
    for fields in [
        json!({ "DNP": "yes" }),
        json!({ "dnp": "yes" }),
        json!({ "Dnp": "yes" }),
        json!({ "MPN": "MPN-415", "DNP": "yes" }),
    ] {
        let (_dir, path) = fixture(SHEET);
        let result = call(
            "edit_schematic_component",
            &path,
            json!({ "reference": "R1", "value": "4k7", "fields": fields }),
        )
        .await;
        assert!(result.is_error, "{fields}: {}", text(&result));
        assert_eq!(
            extract_error_kind(&result).as_deref(),
            Some("invalid_argument")
        );
        assert!(
            text(&result).contains("'dnp' argument"),
            "{}",
            text(&result)
        );
        assert_eq!(std::fs::read_to_string(&path).unwrap(), SHEET, "{fields}");
    }
}

/// R1 carries a `DNP` custom property in the KiCad-saved fixture, written
/// the way #415 reports. It is neither refused on other edits nor removed,
/// and the readback tells it apart from the attribute.
#[tokio::test]
async fn an_existing_dnp_property_is_left_alone() {
    assert!(SHEET.contains("\t\t(property \"DNP\" \"yes\""));
    let (_dir, path) = fixture(SHEET);
    let before = body(
        &call(
            "get_schematic_component",
            &path,
            json!({ "reference": "R1" }),
        )
        .await,
    );
    assert_eq!(before["dnp"], false, "{before}");
    assert_eq!(before["properties"]["DNP"], "yes", "{before}");

    let result = body(
        &call(
            "edit_schematic_component",
            &path,
            json!({ "reference": "R1", "dnp": true }),
        )
        .await,
    );
    let saved = std::fs::read_to_string(&path).unwrap();
    assert_eq!(saved, kicad_with(SHEET, "R1", "no", "yes"));
    assert_eq!(result["fields"]["DNP"], "yes", "{result}");
    assert_eq!(result["dnp"], true, "{result}");
}

/// `remove_fields` can clear the stray `DNP` property #415 describes, in
/// the same write that sets the native attribute. It does not touch the
/// attribute, and `fields` still refuses a property named DNP, whether or
/// not the same call removes one.
#[tokio::test]
async fn remove_fields_clears_a_dnp_property_beside_the_native_attribute() {
    let (_dir, path) = fixture(SHEET);
    let result = body(
        &call(
            "edit_schematic_component",
            &path,
            json!({ "reference": "R1", "dnp": true, "remove_fields": ["DNP"] }),
        )
        .await,
    );
    assert_eq!(result["removed_fields"], json!(["DNP"]), "{result}");
    assert_eq!(result["dnp"], true, "{result}");
    assert!(result["fields"].get("DNP").is_none(), "{result}");
    let saved = std::fs::read_to_string(&path).unwrap();
    assert!(!saved.contains("(property \"DNP\""));

    let (_dir, path) = fixture(SHEET);
    let refused = call(
        "edit_schematic_component",
        &path,
        json!({ "reference": "R1", "fields": { "DNP": "yes" }, "remove_fields": ["DNP"] }),
    )
    .await;
    assert!(refused.is_error, "{}", text(&refused));
    assert!(
        text(&refused).contains("'dnp' argument"),
        "{}",
        text(&refused)
    );
    assert_eq!(std::fs::read_to_string(&path).unwrap(), SHEET);
}

/// `"DNP": true` is refused as DNP, not as a non-string value, so the
/// caller is pointed at the native argument on the first try.
#[tokio::test]
async fn a_boolean_dnp_field_names_the_native_argument() {
    let (_dir, path) = fixture(SHEET);
    let result = call(
        "edit_schematic_component",
        &path,
        json!({ "reference": "R1", "fields": { "DNP": true } }),
    )
    .await;
    assert!(result.is_error);
    assert!(
        text(&result).contains("'dnp' argument"),
        "{}",
        text(&result)
    );
    // Served, the batch schema refuses a non-string field value first;
    // a direct call reaches the handler's own refusal.
    let result = call_direct(
        "batch_edit_schematic_components",
        &path,
        json!({ "edits": [{ "reference": "R1", "fields": { "DNP": true } }] }),
    )
    .await;
    assert!(result.is_error);
    assert!(
        text(&result).contains("'dnp' argument"),
        "{}",
        text(&result)
    );
    assert_eq!(std::fs::read_to_string(&path).unwrap(), SHEET);
}

/// `unit` scopes field_placements only; beside `dnp` alone it would read
/// as one gate while every unit changed.
#[tokio::test]
async fn unit_with_dnp_alone_is_refused() {
    let (_dir, path) = fixture(SHEET);
    let result = call(
        "edit_schematic_component",
        &path,
        json!({ "reference": "U1", "unit": 2, "dnp": true }),
    )
    .await;
    assert!(result.is_error);
    assert_eq!(
        extract_error_kind(&result).as_deref(),
        Some("invalid_argument")
    );
    assert_eq!(std::fs::read_to_string(&path).unwrap(), SHEET);
}

/// The batch reports the state read back from the written file, and a
/// write that did not hold the requested state is uncertain, not success.
#[tokio::test(flavor = "current_thread")]
async fn batch_readback_catches_a_write_that_did_not_land() {
    for (from, to) in [
        (
            "(dnp yes)\n\t\t(uuid \"d324d24a",
            "(dnp no)\n\t\t(uuid \"d324d24a",
        ),
        ("(kicad_sch", "(kicad_sch (("),
    ] {
        let (_dir, path) = fixture(SHEET);
        crate::tools::sch_batch::BATCH_WRITE_FAULT.with(|fault| {
            *fault.borrow_mut() = Some((from.to_owned(), to.to_owned()));
        });
        let result = call_direct(
            "batch_edit_schematic_components",
            &path,
            json!({ "edits": [{ "reference": "R1", "dnp": true }] }),
        )
        .await;
        assert!(result.is_error, "{from}: {}", text(&result));
        assert_eq!(
            extract_error_kind(&result).as_deref(),
            Some("mutation_outcome_uncertain"),
            "{from}: {}",
            text(&result)
        );
        assert!(
            text(&result).contains("Entries in the write: R1 (dnp → yes)"),
            "{from}: {}",
            text(&result)
        );
    }
}

#[tokio::test]
async fn an_annotation_named_dnp_is_refused() {
    let (_dir, path) = fixture(SHEET);
    let result = call(
        "add_component_annotation",
        &path,
        json!({ "reference": "R1", "key": "DNP", "value": "yes" }),
    )
    .await;
    assert!(result.is_error);
    assert_eq!(
        extract_error_kind(&result).as_deref(),
        Some("invalid_argument")
    );
    assert!(
        text(&result).contains("'dnp' argument"),
        "{}",
        text(&result)
    );
    assert_eq!(std::fs::read_to_string(&path).unwrap(), SHEET);
}

/// The fixture with every `(dnp …)` stripped, as a file from before
/// KiCad 7 has none, and what setting U1 DNP must make of it: KiCad's own
/// file with U1 marked DNP and every other symbol's token stripped.
fn tokenless_and_u1_dnp() -> (String, String) {
    let strip = |sheet: &str| {
        sheet
            .replace("\n\t\t(dnp no)", "")
            .replace("\n\t\t(dnp yes)", "")
    };
    let mut expected = kicad_with(SHEET, "U1", "no", "yes");
    for (start, end) in kicad_blocks(&expected, "R1")
        .into_iter()
        .chain(kicad_blocks(&expected, "C1"))
        .collect::<BTreeSet<_>>()
        .into_iter()
        .rev()
    {
        let block = strip(&expected[start..end]);
        expected.replace_range(start..end, &block);
    }
    (strip(SHEET), expected)
}

/// A file from before KiCad 7 has no `(dnp …)`. The attribute goes where
/// KiCad 10 writes it: each unit lands on KiCad's own bytes.
#[tokio::test]
async fn a_symbol_without_the_token_gets_it_where_kicad_writes_it() {
    let (tokenless, expected) = tokenless_and_u1_dnp();
    assert!(native_dnp(&tokenless, "U1")
        .iter()
        .all(|(_, dnp)| dnp.is_none()));
    let (_dir, path) = fixture(&tokenless);
    let result = body(
        &call(
            "edit_schematic_component",
            &path,
            json!({ "reference": "U1", "dnp": true }),
        )
        .await,
    );
    assert_eq!(std::fs::read_to_string(&path).unwrap(), expected);
    assert!(result["units"]
        .as_array()
        .unwrap()
        .iter()
        .all(|unit| unit["dnp"] == true));
}

/// The same in a file with CRLF line endings: the inserted line takes the
/// file's ending, not a bare LF.
#[tokio::test]
async fn an_inserted_token_keeps_the_files_line_endings() {
    let (tokenless, expected) = tokenless_and_u1_dnp();
    let (_dir, path) = fixture(&tokenless.replace('\n', "\r\n"));
    body(
        &call(
            "edit_schematic_component",
            &path,
            json!({ "reference": "U1", "dnp": true }),
        )
        .await,
    );
    assert_eq!(
        std::fs::read_to_string(&path).unwrap(),
        expected.replace('\n', "\r\n")
    );
}

/// A unit whose `(dnp …)` cannot be rewritten is a malformed file: the
/// edit aborts, and the field edit beside it is not written either.
#[tokio::test]
async fn a_malformed_dnp_token_aborts_the_whole_edit() {
    let (start, end) = kicad_blocks(SHEET, "R1")[0];
    let mut doubled = SHEET.to_owned();
    doubled.replace_range(
        start..end,
        &SHEET[start..end].replacen("\n\t\t(dnp no)", "\n\t\t(dnp no)\n\t\t(dnp no)", 1),
    );
    let (_dir, path) = fixture(&doubled);
    let result = call(
        "edit_schematic_component",
        &path,
        json!({ "reference": "R1", "value": "4k7", "dnp": true }),
    )
    .await;
    assert!(result.is_error, "{}", text(&result));
    assert_eq!(extract_error_kind(&result).as_deref(), Some("stale_target"));
    assert!(text(&result).contains("more than one"), "{}", text(&result));
    assert_eq!(std::fs::read_to_string(&path).unwrap(), doubled);
}

/// The edit is bound to the component's own units by UUID. Renamed onto a
/// designator another part already carries, it writes and counts only its
/// own unit; the other part keeps its state.
#[tokio::test]
async fn a_rename_onto_an_existing_designator_writes_only_the_bound_units() {
    let (_dir, path) = fixture(SHEET);
    let result = body(
        &call(
            "edit_schematic_component",
            &path,
            json!({ "reference": "R1", "new_reference": "C1", "dnp": false }),
        )
        .await,
    );
    assert!(
        result["changes"]
            .as_array()
            .unwrap()
            .iter()
            .any(|change| change == "dnp → no (1 unit(s))"),
        "{result}"
    );
    let saved = std::fs::read_to_string(&path).unwrap();
    let mut states = native_dnp(&saved, "C1");
    states.sort();
    assert_eq!(
        states,
        vec![(1, Some("no".to_owned())), (1, Some("yes".to_owned()))],
        "the original C1 keeps (dnp yes)"
    );
}

/// KiCad 6 writes the header on shared lines,
/// `(in_bom yes) (on_board yes) (fields_autoplaced)`; the attribute joins
/// that line after `(on_board …)`, where KiCad 7 put it. The sheet is in
/// KiCad 6.0's layout (format 20211123): its header tokens share lines as
/// that save writes them. KiCad 10 cannot write that format; the check on
/// a real KiCad 6 sheet is recorded in the fixture README.
#[test]
fn a_kicad6_header_line_takes_the_attribute_inline() {
    let sheet = "(kicad_sch (version 20211123) (generator eeschema)\n\n  (uuid d3800c7a-8408-4f38-ad1f-1122869af9fb)\n\n  (paper \"A4\")\n\n  (lib_symbols)\n\n  (symbol (lib_id \"Device:R\") (at 100 50 0) (unit 1)\n    (in_bom yes) (on_board yes) (fields_autoplaced)\n    (uuid ae99f7c5-465b-44fc-bc42-3808dbc52945)\n    (property \"Reference\" \"R1\" (id 0) (at 102 49 0)\n      (effects (font (size 1.27 1.27)) (justify left))\n    )\n  )\n)\n";
    let (written, units) = set_dnp_attribute(sheet, "R1", true).unwrap();
    assert_eq!(units, 1);
    assert_eq!(
        written,
        sheet.replace(
            "(on_board yes) (fields_autoplaced)",
            "(on_board yes) (dnp yes) (fields_autoplaced)"
        )
    );
}

/// The committed-file readback is what catches a writer that reports
/// DNP but leaves the token as it was.
#[tokio::test(flavor = "current_thread")]
async fn a_write_that_misses_the_token_is_refused_before_commit() {
    let (_dir, path) = fixture(SHEET);
    COMPONENT_PROSPECTIVE_FAULT.with(|fault| {
        *fault.borrow_mut() = Some((
            "(dnp yes)\n\t\t(uuid \"d324d24a".to_owned(),
            "(dnp no)\n\t\t(uuid \"d324d24a".to_owned(),
        ));
    });
    let result = call_direct(
        "edit_schematic_component",
        &path,
        json!({ "reference": "R1", "dnp": true }),
    )
    .await;
    assert!(result.is_error, "{}", text(&result));
    assert_eq!(
        extract_error_kind(&result).as_deref(),
        Some("stale_target"),
        "{}",
        text(&result)
    );
    assert!(text(&result).contains("dnp"), "{}", text(&result));
    assert_eq!(std::fs::read_to_string(&path).unwrap(), SHEET);
}

/// A package whose units disagree, as KiCad saves one, has no single DNP
/// state for the batch readback to report.
#[test]
fn units_that_disagree_have_no_one_dnp_state() {
    let (start, end) = kicad_blocks(SHEET, "U1")[0];
    let mut mixed = SHEET.to_owned();
    mixed.replace_range(
        start..end,
        &SHEET[start..end].replacen("(dnp no)", "(dnp yes)", 1),
    );
    let (_dir, path) = fixture(&mixed);
    let mixed = cse::Schematic::load(&path).unwrap();
    let error = crate::tools::sch_batch::observed_dnp(&mixed, "U1").unwrap_err();
    assert!(error.contains("disagree"), "{error}");
    assert_eq!(
        crate::tools::sch_batch::observed_dnp(&mixed, "C1"),
        Ok(true)
    );
    assert!(crate::tools::sch_batch::observed_dnp(&mixed, "R99").is_err());
}

/// Two `(dnp …)` children leave nothing sound to rewrite, and a block
/// whose header cannot be found has nowhere to put one.
#[test]
fn a_symbol_with_two_tokens_or_no_header_is_refused() {
    let (start, end) = kicad_blocks(SHEET, "R1")[0];
    let doubled = SHEET[start..end].replacen("\n\t\t(dnp no)", "\n\t\t(dnp no)\n\t\t(dnp no)", 1);
    let error = set_dnp_attribute(&doubled, "R1", true).unwrap_err();
    assert!(error.contains("more than one"), "{error}");

    let headless = "(symbol (property \"Note\" \"(lib_id x)\") (property \"Reference\" \"R1\"))";
    let error = set_dnp_attribute(headless, "R1", true).unwrap_err();
    assert!(error.contains("header"), "{error}");
}

#[tokio::test]
async fn batch_sets_the_attribute_on_several_parts_in_one_write() {
    let (_dir, path) = fixture(SHEET);
    let result = body(
        &call(
            "batch_edit_schematic_components",
            &path,
            json!({ "edits": [
                    { "reference": "R1", "dnp": true, "value": "4k7" },
                    { "reference": "U1", "dnp": true },
                    { "reference": "C1", "dnp": false }
                ] }),
        )
        .await,
    );
    let saved = std::fs::read_to_string(&path).unwrap();
    let expected = kicad_with(
        &kicad_with(&kicad_with(SHEET, "R1", "no", "yes"), "U1", "no", "yes"),
        "C1",
        "yes",
        "no",
    )
    .replacen(
        "\n\t\t(property \"Value\" \"R\"",
        "\n\t\t(property \"Value\" \"4k7\"",
        1,
    );
    assert_eq!(saved, expected);
    let updated = result["updated"].as_array().unwrap();
    assert_eq!(updated.len(), 3, "{result}");
    assert_eq!(updated[0]["dnp"], true);
    assert_eq!(updated[1]["dnp"], true);
    assert_eq!(updated[2]["dnp"], false);
    assert!(
        updated[1]["changes"][0]
            .as_str()
            .unwrap()
            .contains("3 units"),
        "{result}"
    );
    assert_eq!(result["errors"], json!([]), "{result}");
}

#[tokio::test]
async fn batch_refuses_a_dnp_property_without_applying_any_entry() {
    let (_dir, path) = fixture(SHEET);
    let result = call(
        "batch_edit_schematic_components",
        &path,
        json!({ "create_missing": true, "edits": [
                { "reference": "U1", "dnp": true, "value": "TL072" },
                { "reference": "R1", "fields": { "DNP": "yes" } }
            ] }),
    )
    .await;
    assert!(result.is_error, "{}", text(&result));
    assert_eq!(
        extract_error_kind(&result).as_deref(),
        Some("invalid_argument")
    );
    assert!(
        text(&result).contains("edits[1].fields"),
        "{}",
        text(&result)
    );
    assert_eq!(std::fs::read_to_string(&path).unwrap(), SHEET);
}

#[tokio::test]
async fn batch_reports_a_missing_reference_and_a_malformed_dnp_without_writing_them() {
    let (_dir, path) = fixture(SHEET);
    let result = body(
        &call(
            "batch_edit_schematic_components",
            &path,
            json!({ "edits": [{ "reference": "R99", "dnp": true }] }),
        )
        .await,
    );
    assert_eq!(std::fs::read_to_string(&path).unwrap(), SHEET);
    assert_eq!(result["updated_count"], 0, "{result}");
    assert!(
        result["errors"][0].as_str().unwrap().contains("R99"),
        "{result}"
    );

    // Served, the schema refuses the whole call; direct, the handler
    // reports the entry and writes nothing for it.
    let served = call(
        "batch_edit_schematic_components",
        &path,
        json!({ "edits": [{ "reference": "C1", "dnp": "no" }] }),
    )
    .await;
    assert_eq!(
        extract_error_kind(&served).as_deref(),
        Some("invalid_argument")
    );
    let direct = body(
        &call_direct(
            "batch_edit_schematic_components",
            &path,
            json!({ "edits": [{ "reference": "C1", "dnp": "no" }] }),
        )
        .await,
    );
    assert!(
        direct["errors"][0]
            .as_str()
            .unwrap()
            .contains("must be a boolean"),
        "{direct}"
    );
    assert_eq!(std::fs::read_to_string(&path).unwrap(), SHEET);
}

/// Omitting `dnp` leaves the attribute as the file has it, on both tools:
/// C1 is DNP in the KiCad-saved fixture and stays so through other edits.
#[tokio::test]
async fn omitting_dnp_leaves_the_attribute_alone() {
    let (_dir, path) = fixture(SHEET);
    let edited = body(
        &call(
            "edit_schematic_component",
            &path,
            json!({ "reference": "C1", "value": "100n" }),
        )
        .await,
    );
    assert_eq!(edited["dnp"], true, "{edited}");
    body(
        &call(
            "batch_edit_schematic_components",
            &path,
            json!({ "edits": [
                    { "reference": "C1", "footprint": "Capacitor_SMD:C_0603_1608Metric" },
                    { "reference": "R1", "value": "4k7" }
                ] }),
        )
        .await,
    );
    let saved = std::fs::read_to_string(&path).unwrap();
    assert_eq!(native_dnp(&saved, "C1"), native_dnp(SHEET, "C1"));
    assert_eq!(native_dnp(&saved, "R1"), native_dnp(SHEET, "R1"));
    assert_eq!(native_dnp(&saved, "U1"), native_dnp(SHEET, "U1"));
}

/// KiCad's editor lock refuses a DNP edit like any other component edit.
#[tokio::test]
async fn a_sheet_open_in_kicad_refuses_a_dnp_edit() {
    let (_dir, path) = fixture(SHEET);
    let lock = path.with_file_name(format!(
        "~{}.lck",
        path.file_name().unwrap().to_string_lossy()
    ));
    std::fs::write(&lock, "locked").unwrap();
    for (tool, args) in [
        (
            "edit_schematic_component",
            json!({ "reference": "R1", "dnp": true }),
        ),
        (
            "batch_edit_schematic_components",
            json!({ "edits": [{ "reference": "R1", "dnp": true }] }),
        ),
    ] {
        let result = call(tool, &path, args).await;
        assert!(result.is_error, "{tool}: {}", text(&result));
        assert_eq!(
            extract_error_kind(&result).as_deref(),
            Some("conflict"),
            "{tool}"
        );
        assert_eq!(std::fs::read_to_string(&path).unwrap(), SHEET, "{tool}");
    }
}

#[tokio::test]
async fn batch_refuses_a_second_dnp_for_one_reference() {
    let (_dir, path) = fixture(SHEET);
    let result = body(
            &call(
                "batch_edit_schematic_components",
                &path,
                json!({ "edits": [ { "reference": "R1", "dnp": true }, { "reference": "R1", "dnp": false } ] }),
            )
            .await,
        );
    let saved = std::fs::read_to_string(&path).unwrap();
    assert_eq!(
        saved,
        kicad_with(SHEET, "R1", "no", "yes"),
        "the first assignment wins"
    );
    assert!(
        result["errors"][0].as_str().unwrap().contains("Duplicate"),
        "{result}"
    );
}

// Exclude from BOM and Exclude from board, KiCad's native `(in_bom …)` and
// `(on_board …)`, set through the same path as `dnp` (#34).

/// Each placed unit of `reference` as (unit, the value of its direct
/// `(flag …)` child), read as `native_dnp` reads `(dnp …)`.
fn native_flag(content: &str, reference: &str, flag: &str) -> Vec<(u32, Option<String>)> {
    let tree = parse_sexp(content).unwrap();
    let mut units = tree
        .children()
        .unwrap()
        .iter()
        .filter(|node| node.head() == Some("symbol") && node.find("lib_id").is_some())
        .filter(|node| {
            node.find_all("property").iter().any(|property| {
                property.get(1).and_then(SexpNode::as_str) == Some("Reference")
                    && property.get(2).and_then(SexpNode::as_str) == Some(reference)
            })
        })
        .map(|node| {
            let values = node
                .children()
                .unwrap()
                .iter()
                .filter(|child| child.head() == Some(flag))
                .map(|child| child.get(1).and_then(SexpNode::as_str).unwrap().to_owned())
                .collect::<Vec<_>>();
            assert!(values.len() <= 1, "one ({flag} …) per placed unit");
            let unit = node.find_str("unit").unwrap().parse().unwrap();
            (unit, values.into_iter().next())
        })
        .collect::<Vec<_>>();
    units.sort();
    units
}

/// `kicad_with` for any of the flags: KiCad's text with `flag` flipped on
/// every placed unit of `reference`.
fn kicad_with_flag(content: &str, reference: &str, flag: &str, from: &str, to: &str) -> String {
    let mut out = content.to_owned();
    for (start, end) in kicad_blocks(content, reference) {
        let block = &content[start..end];
        let flipped = block.replacen(
            &format!("\n\t\t({flag} {from})"),
            &format!("\n\t\t({flag} {to})"),
            1,
        );
        assert_ne!(block, flipped, "{reference} carries ({flag} {from})");
        out = out.replacen(block, &flipped, 1);
    }
    out
}

/// Each flag goes to `no` and back on KiCad's own bytes, and the reply
/// reads it back; the other flags stay as the file has them.
#[tokio::test]
async fn in_bom_and_on_board_round_trip_to_kicads_own_bytes() {
    for (flag, other) in [("in_bom", "on_board"), ("on_board", "in_bom")] {
        let (_dir, path) = fixture(SHEET);
        let off = body(
            &call(
                "edit_schematic_component",
                &path,
                json!({ "reference": "C1", flag: false }),
            )
            .await,
        );
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            kicad_with_flag(SHEET, "C1", flag, "yes", "no"),
            "{flag}"
        );
        assert_eq!(off[flag], false, "{off}");
        assert_eq!(off["units"][0][flag], false, "{off}");
        assert_eq!(off[other], true, "{off}");
        assert_eq!(off["dnp"], true, "C1 is DNP in the fixture: {off}");

        let on = body(
            &call(
                "edit_schematic_component",
                &path,
                json!({ "reference": "C1", flag: true }),
            )
            .await,
        );
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            SHEET,
            "{flag}: back to KiCad's bytes"
        );
        assert_eq!(on[flag], true, "{on}");
    }
}

/// Both flags in one call reach every placed unit of U1.
#[tokio::test]
async fn every_placed_unit_takes_in_bom_and_on_board() {
    let (_dir, path) = fixture(SHEET);
    let result = call(
        "edit_schematic_component",
        &path,
        json!({ "reference": "U1", "in_bom": false, "on_board": false }),
    )
    .await;
    let changes = text(&result);
    let result = body(&result);
    let expected = kicad_with_flag(
        &kicad_with_flag(SHEET, "U1", "in_bom", "yes", "no"),
        "U1",
        "on_board",
        "yes",
        "no",
    );
    assert_eq!(std::fs::read_to_string(&path).unwrap(), expected);
    for flag in ["in_bom", "on_board"] {
        assert_eq!(native_flag(&expected, "U1", flag).len(), 3);
        assert!(
            changes.contains(&format!("{flag} → no (3 unit(s))")),
            "{changes}"
        );
    }
    let units = result["units"].as_array().unwrap();
    assert_eq!(units.len(), 3, "{result}");
    assert!(
        units
            .iter()
            .all(|unit| unit["in_bom"] == false && unit["on_board"] == false),
        "{result}"
    );
}

/// A call that names neither flag keeps what the file holds, here a C1
/// excluded from both, while it edits the value.
#[tokio::test]
async fn an_edit_without_the_flags_keeps_the_files_values() {
    let excluded = kicad_with_flag(
        &kicad_with_flag(SHEET, "C1", "in_bom", "yes", "no"),
        "C1",
        "on_board",
        "yes",
        "no",
    );
    let (_dir, path) = fixture(&excluded);
    let result = body(
        &call(
            "edit_schematic_component",
            &path,
            json!({ "reference": "C1", "value": "220n" }),
        )
        .await,
    );
    let saved = std::fs::read_to_string(&path).unwrap();
    for flag in ["in_bom", "on_board"] {
        assert_eq!(
            native_flag(&saved, "C1", flag),
            vec![(1, Some("no".to_owned()))]
        );
        assert_eq!(result[flag], false, "{result}");
    }
    assert_eq!(native_dnp(&saved, "C1"), native_dnp(SHEET, "C1"));
}

/// Setting the value a file already holds leaves KiCad's bytes.
#[tokio::test]
async fn setting_the_stored_value_leaves_kicads_bytes() {
    let (_dir, path) = fixture(SHEET);
    let result = body(
        &call(
            "edit_schematic_component",
            &path,
            json!({ "reference": "U1", "in_bom": true, "on_board": true }),
        )
        .await,
    );
    assert_eq!(std::fs::read_to_string(&path).unwrap(), SHEET);
    assert_eq!(result["in_bom"], true, "{result}");
    assert_eq!(result["on_board"], true, "{result}");
}

/// A value that is not a boolean, a reference the sheet lacks, `unit`
/// beside a flag alone, and a sheet KiCad holds are each refused, and
/// nothing is written. The malformed value is refused by the served schema
/// and, for direct callers, by the handler.
#[tokio::test]
async fn in_bom_and_on_board_refusals_write_nothing() {
    for flag in ["in_bom", "on_board"] {
        let (_dir, path) = fixture(SHEET);
        let malformed = json!({ "reference": "R1", flag: "no", "value": "1k" });
        for result in [
            call("edit_schematic_component", &path, malformed.clone()).await,
            call_direct("edit_schematic_component", &path, malformed).await,
        ] {
            assert_eq!(
                extract_error_kind(&result).as_deref(),
                Some("invalid_argument"),
                "{flag}: {}",
                text(&result)
            );
        }
        for (args, kind) in [
            (json!({ "reference": "R99", flag: false }), None),
            (
                json!({ "reference": "U1", "unit": 2, flag: false }),
                Some("invalid_argument"),
            ),
        ] {
            let result = call("edit_schematic_component", &path, args).await;
            assert!(result.is_error, "{flag}: {}", text(&result));
            if let Some(kind) = kind {
                assert_eq!(extract_error_kind(&result).as_deref(), Some(kind));
            }
        }
        let lock = path.with_file_name(format!(
            "~{}.lck",
            path.file_name().unwrap().to_string_lossy()
        ));
        std::fs::write(&lock, "locked").unwrap();
        let result = call(
            "edit_schematic_component",
            &path,
            json!({ "reference": "R1", flag: false }),
        )
        .await;
        assert_eq!(
            extract_error_kind(&result).as_deref(),
            Some("conflict"),
            "{flag}: {}",
            text(&result)
        );
        assert_eq!(std::fs::read_to_string(&path).unwrap(), SHEET, "{flag}");
    }
}

/// A placed symbol with neither token gets each where KiCad 10 writes it,
/// `(in_bom …)` after `(exclude_from_sim …)` and `(on_board …)` after it:
/// U1 lands on KiCad's own bytes.
#[tokio::test]
async fn a_symbol_without_the_flags_gets_them_where_kicad_writes_them() {
    let strip = |sheet: &str| {
        sheet
            .replace("\n\t\t(in_bom yes)", "")
            .replace("\n\t\t(on_board yes)", "")
    };
    let mut expected = kicad_with_flag(
        &kicad_with_flag(SHEET, "U1", "in_bom", "yes", "no"),
        "U1",
        "on_board",
        "yes",
        "no",
    );
    for (start, end) in kicad_blocks(&expected, "R1")
        .into_iter()
        .chain(kicad_blocks(&expected, "C1"))
        .collect::<BTreeSet<_>>()
        .into_iter()
        .rev()
    {
        let block = strip(&expected[start..end]);
        expected.replace_range(start..end, &block);
    }
    let tokenless = strip(SHEET);
    assert!(native_flag(&tokenless, "U1", "in_bom")
        .iter()
        .all(|(_, value)| value.is_none()));
    let (_dir, path) = fixture(&tokenless);
    body(
        &call(
            "edit_schematic_component",
            &path,
            json!({ "reference": "U1", "in_bom": false, "on_board": false }),
        )
        .await,
    );
    assert_eq!(std::fs::read_to_string(&path).unwrap(), expected);
}

/// KiCad's BOM export leaves out a part whose `in_bom` was set to false,
/// and lists it again once it is set back. Needs a real kicad-cli, named
/// by `KICAD_CLI`.
#[tokio::test]
#[ignore = "needs a real kicad-cli, named by KICAD_CLI"]
async fn kicads_bom_export_follows_in_bom() {
    let cli = std::env::var("KICAD_CLI").expect("KICAD_CLI names kicad-cli");
    let (dir, path) = fixture(SHEET);
    let bom = dir.path().join("bom.csv");
    let listed = |content: &str| {
        content
            .lines()
            .skip(1)
            .any(|row| row.split(',').next() == Some("\"R1\""))
    };
    let options = crate::tools::cli::BomOptions {
        fields: Some("Reference,Value"),
        ..Default::default()
    };
    crate::tools::cli::export_bom(&cli, &path, &bom, &options)
        .await
        .unwrap();
    assert!(listed(&std::fs::read_to_string(&bom).unwrap()), "R1 listed");

    body(
        &call(
            "edit_schematic_component",
            &path,
            json!({ "reference": "R1", "in_bom": false }),
        )
        .await,
    );
    crate::tools::cli::export_bom(&cli, &path, &bom, &options)
        .await
        .unwrap();
    let excluded = std::fs::read_to_string(&bom).unwrap();
    assert!(!listed(&excluded), "R1 left out:\n{excluded}");
    assert!(
        excluded.lines().any(|row| row.starts_with("\"C1\"")),
        "C1 still listed:\n{excluded}"
    );

    body(
        &call(
            "edit_schematic_component",
            &path,
            json!({ "reference": "R1", "in_bom": true }),
        )
        .await,
    );
    crate::tools::cli::export_bom(&cli, &path, &bom, &options)
        .await
        .unwrap();
    assert!(
        listed(&std::fs::read_to_string(&bom).unwrap()),
        "R1 listed again"
    );
}

// A placed symbol takes its library symbol's exclusions, as eeschema's
// placement does (#124).

/// Four symbols, each excluded from one thing, so every flag is read both
/// ways and no two flags read alike across them. See its README.
const EXCLUSIONS: &str =
    include_str!("../../../tests/fixtures/placement_exclusions_kicad10.kicad_sym");
const EXCLUSION_FLAGS: [&str; 4] = ["exclude_from_sim", "in_bom", "on_board", "in_pos_files"];
const EXCLUDED_SYMBOLS: [&str; 4] = [
    "EXCLUDED_FROM_SIM",
    "EXCLUDED_FROM_BOM",
    "EXCLUDED_FROM_BOARD",
    "EXCLUDED_FROM_POS_FILES",
];

/// `symbol`'s own value of each of `EXCLUSION_FLAGS` in `library`.
fn library_flags(library: &str, symbol: &str) -> Vec<String> {
    let tree = parse_sexp(library).unwrap();
    let symbol = tree
        .children()
        .unwrap()
        .iter()
        .find(|node| {
            node.head() == Some("symbol") && node.get(1).and_then(SexpNode::as_str) == Some(symbol)
        })
        .unwrap();
    EXCLUSION_FLAGS
        .iter()
        .map(|flag| symbol.find_str(flag).unwrap().to_owned())
        .collect()
}

/// The fixture sheet, under the project name its instances record so a
/// placement takes it, beside a project table that names EXCLUSIONS
/// `Exclusions`, and `power` too, since `add_power_symbol` places
/// whatever `power:<net>` resolves to.
fn exclusions_fixture() -> (tempfile::TempDir, std::path::PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("dnp_kicad10.kicad_sch");
    std::fs::write(&path, SHEET).unwrap();
    std::fs::write(dir.path().join("exclusions.kicad_sym"), EXCLUSIONS).unwrap();
    let lib = |name: &str| {
        format!("  (lib (name \"{name}\") (type \"KiCad\") (uri \"${{KIPRJMOD}}/exclusions.kicad_sym\") (options \"\") (descr \"\"))\n")
    };
    std::fs::write(
        dir.path().join("sym-lib-table"),
        format!(
            "(sym_lib_table\n  (version 7)\n{}{})\n",
            lib("Exclusions"),
            lib("power")
        ),
    )
    .unwrap();
    (dir, path)
}

/// Every placer writes the library's four flags on the symbol it places,
/// and its readback holds `in_bom` and `on_board`, so the call succeeds.
#[tokio::test]
async fn a_placed_symbol_takes_its_library_symbols_exclusions() {
    for tool in [
        "add_schematic_component",
        "batch_place_components",
        "add_power_symbol",
    ] {
        let (_dir, path) = exclusions_fixture();
        let placed = |index: usize| {
            let mut args = json!({ "x": 101.6 + 25.4 * index as f64, "y": 101.6 });
            if tool == "add_power_symbol" {
                args["power_net"] = json!(EXCLUDED_SYMBOLS[index]);
            } else {
                args["lib_id"] = json!(format!("Exclusions:{}", EXCLUDED_SYMBOLS[index]));
                args["reference"] = json!(format!("H{}", 10 + index));
            }
            args
        };
        let mut references = Vec::new();
        if tool == "batch_place_components" {
            let components = (0..EXCLUDED_SYMBOLS.len()).map(placed).collect::<Vec<_>>();
            let result = body(&call(tool, &path, json!({ "components": components })).await);
            assert_eq!(result["placed_count"], 4, "{result}");
            references.extend((0..EXCLUDED_SYMBOLS.len()).map(|index| format!("H{}", 10 + index)));
        } else {
            for index in 0..EXCLUDED_SYMBOLS.len() {
                let result = body(&call(tool, &path, placed(index)).await);
                references.push(result["reference"].as_str().unwrap().to_owned());
            }
        }
        let sheet = std::fs::read_to_string(&path).unwrap();
        for (symbol, reference) in EXCLUDED_SYMBOLS.iter().zip(&references) {
            for (flag, value) in EXCLUSION_FLAGS
                .iter()
                .zip(library_flags(EXCLUSIONS, symbol))
            {
                assert_eq!(
                    native_flag(&sheet, reference, flag),
                    [(1, Some(value))],
                    "{tool}: {symbol} as {reference}, ({flag} …)"
                );
            }
        }
    }
}
