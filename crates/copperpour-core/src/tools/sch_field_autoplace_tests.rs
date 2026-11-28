use super::*;
use crate::tools::project_file_writer_tests::{served_call, text_of};
use crate::tools::sch_block_move::{snapshot_net_changes, NetComparison};
use crate::{mcp::handler::McpHandler, tools::ServerConfig};
use std::{fs, path::PathBuf};

const LANES: &str = "direct_lanes_kicad10/direct_lanes.kicad_sch";
const POWER: &str = "field_placement_kicad10/field_placement.kicad_sch";

/// `kicad-cli` (or `TEST_KICAD_CLI`) run on the sheet copy, writing beside it.
fn native(sheet: &Sheet, args: &[&str], ext: &str) -> String {
    let cli = crate::app_dirs::env_var("TEST_KICAD_CLI").unwrap_or_else(|| "kicad-cli".into());
    let path = sheet.path.with_extension(ext);
    let output = std::process::Command::new(&cli)
        .current_dir(sheet.path.parent().unwrap())
        .args(args)
        .arg("-o")
        .arg(&path)
        .arg(&sheet.path)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    fs::read_to_string(path).unwrap()
}

/// The native netlist's nets and components, which a field or lead edit must keep.
fn native_netlist(sheet: &Sheet) -> (SexpNode, SexpNode, usize) {
    let netlist = parse_sexp(&native(
        sheet,
        &["sch", "export", "netlist", "--format", "kicadsexpr"],
        "net",
    ))
    .unwrap();
    let nets = netlist.find("nets").unwrap().clone();
    let pins = nets
        .find_all("net")
        .iter()
        .map(|net| net.find_all("node").len())
        .sum();
    (nets, netlist.find("components").unwrap().clone(), pins)
}

/// Native ERC findings by rule, severity and item UUIDs. Positions are left
/// out: KiCad 10.0.6 reports them at 1/100 the scale 10.0.8 does.
fn native_erc(sheet: &Sheet) -> Vec<(String, String, Vec<String>)> {
    let report: Value = serde_json::from_str(&native(
        sheet,
        &["sch", "erc", "--severity-all", "--format", "json"],
        "json",
    ))
    .unwrap();
    let mut findings = report["sheets"]
        .as_array()
        .unwrap()
        .iter()
        .flat_map(|sheet| sheet["violations"].as_array().unwrap())
        .map(|finding| {
            let mut items = finding["items"]
                .as_array()
                .unwrap()
                .iter()
                .map(|item| item["uuid"].as_str().unwrap().to_owned())
                .collect::<Vec<_>>();
            items.sort();
            (
                finding["type"].as_str().unwrap().to_owned(),
                finding["severity"].as_str().unwrap().to_owned(),
                items,
            )
        })
        .collect::<Vec<_>>();
    findings.sort();
    findings
}

#[tokio::test]
#[ignore = "requires KiCad CLI with stock symbol and footprint tables"]
async fn direct_lanes_native_erc_and_netlist_are_unchanged() {
    let sheet = Sheet::copy(LANES).await;
    let before = native_netlist(&sheet);
    assert_eq!(before.2, 8);
    assert_eq!(native_erc(&sheet), vec![]);
    let preview = sheet.call(OPERATION, json!({})).await;
    let ids = approvals(&preview, "C1");
    assert_eq!(ids.len(), 2);
    let applied = sheet
        .call(
            OPERATION,
            json!({"revision":preview["revision"], "approvals":ids}),
        )
        .await;
    assert_eq!(applied["written"], true);
    assert_eq!(applied["placements"].as_array().unwrap().len(), 2);
    assert_eq!(native_netlist(&sheet), before);
    assert_eq!(native_erc(&sheet), vec![]);
}

/// The advice's own trial, saved: KiCad keeps every pin on its net and
/// reports the same findings, and U2 then needs no field change.
#[tokio::test]
#[ignore = "requires KiCad CLI with stock symbol and footprint tables"]
async fn following_the_power_advice_keeps_native_nets_and_clears_u2() {
    let sheet = Sheet::copy(POWER).await;
    let netlist = native_netlist(&sheet);
    assert_eq!(netlist.2, 54);
    let erc = native_erc(&sheet);
    let preview = sheet.call(OPERATION, json!({})).await;
    let suggestion = &field(&preview, "U2", "Reference")["suggestion"];
    let point = |key: &str| {
        let p = &suggestion[key];
        (p["x_mm"].as_f64().unwrap(), p["y_mm"].as_f64().unwrap())
    };
    let trial = power_lead_trial(
        &sheet.source(),
        suggestion["power_symbol_uuid"].as_str().unwrap(),
        suggestion["wire_uuid"].as_str().unwrap(),
        point("pin"),
        point("destination"),
    )
    .unwrap();
    fs::write(&sheet.path, &trial).unwrap();
    assert_eq!(native_netlist(&sheet), netlist);
    assert_eq!(native_erc(&sheet), erc);
    let after = sheet.call(OPERATION, json!({})).await;
    for name in ["Reference", "Value"] {
        let u2 = field(&after, "U2", name);
        assert_eq!(u2["reason"], "already_clear", "{u2}");
        assert!(u2["suggestion"].is_null());
    }
}

struct Sheet {
    _dir: tempfile::TempDir,
    path: PathBuf,
    handler: McpHandler,
}

impl Sheet {
    async fn copy(name: &str) -> Self {
        let fixture = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures")
            .join(name);
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(fixture.file_name().unwrap());
        fs::copy(&fixture, &path).unwrap();
        fs::copy(
            fixture.with_extension("kicad_pro"),
            path.with_extension("kicad_pro"),
        )
        .unwrap();
        let handler = McpHandler::new(ServerConfig {
            eager_toolsets: true,
            ..Default::default()
        })
        .await
        .unwrap();
        Self {
            _dir: dir,
            path,
            handler,
        }
    }

    fn source(&self) -> String {
        fs::read_to_string(&self.path).unwrap()
    }

    async fn invoke(&self, tool: &str, mut args: Value) -> (bool, Value) {
        args["schematic"] = json!(self.path);
        let result = served_call(&self.handler, tool, args).await;
        (
            result.is_error,
            serde_json::from_str(text_of(&result)).unwrap(),
        )
    }

    async fn call(&self, tool: &str, args: Value) -> Value {
        let (error, body) = self.invoke(tool, args).await;
        assert!(!error, "{body}");
        body
    }
}

fn symbol<'a>(preview: &'a Value, reference: &str) -> &'a Value {
    preview["symbols"]
        .as_array()
        .unwrap()
        .iter()
        .find(|p| p["reference"] == reference)
        .unwrap()
}

fn approvals(preview: &Value, reference: &str) -> Vec<Value> {
    symbol(preview, reference)["fields"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|f| f.get("approval").cloned())
        .collect()
}

fn field<'a>(preview: &'a Value, reference: &str, name: &str) -> &'a Value {
    symbol(preview, reference)["fields"]
        .as_array()
        .unwrap()
        .iter()
        .find(|field| field["name"] == name)
        .unwrap()
}

fn use_legacy_angle(sheet: &Sheet, reference: &str, fractional: f64) -> String {
    // A parser control on a disposable copy: KiCad's next save would
    // normalize the fractional angle this test needs Copperpour to read.
    let source = sheet.source();
    let tree = parse_sexp(&source).unwrap();
    let mut symbols = tree
        .find_all("symbol")
        .into_iter()
        .filter_map(symbol_instance)
        .filter(|symbol| symbol.reference == reference);
    let symbol = symbols.next().unwrap();
    assert!(symbols.next().is_none());
    assert_eq!(symbol.rotation, 0.0);
    let (start, end) = item_range(&source, "symbol", symbol.uuid.as_deref().unwrap()).unwrap();
    let placements = direct_children_with_tag(&source[start..end], "symbol", "at");
    let [(at, stop)] = placements.as_slice() else {
        panic!("expected one placement for {reference}");
    };
    let legacy = apply_edits(
        source,
        vec![SexpEdit::replace(
            start + *at,
            start + *stop,
            format!("(at {} {} {fractional})", symbol.x, symbol.y),
        )],
    );
    let changed = parse_sexp(&legacy).unwrap();
    let observed = changed
        .find_all("symbol")
        .into_iter()
        .filter_map(symbol_instance)
        .find(|s| s.uuid == symbol.uuid)
        .unwrap();
    assert_eq!(
        (observed.x, observed.y, observed.rotation),
        (symbol.x, symbol.y, fractional)
    );
    fs::write(&sheet.path, &legacy).unwrap();
    legacy
}

#[tokio::test]
async fn a_saved_fractional_obstacle_withholds_field_offers() {
    let sheet = Sheet::copy(LANES).await;
    let clean = sheet.call(OPERATION, json!({})).await;
    let ids = approvals(&clean, "C1");
    assert_eq!(ids.len(), 2);
    let legacy = use_legacy_angle(&sheet, "J1", 0.5);
    let preview = sheet.call(OPERATION, json!({})).await;
    for reference in ["C1", "J1"] {
        let report = field(&preview, reference, "Reference");
        assert_eq!(report["reason"], "cannot_measure", "{report}");
        assert!(report["proposed"].is_null() && report["approval"].is_null());
    }
    assert_eq!(sheet.source(), legacy);
    let (error, refusal) = sheet
        .invoke(
            OPERATION,
            json!({"revision":preview["revision"], "approvals":ids}),
        )
        .await;
    assert!(error, "{refusal}");
    assert_eq!(refusal["error"]["kind"], "invalid_argument");
    assert_eq!(sheet.source(), legacy);
}

#[tokio::test]
async fn fractional_saved_geometry_withholds_power_stub_advice() {
    let sheet = Sheet::copy(POWER).await;
    let clean = sheet.call(OPERATION, json!({})).await;
    assert_eq!(
        field(&clean, "U2", "Reference")["reason"],
        "move_power_to_pin"
    );
    let legacy = use_legacy_angle(&sheet, "R3", 0.5);
    let preview = sheet.call(OPERATION, json!({})).await;
    let field = field(&preview, "U2", "Reference");
    assert_eq!(field["reason"], "cannot_measure", "{field}");
    assert!(field["suggestion"].is_null());
    assert_eq!(sheet.source(), legacy);
}

#[tokio::test]
async fn consenting_to_one_field_leaves_the_other_where_it_was() {
    let sheet = Sheet::copy(LANES).await;
    let preview = sheet.call(OPERATION, json!({})).await;
    let reference = field(&preview, "C1", "Reference");
    let value = field(&preview, "C1", "Value");
    assert!(value["approval"].is_string());
    let apply = sheet
        .call(
            OPERATION,
            json!({
                "revision": preview["revision"], "approvals": [reference["approval"]]
            }),
        )
        .await;
    assert_eq!(apply["placements"].as_array().unwrap().len(), 1);
    assert_eq!(apply["placements"][0]["position"], reference["proposed"]);
    assert_eq!(apply["withdrawn_offers"], json!([]));
    let after = sheet.call(OPERATION, json!({})).await;
    assert_eq!(field(&after, "C1", "Value")["old"], value["old"]);
}

#[tokio::test]
async fn a_marked_nondefault_field_requires_its_own_approval() {
    let sheet = Sheet::copy(LANES).await;
    let source = c1_with_nondefault_justification(sheet.source(), "(fields_autoplaced yes)");
    fs::write(&sheet.path, &source).unwrap();
    let preview = sheet.call(OPERATION, json!({})).await;
    let reference = field(&preview, "C1", "Reference");
    assert_eq!(reference["evidence"], "fields_autoplaced");
    assert!(reference["approval"].is_string(), "{reference}");
    assert_eq!(sheet.source(), source);
    let no_approval = sheet
        .call(
            OPERATION,
            json!({
                "revision": preview["revision"], "approvals": []
            }),
        )
        .await;
    assert_eq!(no_approval["written"], false);
    assert_eq!(sheet.source(), source);
    let applied = sheet
        .call(
            OPERATION,
            json!({
                "revision": preview["revision"], "approvals": [reference["approval"]]
            }),
        )
        .await;
    assert_eq!(applied["placements"][0]["position"], reference["proposed"]);
    assert!(!sheet.source().contains("(fields_autoplaced yes)"));
}

fn c1_with_nondefault_justification(source: String, marker: &str) -> String {
    let offset = source.find(r#"(property "Reference" "C1""#).unwrap();
    let justify = offset + source[offset..].find("(justify left)").unwrap();
    apply_edits(
        source,
        vec![
            SexpEdit::replace(
                justify,
                justify + "(justify left)".len(),
                "(justify left bottom)",
            ),
            SexpEdit::insert(offset, format!("{marker}\n\t\t")),
        ],
    )
}

async fn check_nondefault_justification_is_protected(marker: &str) {
    let sheet = Sheet::copy(LANES).await;
    let source = c1_with_nondefault_justification(sheet.source(), marker);
    fs::write(&sheet.path, &source).unwrap();
    let preview = sheet.call(OPERATION, json!({})).await;
    let reference = field(&preview, "C1", "Reference");
    assert_eq!(reference["reason"], "positioned", "{reference}");
    assert!(reference["approval"].is_null());
    assert_eq!(sheet.source(), source);
}

#[tokio::test]
async fn manual_justification_without_a_marker_is_protected() {
    check_nondefault_justification_is_protected("").await;
}

#[tokio::test]
async fn an_explicit_no_is_not_evidence_of_autoplacement() {
    check_nondefault_justification_is_protected("(fields_autoplaced no)").await;
}

#[tokio::test]
async fn a_do_not_autoplace_field_stays() {
    let sheet = Sheet::copy(LANES).await;
    let content = sheet.source();
    let offset = content.find(r#"(property "Value" "100nF""#).unwrap();
    let flag = offset + content[offset..].find("(do_not_autoplace no)").unwrap();
    let source = apply_edits(
        content,
        vec![
            SexpEdit::replace(
                flag,
                flag + "(do_not_autoplace no)".len(),
                "(do_not_autoplace yes)",
            ),
            SexpEdit::insert(offset, "(fields_autoplaced)\n\t\t"),
        ],
    );
    fs::write(&sheet.path, &source).unwrap();
    let preview = sheet.call(OPERATION, json!({})).await;
    let value = field(&preview, "C1", "Value");
    assert_eq!(value["reason"], "do_not_autoplace");
    assert!(value["approval"].is_null());
    assert_eq!(sheet.source(), source);
}

#[tokio::test]
async fn protected_text_blocks_another_fields_proposed_position() {
    let sheet = Sheet::copy(LANES).await;
    let obstacle = sheet
        .call(
            "add_schematic_component",
            json!({
                "lib_id":"Device:C", "reference":"C90", "x":260.35, "y":180.34
            }),
        )
        .await;
    let uuid = obstacle["uuid"].as_str().unwrap();
    let without_obstacle = sheet.call(OPERATION, json!({})).await;
    let target_uuid = symbol(&without_obstacle, "C1")["symbol_uuid"]
        .as_str()
        .unwrap();
    let positive = field(&without_obstacle, "C1", "Value")["proposed"].clone();
    let x = positive["x_mm"].as_f64().unwrap();
    let y = positive["y_mm"].as_f64().unwrap();
    let proposed = Position {
        x_mm: x,
        y_mm: y,
        rotation_degrees: positive["rotation_degrees"].as_f64().unwrap(),
        justify: Vec::new(),
    };
    let moved = |source: &str| {
        parse_sexp(&move_field(source, target_uuid, "Value", &proposed).unwrap()).unwrap()
    };
    assert!(clearance(&moved(&sheet.source()), target_uuid, &["Value"]).0);
    let source = replace_test_field(
        sheet.source(),
        uuid,
        "Reference",
        &format!(
            r#"(property "Reference" "C90" (at {x} {y} 0) (do_not_autoplace yes)
           (effects (font (size 1.27 1.27))))"#
        ),
    );
    assert!(!clearance(&moved(&source), target_uuid, &["Value"]).0);
    fs::write(&sheet.path, &source).unwrap();
    let blocked = sheet.call(OPERATION, json!({})).await;
    assert_eq!(
        field(&blocked, "C90", "Reference")["reason"],
        "do_not_autoplace"
    );
    assert_ne!(field(&blocked, "C1", "Value")["proposed"], positive);
    assert_eq!(sheet.source(), source);
}

#[tokio::test]
async fn straight_signal_lanes_keep_their_wires_and_move_only_fields() {
    let sheet = Sheet::copy(LANES).await;
    let before = sheet.source();
    let tree = parse_sexp(&before).unwrap();
    let before_wires = tree.find_all("wire");
    assert_eq!(before_wires.len(), 4);
    let preview = sheet.call(OPERATION, json!({})).await;
    assert_eq!(sheet.source(), before);
    let part = symbol(&preview, "C1");
    let ids = approvals(&preview, "C1");
    assert_eq!(ids.len(), 2, "{part}");
    let baseline = sheet.call("check_schematic_overlaps", json!({})).await;
    let crossings = |result: &Value| {
        result["overlaps"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|o| o["type"] == "wire_field_overlap" && o["b"]["reference"] == "C1")
            .count()
    };
    assert_eq!(crossings(&baseline), 3);
    let contacts = sheet
        .call("find_pin_contacts", json!({"min_wire_mm": 0}))
        .await;
    assert_eq!(contacts["contact_count"], 0);
    let applied = sheet
        .call(
            OPERATION,
            json!({"revision":preview["revision"], "approvals":ids}),
        )
        .await;
    assert_eq!(applied["written"], true);
    let saved = sheet.source();
    let after_tree = parse_sexp(&saved).unwrap();
    assert_eq!(after_tree.find_all("wire"), before_wires);
    let after = sheet.call("check_schematic_overlaps", json!({})).await;
    assert_eq!(crossings(&after), 0);
    let contacts = sheet
        .call("find_pin_contacts", json!({"min_wire_mm": 0}))
        .await;
    assert_eq!(contacts["contact_count"], 0);
    let uuid = part["symbol_uuid"].as_str().unwrap();
    let observed = after_tree
        .find_all("symbol")
        .into_iter()
        .find(|n| n.find_str("uuid") == Some(uuid))
        .unwrap();
    for p in applied["placements"].as_array().unwrap() {
        let name = p["field"].as_str().unwrap();
        let saved = Position::read(property(observed, name).unwrap()).unwrap();
        assert_eq!(p["position"], json!(saved));
        assert_eq!(saved.rotation_degrees, 90.0);
        assert!(saved.justify.is_empty());
        assert!(saved.y_mm < 76.2);
    }
    let review = sheet.call(OPERATION, json!({})).await;
    assert!(approvals(&review, "C1").is_empty());
    let replay = sheet
        .call(
            OPERATION,
            json!({"revision":review["revision"], "approvals":[]}),
        )
        .await;
    assert_eq!(replay["written"], false);
    assert_eq!(sheet.source(), saved);
}

#[tokio::test]
async fn stale_revision_refuses_unchanged_proposal_without_writing() {
    let sheet = Sheet::copy(LANES).await;
    let preview = sheet.call(OPERATION, json!({})).await;
    let ids = approvals(&preview, "C1");
    assert!(!ids.is_empty());
    let new_bytes = sheet.source() + "\n";
    fs::write(&sheet.path, &new_bytes).unwrap();
    let intervening = sheet.call(OPERATION, json!({})).await;
    let (error, body) = sheet
        .invoke(
            OPERATION,
            json!({"revision":preview["revision"], "approvals":approvals(&intervening, "C1")}),
        )
        .await;
    assert!(error, "{body}");
    assert_eq!(body["error"]["kind"], "conflict");
    assert_eq!(sheet.source(), new_bytes);
}

#[tokio::test]
async fn a_current_revision_does_not_reauthorize_approval_from_an_older_preview() {
    let sheet = Sheet::copy(LANES).await;
    let preview = sheet.call(OPERATION, json!({})).await;
    let ids = approvals(&preview, "C1");
    assert_eq!(ids.len(), 2);
    let source = sheet.source() + "\n";
    fs::write(&sheet.path, &source).unwrap();
    let intervening = sheet.call(OPERATION, json!({})).await;
    assert_eq!(
        field(&preview, "C1", "Value")["proposed"],
        field(&intervening, "C1", "Value")["proposed"]
    );
    let (error, body) = sheet
        .invoke(
            OPERATION,
            json!({
                "revision":intervening["revision"], "approvals":ids
            }),
        )
        .await;
    assert!(error, "{body}");
    assert_eq!(body["error"]["kind"], "invalid_argument");
    assert_eq!(sheet.source(), source);
}

#[tokio::test]
async fn unrecognized_approval_refuses_even_with_the_current_revision() {
    let sheet = Sheet::copy(LANES).await;
    let before = sheet.source();
    let preview = sheet.call(OPERATION, json!({})).await;
    let mut ids = approvals(&preview, "C1");
    assert_eq!(ids.len(), 2);
    ids[1] = json!("unapproved-placement");
    let (error, body) = sheet
        .invoke(
            OPERATION,
            json!({"revision":preview["revision"], "approvals":ids}),
        )
        .await;
    assert!(error, "{body}");
    assert_eq!(body["error"]["kind"], "invalid_argument");
    assert_eq!(sheet.source(), before);
}

#[tokio::test]
async fn readback_mismatch_after_replacement_is_not_a_refusal() {
    let sheet = Sheet::copy(LANES).await;
    let preview = sheet.call(OPERATION, json!({})).await;
    let ids = approvals(&preview, "C1");
    assert_eq!(ids.len(), 2);
    let concurrent = sheet.source() + "\n";
    copperpour_sexp::writer::test_hooks::save_after_replace(&sheet.path, concurrent.clone());
    let (error, body) = sheet
        .invoke(
            OPERATION,
            json!({
                "revision": preview["revision"], "approvals": ids
            }),
        )
        .await;
    assert!(error, "{body}");
    assert_eq!(body["error"]["kind"], "mutation_outcome_uncertain");
    assert_eq!(sheet.source(), concurrent);
}

#[tokio::test]
async fn a_dedicated_power_lead_gets_a_suggestion_not_a_field_write() {
    let sheet = Sheet::copy(POWER).await;
    let before = sheet.source();
    let preview = sheet.call(OPERATION, json!({})).await;
    let target = field(&preview, "U2", "Reference");
    assert_eq!(
        target["suggestion"]["reason"], "move_power_to_pin",
        "{target}"
    );
    assert_eq!(target["suggestion"]["power_reference"], "#PWR03");
    assert_eq!(target["suggestion"]["stub_length_mm"], 2.54);
    assert!(approvals(&preview, "U2").is_empty());
    assert_eq!(sheet.source(), before);

    let suggestion = &target["suggestion"];
    let point = |key: &str| {
        let p = &suggestion[key];
        (p["x_mm"].as_f64().unwrap(), p["y_mm"].as_f64().unwrap())
    };
    let id = suggestion["wire_uuid"].as_str().unwrap();
    let trial = parse_sexp(
        &power_lead_trial(
            &before,
            suggestion["power_symbol_uuid"].as_str().unwrap(),
            id,
            point("pin"),
            point("destination"),
        )
        .unwrap(),
    )
    .unwrap();
    let before = parse_sexp(&before).unwrap();
    let length = |tree: &SexpNode| {
        let lead = extract_wires(tree)
            .into_iter()
            .find(|w| w.uuid.as_deref() == Some(id))
            .unwrap();
        round_mm((lead.x1 - lead.x2).hypot(lead.y1 - lead.y2))
    };
    assert_eq!(length(&before), 25.4);
    assert_eq!(length(&trial), 2.54);
    assert!(
        snapshot_net_changes(&before, &trial, &BTreeSet::new(), NetComparison::Layout).is_empty()
    );
    let (findings, _, _) = overlap_findings(&trial, &extract_wires(&trial), OVERLAP_TOLERANCE);
    let u2 = symbol(&preview, "U2")["symbol_uuid"].as_str().unwrap();
    assert!(field_crossings(&findings, u2, &["Reference", "Value"]).is_empty());
}

#[tokio::test]
async fn moving_power_text_onto_a_passive_pin_blocks_advice() {
    let sheet = Sheet::copy(POWER).await;
    let advice = u2_power_suggestion(&sheet).await;
    assert_eq!(advice["reason"], "move_power_to_pin");
    sheet
        .call(
            "add_schematic_component",
            json!({"lib_id":"Device:R", "reference":"R90", "x":81.28, "y":118.11, "rotation":90}),
        )
        .await;
    let source = sheet.source();
    let before = parse_sexp(&source).unwrap();
    let point = |key: &str| {
        let p = &advice[key];
        (p["x_mm"].as_f64().unwrap(), p["y_mm"].as_f64().unwrap())
    };
    let power = advice["power_symbol_uuid"].as_str().unwrap();
    let after = parse_sexp(
        &power_lead_trial(
            &source,
            power,
            advice["wire_uuid"].as_str().unwrap(),
            point("pin"),
            point("destination"),
        )
        .unwrap(),
    )
    .unwrap();
    assert_eq!(layout_verdict(&before, &after), "improved");
    let passive = placed_symbols(&before, true)
        .into_iter()
        .find(|part| part.instance.reference == "R90")
        .unwrap()
        .instance
        .uuid
        .unwrap();
    assert!(field_collisions(&after)
        .difference(&field_collisions(&before))
        .any(|contact| contact.symbol == power
            && contact.field == "Value"
            && matches!(&contact.obstacle, FieldObstacle::Pin(uuid, _) if *uuid == passive)));
    assert!(u2_power_suggestion(&sheet).await.is_null());
    assert_eq!(sheet.source(), source);
}

#[tokio::test]
async fn a_drawn_contact_without_a_junction_disqualifies_the_power_move() {
    let sheet = Sheet::copy(POWER).await;
    let before = sheet.source();
    let offset = before.rfind(')').unwrap();
    let source = apply_edits(
        before,
        vec![SexpEdit::insert(
            offset,
            r#"(wire (pts (xy 52.07 124.46) (xy 52.07 127)) (stroke (width 0) (type default))
            (uuid "47a8f390-ffa3-489a-809d-d16a303e64af"))"#,
        )],
    );
    fs::write(&sheet.path, &source).unwrap();
    let preview = sheet.call(OPERATION, json!({})).await;
    assert!(field(&preview, "U2", "Reference")["suggestion"].is_null());
    assert_eq!(sheet.source(), source);
}

#[tokio::test]
async fn a_joined_branch_off_the_power_lead_disqualifies_the_power_move() {
    let sheet = Sheet::copy(POWER).await;
    let preview = sheet.call(OPERATION, json!({})).await;
    let lead = field(&preview, "U2", "Reference")["suggestion"]["wire_uuid"]
        .as_str()
        .unwrap();
    let source = sheet.source();
    let (start, end) = item_range(&source, "wire", lead).unwrap();
    let half = "c9419d7d-506d-42c7-8bfa-f2607572fcf2";
    let branch = "42eb3a64-5bdf-400e-9c37-30d91d702248";
    // Split the original lead and place a junction well away from U2's text.
    let source = apply_edits(
        source,
        vec![SexpEdit::replace(
            start,
            end,
            format!(
                r#"
        (wire (pts (xy 50.8 124.46) (xy 52.07 124.46)) (stroke (width 0) (type default)) (uuid "{half}"))
        (wire (pts (xy 52.07 124.46) (xy 76.2 124.46)) (stroke (width 0) (type default)) (uuid "{lead}"))
        (wire (pts (xy 52.07 124.46) (xy 52.07 127)) (stroke (width 0) (type default)) (uuid "{branch}"))
        (junction (at 52.07 124.46) (diameter 0) (color 0 0 0 0)
            (uuid "2cf76fff-013b-468f-853c-2ead82511e71"))
    "#
            ),
        )],
    );
    let tree = parse_sexp(&source).unwrap();
    let wires = extract_wires(&tree);
    let labels = extract_labels(&tree);
    let mut graph = super::super::sch_connectivity::drawn_net_graph_for(&tree, &wires, &labels);
    let root = graph.root_at(52.07, 127.0);
    assert_eq!(root, graph.root_at(50.8, 124.46));
    assert_eq!(root, graph.root_at(76.2, 124.46));

    fs::write(&sheet.path, &source).unwrap();
    let preview = sheet.call(OPERATION, json!({})).await;
    assert!(field(&preview, "U2", "Reference")["suggestion"].is_null());
    assert_eq!(sheet.source(), source);
}

#[tokio::test]
async fn netclass_flag_on_the_removed_lead_disqualifies_the_suggestion() {
    let sheet = Sheet::copy(POWER).await;
    let before = sheet.source();
    let offset = before.rfind(')').unwrap();
    let source = apply_edits(
        before,
        vec![SexpEdit::insert(
            offset,
            r#"(netclass_flag "" (at 52.07 124.46 0) (shape dot) (length 2.54)
                (uuid "47a8f390-ffa3-489a-809d-d16a303e64af"))"#,
        )],
    );
    fs::write(&sheet.path, &source).unwrap();
    let preview = sheet.call(OPERATION, json!({})).await;
    assert!(field(&preview, "U2", "Reference")["suggestion"].is_null());
    assert_eq!(sheet.source(), source);
}

#[tokio::test]
async fn attachment_on_the_proposed_power_stub_disqualifies_the_suggestion() {
    let sheet = Sheet::copy(POWER).await;
    let expected = sheet.call(OPERATION, json!({})).await;
    let pin = &field(&expected, "U2", "Reference")["suggestion"]["pin"];
    let x = pin["x_mm"].as_f64().unwrap();
    let y = pin["y_mm"].as_f64().unwrap() - 1.27;
    let before = sheet.source();
    let offset = before.rfind(')').unwrap();
    let source = apply_edits(
        before,
        vec![SexpEdit::insert(
            offset,
            format!(r#"(no_connect (at {x} {y}) (uuid "47a8f390-ffa3-489a-809d-d16a303e64af"))"#),
        )],
    );
    fs::write(&sheet.path, &source).unwrap();
    let preview = sheet.call(OPERATION, json!({})).await;
    assert!(field(&preview, "U2", "Reference")["suggestion"].is_null());
    assert_eq!(sheet.source(), source);
}

#[tokio::test]
async fn an_improved_power_trial_cannot_lay_a_stub_over_another_wire() {
    let sheet = Sheet::copy(POWER).await;
    let preview = sheet.call(OPERATION, json!({})).await;
    let suggestion = &field(&preview, "U2", "Reference")["suggestion"];
    let point = |key: &str| {
        let p = &suggestion[key];
        (p["x_mm"].as_f64().unwrap(), p["y_mm"].as_f64().unwrap())
    };
    let (pin, destination) = (point("pin"), point("destination"));
    let power = suggestion["power_symbol_uuid"].as_str().unwrap();
    let lead = suggestion["wire_uuid"].as_str().unwrap();
    let (x, y) = pin;
    assert_eq!(x, destination.0);
    let sign = (destination.1 - y).signum();
    let source = sheet.source();
    let end = source.rfind(')').unwrap();
    // No anchor lies at either end of the stub or on the old lead.
    let source = apply_edits(
        source,
        vec![SexpEdit::insert(
            end,
            format!(
                r#"(wire (pts (xy {x} {}) (xy {x} {})) (stroke (width 0) (type default))
                  (uuid "3135f8e9-44fb-439e-b254-374a554c2aef"))"#,
                y + sign * 0.25,
                y + sign * 0.75
            ),
        )],
    );
    let trial = power_lead_trial(&source, power, lead, pin, destination).unwrap();
    let before = parse_sexp(&source).unwrap();
    let after = parse_sexp(&trial).unwrap();
    assert!(
        snapshot_net_changes(&before, &after, &BTreeSet::new(), NetComparison::Layout).is_empty()
    );
    assert_eq!(layout_verdict(&before, &after), "improved");
    let baseline = sheet.path.with_file_name("with-segment.kicad_sch");
    fs::write(&baseline, &source).unwrap();
    fs::write(&sheet.path, trial).unwrap();
    let legacy = sheet
        .call("compare_schematic_layout", json!({"baseline": baseline}))
        .await;
    assert_eq!(legacy["delta"]["collinear_overlaps"], 1, "{legacy}");
    assert_eq!(legacy["delta"]["dangling_wire_end"], 0, "{legacy}");
    assert_eq!(legacy["verdict"], "improved", "{legacy}");

    fs::write(&sheet.path, &source).unwrap();
    let preview = sheet.call(OPERATION, json!({})).await;
    assert!(field(&preview, "U2", "Reference")["suggestion"].is_null());
    assert_eq!(sheet.source(), source);
}

fn insert_power_obstacle(sheet: &Sheet, text: &str) -> String {
    let source = sheet.source();
    let end = source.rfind(')').unwrap();
    let source = apply_edits(source, vec![SexpEdit::insert(end, text)]);
    fs::write(&sheet.path, &source).unwrap();
    source
}

async fn u2_power_suggestion(sheet: &Sheet) -> Value {
    let preview = sheet.call(OPERATION, json!({})).await;
    field(&preview, "U2", "Reference")["suggestion"].clone()
}

fn bus_stroke(from: Point, to: Point) -> String {
    format!(
        r#"(bus (pts (xy {} {}) (xy {} {})) (stroke (width 0) (type default))
           (uuid "f4c36b45-44a4-433e-9d94-022a55970d45"))"#,
        from.0, from.1, to.0, to.1
    )
}

fn bus_tap(at: Point, size: Point) -> String {
    format!(
        r#"(bus_entry (at {} {}) (size {} {}) (stroke (width 0) (type default))
           (uuid "36af4057-ccf0-45bc-8269-fbeeea762b06"))"#,
        at.0, at.1, size.0, size.1
    )
}

fn point_in_stub(suggestion: &Value) -> Point {
    let axis = |key: &str| {
        let start = suggestion["pin"][key].as_f64().unwrap();
        let end = suggestion["destination"][key].as_f64().unwrap();
        round_mm((start + end) / 2.0)
    };
    (axis("x_mm"), axis("y_mm"))
}

async fn assert_pinless_wire_on_stub_blocks_advice(at_destination: bool) {
    let sheet = Sheet::copy(POWER).await;
    let suggestion = u2_power_suggestion(&sheet).await;
    let point = |key: &str| {
        let p = &suggestion[key];
        (p["x_mm"].as_f64().unwrap(), p["y_mm"].as_f64().unwrap())
    };
    let pin = point("pin");
    let destination = point("destination");
    let (x, y) = if at_destination {
        destination
    } else {
        point_in_stub(&suggestion)
    };
    let source = insert_power_obstacle(
        &sheet,
        &format!(
            r#"(wire (pts (xy {x} {y}) (xy {} {y})) (stroke (width 0) (type default))
                  (uuid "de0fa06b-0305-486a-90e7-91fbdcb6f338"))"#,
            x - 0.5
        ),
    );
    let trial = power_lead_trial(
        &source,
        suggestion["power_symbol_uuid"].as_str().unwrap(),
        suggestion["wire_uuid"].as_str().unwrap(),
        pin,
        destination,
    )
    .unwrap();
    let before = parse_sexp(&source).unwrap();
    let after = parse_sexp(&trial).unwrap();
    assert!(
        snapshot_net_changes(&before, &after, &BTreeSet::new(), NetComparison::Layout).is_empty()
    );
    assert!(u2_power_suggestion(&sheet).await.is_null());
    assert_eq!(sheet.source(), source);
}

#[tokio::test]
async fn a_wire_ending_mid_stub_without_a_junction_blocks_power_advice() {
    assert_pinless_wire_on_stub_blocks_advice(false).await;
}

#[tokio::test]
async fn a_wire_at_the_stubs_destination_blocks_power_advice() {
    assert_pinless_wire_on_stub_blocks_advice(true).await;
}

#[tokio::test]
async fn an_unrelated_bus_and_tap_leave_the_power_advice_unchanged() {
    let sheet = Sheet::copy(POWER).await;
    let expected = u2_power_suggestion(&sheet).await;
    assert_eq!(expected["reason"], "move_power_to_pin");
    let source = insert_power_obstacle(
        &sheet,
        &(bus_stroke((264.16, 175.26), (279.4, 175.26)) + &bus_tap((269.24, 175.26), (2.54, 2.54))),
    );
    assert_eq!(u2_power_suggestion(&sheet).await, expected);
    assert_eq!(sheet.source(), source);
}

#[tokio::test]
async fn a_bus_touching_only_the_old_power_lead_refuses_advice() {
    let sheet = Sheet::copy(POWER).await;
    let source = insert_power_obstacle(&sheet, &bus_stroke((52.07, 124.46), (52.07, 121.92)));
    assert!(u2_power_suggestion(&sheet).await.is_null());
    assert_eq!(sheet.source(), source);
}

#[tokio::test]
async fn a_bus_crossing_only_the_proposed_power_stub_refuses_advice() {
    let sheet = Sheet::copy(POWER).await;
    let (x, y) = point_in_stub(&u2_power_suggestion(&sheet).await);
    let source = insert_power_obstacle(&sheet, &bus_stroke((x - 0.5, y), (x + 0.5, y)));
    assert!(u2_power_suggestion(&sheet).await.is_null());
    assert_eq!(sheet.source(), source);
}

#[tokio::test]
async fn a_tap_whose_interior_crosses_the_old_power_lead_refuses_advice() {
    let sheet = Sheet::copy(POWER).await;
    // The bus and both tap endpoints are off the lead.
    let source = insert_power_obstacle(
        &sheet,
        &(bus_stroke((50.8, 123.19), (53.34, 123.19)) + &bus_tap((52.07, 123.19), (2.54, 2.54))),
    );
    assert!(u2_power_suggestion(&sheet).await.is_null());
    assert_eq!(sheet.source(), source);
}

#[tokio::test]
async fn a_tap_whose_interior_crosses_the_proposed_power_stub_refuses_advice() {
    let sheet = Sheet::copy(POWER).await;
    let (x, y) = point_in_stub(&u2_power_suggestion(&sheet).await);
    let source = insert_power_obstacle(
        &sheet,
        &(bus_stroke((x - 1.0, y - 0.25), (x - 0.5, y - 0.25))
            + &bus_tap((x - 0.5, y - 0.25), (1.0, 0.5))),
    );
    assert!(u2_power_suggestion(&sheet).await.is_null());
    assert_eq!(sheet.source(), source);
}

#[tokio::test]
async fn an_unreadable_bus_does_not_establish_power_lead_clearance() {
    let sheet = Sheet::copy(POWER).await;
    let source = insert_power_obstacle(&sheet, "(bus (pts (xy 50.8 123.19)))");
    assert!(u2_power_suggestion(&sheet).await.is_null());
    assert_eq!(sheet.source(), source);
}

#[tokio::test]
async fn an_unreadable_tap_does_not_establish_power_lead_clearance() {
    let sheet = Sheet::copy(POWER).await;
    let source = insert_power_obstacle(
        &sheet,
        &(bus_stroke((264.16, 175.26), (279.4, 175.26))
            + "(bus_entry (at 269.24 175.26) (size 2.54 unknown))"),
    );
    assert!(u2_power_suggestion(&sheet).await.is_null());
    assert_eq!(sheet.source(), source);
}

#[test]
fn diagonal_bus_geometry_needs_actual_segment_contact() {
    let route = ((1.0, 1.0), (1.0, 2.0));
    // This stroke's bounding box meets the route at its upper end, but the
    // stroke itself is below the route at x = 1.
    assert!(!bus_contacts_route(route, &[((0.0, 2.0), (2.0, 4.0))]));
    assert!(bus_contacts_route(route, &[((0.0, 2.0), (2.0, 2.0))]));
    assert!(bus_contacts_route(route, &[((1.0, 1.25), (1.0, 1.75))]));
}

#[tokio::test]
async fn fields_do_not_move_when_every_side_is_blocked() {
    let sheet = Sheet::copy(LANES).await;
    let before = sheet.source();
    let offset = before.rfind(')').unwrap();
    let wires = (0..=40)
        .map(|i| {
            format!(
                "(wire (pts (xy 63 {}) (xy 80 {})) (stroke (width 0) (type default)))",
                68.0 + f64::from(i) * 0.4,
                68.0 + f64::from(i) * 0.4
            )
        })
        .collect::<Vec<_>>()
        .join("\n");
    let source = apply_edits(before, vec![SexpEdit::insert(offset, wires)]);
    fs::write(&sheet.path, &source).unwrap();
    let preview = sheet.call(OPERATION, json!({})).await;
    for name in ["Reference", "Value"] {
        assert_eq!(field(&preview, "C1", name)["reason"], "no_clear_side");
    }
    assert_eq!(sheet.source(), source);
}

fn placed_field(source: &str, reference: &str, name: &str) -> Position {
    let tree = parse_sexp(source).unwrap();
    let symbol = tree
        .find_all("symbol")
        .into_iter()
        .find(|node| {
            property(node, "Reference")
                .and_then(|p| p.get(2))
                .and_then(SexpNode::as_str)
                == Some(reference)
        })
        .unwrap();
    Position::read(property(symbol, name).unwrap()).unwrap()
}

#[tokio::test]
async fn single_placement_opt_in_turns_vertical_fields_horizontal() {
    let sheet = Sheet::copy(POWER).await;
    for (reference, x, opt_in) in [("C90", 160, None), ("C91", 180, Some(true))] {
        let mut args =
            json!({"lib_id":"Device:C", "reference": reference, "x":x, "y":70, "rotation":90});
        if let Some(opt_in) = opt_in {
            args["autoplace_fields"] = json!(opt_in);
        }
        let result = sheet.call("add_schematic_component", args).await;
        if opt_in.is_some() {
            let report = &result["autoplaced_fields"][0];
            assert_eq!(report["reference"], reference);
            assert!(report["fields"]
                .as_array()
                .unwrap()
                .iter()
                .all(|field| field["reason"] == "placed"
                    && field["position"]["rotation_degrees"] == 90.0));
        } else {
            assert!(result["autoplaced_fields"].is_null());
        }
    }
    let saved = sheet.source();
    for name in ["Reference", "Value"] {
        assert_eq!(placed_field(&saved, "C90", name).rotation_degrees, 0.0);
        assert_eq!(placed_field(&saved, "C91", name).rotation_degrees, 90.0);
    }
}

#[tokio::test]
async fn opt_in_placement_leaves_a_dotless_tee_but_connects_a_plain_tap() {
    use copperpour_sexp::schematic::extract_junctions;
    let sheet = Sheet::copy(POWER).await;
    // The normal wire tool adds the dot; leave it out deliberately for this contrast.
    let mut setup = copperpour_schematic_editor::Schematic::load(&sheet.path).unwrap();
    setup.add_wire(40.64, 180.34, 63.5, 180.34);
    setup.add_wire(50.8, 170.18, 50.8, 180.34);
    setup.overwrite().unwrap();
    for (x, reference, expected) in [(50.8, "R901", false), (55.88, "R902", true)] {
        let point = (x, 180.34);
        let junctions = |source: &str| extract_junctions(&parse_sexp(source).unwrap());
        let mut want = junctions(&sheet.source());
        assert!(!want.contains(&point));
        let result = sheet
            .call(
                "add_schematic_component",
                json!({"lib_id":"Device:R", "reference":reference, "x":x, "y":184.15,
                       "autoplace_fields":true}),
            )
            .await;
        if expected {
            want.push(point);
            assert_eq!(result["junctions_added"], json!([{"x":x,"y":point.1}]));
        } else {
            assert_eq!(result["junctions_added"], json!([]));
        }
        let mut got = junctions(&sheet.source());
        want.sort_by(|a, b| a.partial_cmp(b).unwrap());
        got.sort_by(|a, b| a.partial_cmp(b).unwrap());
        assert_eq!(got, want, "{reference}");
    }
}

#[tokio::test]
async fn opt_in_single_placement_refuses_a_fractional_angle() {
    let sheet = Sheet::copy(POWER).await;
    let before = sheet.source();
    let invalid = json!({
        "lib_id":"Device:C", "reference":"C91", "x":180, "y":70,
        "rotation":90.5, "autoplace_fields":true
    });
    let (error, refusal) = sheet.invoke("add_schematic_component", invalid).await;
    assert!(error, "{refusal}");
    assert_eq!(refusal["error"]["kind"], "invalid_argument");
    assert_eq!(refusal["error"]["field"], "rotation");
    assert_eq!(sheet.source(), before);
}

#[tokio::test]
async fn authoring_keeps_new_fields_at_defaults_with_a_fractional_obstacle() {
    for tool in ["add_schematic_component", "batch_place_components"] {
        let sheet = Sheet::copy(POWER).await;
        // The editor rounds this angle to zero on serialization. Inspecting
        // just the proposed sheet would miss the unmeasurable saved input.
        let legacy = parse_sexp(&use_legacy_angle(&sheet, "R3", 0.0000001)).unwrap();
        let old = legacy
            .find_all("symbol")
            .into_iter()
            .filter_map(symbol_instance)
            .find(|symbol| symbol.reference == "R3")
            .unwrap();
        let part = json!({
            "lib_id":"Device:C", "reference":"C91", "x":180, "y":70,
            "rotation":90, "autoplace_fields":true
        });
        let args = if tool == "batch_place_components" {
            json!({"components":[part]})
        } else {
            part
        };
        let result = sheet.call(tool, args).await;
        let reports = result["autoplaced_fields"].as_array().unwrap();
        assert_eq!(reports.len(), 1, "{result}");
        assert_eq!(reports[0]["reference"], "C91");
        assert_eq!(reports[0]["fields"][1]["name"], "Value");
        assert_eq!(reports[0]["fields"][1]["reason"], "cannot_measure");
        let saved = sheet.source();
        let observed = parse_sexp(&saved)
            .unwrap()
            .find_all("symbol")
            .into_iter()
            .filter_map(symbol_instance)
            .find(|symbol| symbol.uuid == old.uuid)
            .unwrap();
        assert_eq!(
            (observed.x, observed.y, observed.rotation),
            (old.x, old.y, 0.0)
        );
        assert_eq!(placed_field(&saved, "C91", "Value").rotation_degrees, 0.0);
    }
}

#[tokio::test]
async fn opt_in_batch_keeps_only_the_valid_quarter_turn() {
    let sheet = Sheet::copy(POWER).await;
    let invalid = json!({
        "lib_id":"Device:C", "reference":"C91", "x":180, "y":70,
        "rotation":90.5, "autoplace_fields":true
    });
    let mut valid = invalid.clone();
    valid["rotation"] = json!(450);
    let result = sheet
        .call(
            "batch_place_components",
            json!({"components":[invalid, valid]}),
        )
        .await;
    assert_eq!(result["placed_count"], 1, "{result}");
    assert_eq!(result["failures"][0]["index"], 0);
    assert_eq!(result["failures"][0]["kind"], "invalid_argument");
    assert_eq!(result["placed"][0]["rotation"], 90.0);
    let reports = result["autoplaced_fields"].as_array().unwrap();
    assert_eq!(reports.len(), 1);
    assert_eq!(reports[0]["reference"], "C91");
    assert_eq!(
        placed_field(&sheet.source(), "C91", "Value").rotation_degrees,
        90.0
    );
}

#[tokio::test]
async fn batch_placement_binds_opt_in_to_the_correct_unannotated_symbol() {
    let sheet = Sheet::copy(POWER).await;
    let result = sheet
        .call(
            "batch_place_components",
            json!({"components":[
                {"lib_id":"Device:R", "x":160, "y":70},
                {"lib_id":"Device:R", "x":180, "y":70, "autoplace_fields":true}
            ]}),
        )
        .await;
    assert_eq!(result["placed_count"], 2);
    let tree = parse_sexp(&sheet.source()).unwrap();
    let mut parts = tree
        .find_all("symbol")
        .into_iter()
        .filter(|node| {
            property(node, "Reference")
                .and_then(|p| p.get(2))
                .and_then(SexpNode::as_str)
                == Some("R?")
        })
        .collect::<Vec<_>>();
    parts.sort_by(|a, b| parse_at(a).unwrap().0.total_cmp(&parse_at(b).unwrap().0));
    assert_eq!(parts.len(), 2);
    assert_ne!(parts[0].find_str("uuid"), parts[1].find_str("uuid"));
    let reports = result["autoplaced_fields"].as_array().unwrap();
    assert_eq!(reports.len(), 1);
    assert_eq!(
        reports[0]["symbol_uuid"],
        parts[1].find_str("uuid").unwrap()
    );
    assert_eq!(
        Position::read(property(parts[0], "Value").unwrap())
            .unwrap()
            .rotation_degrees,
        90.0
    );
    assert_eq!(
        Position::read(property(parts[1], "Value").unwrap())
            .unwrap()
            .rotation_degrees,
        0.0
    );
}

async fn two_unannotated_resistors(sheet: &Sheet) -> Vec<String> {
    let placed = sheet
        .call(
            "batch_place_components",
            json!({"components":[
                {"lib_id":"Device:R", "x": 250.19, "y": 180.34},
                {"lib_id":"Device:R", "x": 270.51, "y": 180.34}
            ]}),
        )
        .await;
    let ids = placed["placed"]
        .as_array()
        .unwrap()
        .iter()
        .map(|part| {
            assert_eq!(part["reference"], "R?");
            part["uuid"].as_str().unwrap().to_owned()
        })
        .collect::<Vec<_>>();
    assert_eq!(ids.len(), 2);
    assert_ne!(ids[0], ids[1]);
    ids
}

#[tokio::test]
async fn a_saved_unannotated_approval_moves_only_its_uuid() {
    let sheet = Sheet::copy(POWER).await;
    let ids = two_unannotated_resistors(&sheet).await;
    let preview = sheet.call(OPERATION, json!({})).await;
    let offer = |id: &str, name: &str| {
        preview["symbols"]
            .as_array()
            .unwrap()
            .iter()
            .find(|part| part["symbol_uuid"] == id && part["reference"] == "R?")
            .unwrap()["fields"]
            .as_array()
            .unwrap()
            .iter()
            .find(|field| field["name"] == name)
            .unwrap()
    };
    let (first, second) = (offer(&ids[0], "Value"), offer(&ids[1], "Value"));
    let reference = offer(&ids[1], "Reference");
    assert!(first["approval"].is_string() && second["approval"].is_string());
    assert!(reference["approval"].is_string());
    assert_ne!(first["approval"], second["approval"]);
    let source = sheet.source();
    let before = parse_sexp(&source).unwrap();
    // Here the proposed Value would overlap the old Reference. A partial
    // approval must not move either field, or invalidate the unused revision.
    let (error, refusal) = sheet
        .invoke(
            OPERATION,
            json!({"revision": preview["revision"], "approvals":[second["approval"]]}),
        )
        .await;
    assert!(error, "{refusal}");
    assert_eq!(refusal["error"]["kind"], "plan_blocked");
    assert_eq!(refusal["error"]["operation"], OPERATION);
    assert_eq!(
        refusal["error"]["reasons"],
        json!(["Approved placements are not clear together."]),
        "{refusal}"
    );
    assert_eq!(sheet.source(), source);
    let applied = sheet
        .call(
            OPERATION,
            json!({"revision": preview["revision"], "approvals":[reference["approval"], second["approval"]]}),
        )
        .await;
    let positions = applied["placements"].as_array().unwrap();
    assert_eq!(positions.len(), 2);
    for field in [reference, second] {
        let observed = positions
            .iter()
            .find(|p| p["field"] == field["name"])
            .unwrap();
        assert_eq!(observed["symbol_uuid"], ids[1]);
        assert_eq!(observed["position"], field["proposed"]);
    }
    let after = parse_sexp(&sheet.source()).unwrap();
    let unchanged = |tree: &SexpNode| {
        tree.find_all("symbol")
            .into_iter()
            .find(|node| node.find_str("uuid") == Some(ids[0].as_str()))
            .unwrap()
            .clone()
    };
    assert_eq!(unchanged(&before), unchanged(&after));
}

#[tokio::test]
async fn equal_placement_proposals_on_different_uuids_have_different_tokens() {
    let sheet = Sheet::copy(LANES).await;
    let source = sheet.source();
    let tree = parse_sexp(&source).unwrap();
    let mut plans = options(&source, &tree, |_| true, None, true);
    let mut other = options(&source, &tree, |_| true, None, true);
    let first = plans.iter_mut().find(|p| p.reference == "C1").unwrap();
    let second = other.iter_mut().find(|p| p.reference == "C1").unwrap();
    second.symbol_uuid = "2bcd5d25-c05f-466c-bbe5-019a868aa9ea".to_owned();
    for plan in [&mut *first, &mut *second] {
        bind_approvals(
            std::slice::from_mut(plan),
            "same_revision",
            &json!(OPERATION),
        );
    }
    let tokens = [&*first, &*second].map(|plan| {
        plan.fields
            .iter()
            .find(|f| f.name == "Value")
            .and_then(|f| f.approval.clone())
            .unwrap()
    });
    assert_ne!(tokens[0], tokens[1]);
}

#[tokio::test]
async fn authoring_reports_follow_request_order_not_uuid_order() {
    let sheet = Sheet::copy(POWER).await;
    let mut ids = two_unannotated_resistors(&sheet).await;
    // Make the input disagree with UUID sorting on every run.
    ids.sort_unstable_by(|a, b| b.cmp(a));
    let source = sheet.source();
    let (_, fields) = autoplace_authoring(&source, source.clone(), &ids);
    let observed = fields
        .reports
        .into_iter()
        .map(|p| p.symbol_uuid)
        .collect::<Vec<_>>();
    assert_eq!(observed, ids);
}

#[tokio::test]
async fn rotate_opt_in_repositions_default_fields_and_retains_manual_offsets() {
    let sheet = Sheet::copy(POWER).await;
    sheet
        .call(
            "edit_schematic_component",
            json!({"reference":"C1",
        "field_placements":{"Reference":{"x":24, "y":42}}}),
        )
        .await;
    let before = sheet.source();
    let mut args = json!({"reference":"C1","rotation":90,"autoplace_fields":true});
    let preview = sheet.call("rotate_schematic_component", args.clone()).await;
    assert_eq!(sheet.source(), before);
    assert_eq!(preview["written"], false);
    assert_eq!(preview["turns"][0]["old"]["rotation_degrees"], 0.0);
    assert_eq!(preview["turns"][0]["proposed"]["rotation_degrees"], 90.0);
    assert_eq!(field(&preview, "C1", "Reference")["reason"], "positioned");
    let ids = approvals(&preview, "C1");
    assert_eq!(ids.len(), 1);
    args["revision"] = preview["revision"].clone();
    args["turn"] = preview["turn"].clone();
    args["approvals"] = json!(ids);
    let result = sheet.call("rotate_schematic_component", args).await;
    assert_eq!(
        result["approved_field_placements"][0]["position"],
        field(&preview, "C1", "Value")["proposed"]
    );
    assert_eq!(result["approved_field_placements"][0]["field"], "Value");
    assert_eq!(result["placements"][0]["rotation"], 90.0);
    let saved = sheet.source();
    assert_eq!(placed_field(&saved, "C1", "Value").rotation_degrees, 90.0);
    let manual = placed_field(&saved, "C1", "Reference");
    assert_eq!(manual.rotation_degrees, 0.0);
    assert!(!manual.justify.is_empty());
    // About (111.76, 50.8), delta (-87.76, -8.8) becomes (-8.8, 87.76).
    assert_eq!((manual.x_mm, manual.y_mm), (102.96, 138.56));
}

#[tokio::test]
async fn an_approved_turn_cannot_swap_one_component_overlap_for_another() {
    let sheet = Sheet::copy(POWER).await;
    let mut ids = Vec::new();
    for (reference, x, y) in [
        ("R90", 222.25, 30.48),
        ("R91", 222.65, 35.43),
        ("R92", 225.25, 30.88),
    ] {
        let placed = sheet
            .call(
                "add_schematic_component",
                json!({"lib_id":"Device:R", "reference":reference, "x":x, "y":y}),
            )
            .await;
        sheet
            .call(
                "edit_schematic_component",
                json!({"reference":reference, "field_placements":{
                    "Reference":{"hide":true}, "Value":{"hide":true}
                }}),
            )
            .await;
        let source = sheet.source();
        let id = placed["uuid"].as_str().unwrap();
        ids.push(id.to_owned());
        let (start, end) = item_range(&source, "symbol", id).unwrap();
        let (a, b) = direct_children_with_tag(&source[start..end], "symbol", "at")[0];
        fs::write(
            &sheet.path,
            apply_edits(
                source,
                vec![SexpEdit::replace(
                    start + a,
                    start + b,
                    format!("(at {x} {y} 0)"),
                )],
            ),
        )
        .unwrap();
    }
    let before = sheet.source();
    let tool = "rotate_schematic_component";
    let target = [ids[0].as_str()];
    let original = parse_sexp(&before).unwrap();
    let mut turned =
        copperpour_schematic_editor::Schematic::from_source(&sheet.path, before.clone()).unwrap();
    turned
        .symbols
        .iter_mut()
        .find(|s| s.uuid == ids[0])
        .unwrap()
        .set_rotation(90.0);
    let turned = parse_sexp(&turned.to_source()).unwrap();
    let pair = |n: usize| {
        let mut pair = [ids[0].clone(), ids[n].clone()];
        pair.sort_unstable();
        BTreeSet::from([pair])
    };
    assert_eq!(component_overlap_pairs(&original, &target), pair(1));
    assert_eq!(component_overlap_pairs(&turned, &target), pair(2));
    let mut args = json!({"reference":"R90","rotation":90,"autoplace_fields":true});
    let preview = sheet.call(tool, args.clone()).await;
    assert_eq!(
        preview["layout_verdict_without_field_changes"], "unchanged",
        "{preview}"
    );
    assert_eq!(sheet.source(), before);
    args["revision"] = preview["revision"].clone();
    args["turn"] = preview["turn"].clone();
    args["approvals"] = json!([]);
    let (error, body) = sheet.invoke(tool, args).await;
    assert!(error, "{body}");
    assert_eq!(body["error"]["kind"], "plan_blocked", "{body}");
    assert!(
        body["error"]["reasons"][0]
            .as_str()
            .unwrap()
            .contains("new component overlap"),
        "{body}"
    );
    assert_eq!(sheet.source(), before);
}

#[tokio::test]
async fn empty_field_approvals_cannot_authorize_a_different_turn() {
    let sheet = Sheet::copy(POWER).await;
    let before = sheet.source();
    let tool = "rotate_schematic_component";
    let mut args = json!({"reference":"C1","rotation":90,"autoplace_fields":true});
    let preview = sheet.call(tool, args.clone()).await;
    args["revision"] = preview["revision"].clone();
    args["turn"] = preview["turn"].clone();
    args["approvals"] = json!([]);
    args["rotation"] = json!(270);
    let (error, body) = sheet.invoke(tool, args.clone()).await;
    assert!(error, "{body}");
    assert_eq!(body["error"]["kind"], "invalid_argument");
    assert_eq!(sheet.source(), before);
    args["rotation"] = json!(90);
    let result = sheet.call(tool, args).await;
    assert_eq!(result["rotation"], 90.0);
    assert_eq!(result["approved_field_placements"], json!([]));
}

#[tokio::test]
async fn an_approved_quarter_turn_refuses_fractional_angles_and_accepts_equivalent_turns() {
    let sheet = Sheet::copy(POWER).await;
    let before = sheet.source();
    let tool = "rotate_schematic_component";
    let mut args = json!({"reference":"C1","rotation":450,"autoplace_fields":true});
    let preview = sheet.call(tool, args.clone()).await;
    assert_eq!(preview["rotation_degrees"], 90.0);
    assert_eq!(sheet.source(), before);
    args["revision"] = preview["revision"].clone();
    args["turn"] = preview["turn"].clone();
    args["approvals"] = json!([]);
    args["rotation"] = json!(90.5);
    let (error, refusal) = sheet.invoke(tool, args.clone()).await;
    assert!(error, "{refusal}");
    assert_eq!(
        refusal["error"],
        json!({"kind":"invalid_argument","field":"rotation","reason":"must be a multiple of 90 degrees"})
    );
    assert_eq!(sheet.source(), before);
    args["rotation"] = json!(90);
    let result = sheet.call(tool, args).await;
    assert_eq!(result["rotation"], 90.0);
    assert_eq!(result["approved_field_placements"], json!([]));
}

#[tokio::test]
async fn zero_and_negative_full_turns_share_one_approval() {
    let sheet = Sheet::copy(POWER).await;
    let before = sheet.source();
    let tool = "rotate_schematic_component";
    let mut args = json!({"reference":"C1","rotation":-360.0,"autoplace_fields":true});
    let preview = sheet.call(tool, args.clone()).await;
    args["rotation"] = json!(0.0);
    let zero = sheet.call(tool, args.clone()).await;
    assert_eq!(preview["turn"], zero["turn"]);
    assert_eq!(preview["rotation_degrees"].to_string(), "0.0");
    assert_eq!(sheet.source(), before);
    args["revision"] = preview["revision"].clone();
    args["turn"] = preview["turn"].clone();
    args["approvals"] = json!([]);
    let result = sheet.call(tool, args).await;
    assert_eq!(result["rotation"], 0.0);
}

#[tokio::test]
async fn opt_in_turn_refuses_unmeasurable_angle_on_another_symbol() {
    let sheet = Sheet::copy(POWER).await;
    let tool = "rotate_schematic_component";
    let args = json!({"reference":"C1","rotation":90,"autoplace_fields":true});
    assert!(sheet.call(tool, args.clone()).await["turn"].is_string());
    let legacy = use_legacy_angle(&sheet, "R3", 0.5);
    let (error, refusal) = sheet.invoke(tool, args).await;
    assert!(error, "{refusal}");
    assert_eq!(refusal["error"]["kind"], "plan_blocked");
    assert_eq!(sheet.source(), legacy);
}

#[tokio::test]
async fn opt_in_rotation_refuses_to_disconnect_a_wired_symbol() {
    let sheet = Sheet::copy(LANES).await;
    let before = sheet.source();
    let (error, result) = sheet
        .invoke(
            "rotate_schematic_component",
            json!({"reference":"C1","rotation":0,"autoplace_fields":true}),
        )
        .await;
    assert!(error, "{result}");
    assert_eq!(result["error"]["kind"], "plan_blocked");
    assert_eq!(result["error"]["operation"], ROTATE_OPERATION);
    assert_eq!(sheet.source(), before);
}

#[tokio::test]
async fn save_during_staging_refuses_before_replacing_the_sheet() {
    let sheet = Sheet::copy(LANES).await;
    let preview = sheet.call(OPERATION, json!({})).await;
    let ids = approvals(&preview, "C1");
    assert_eq!(ids.len(), 2);
    let concurrent = sheet.source() + "\n";
    copperpour_sexp::writer::test_hooks::save_before_final_check(&sheet.path, concurrent.clone());
    let (error, body) = sheet
        .invoke(
            OPERATION,
            json!({
                "revision": preview["revision"], "approvals": ids
            }),
        )
        .await;
    assert!(error, "{body}");
    assert_eq!(body["error"]["kind"], "conflict");
    assert!(
        body["message"].as_str().unwrap().contains("preview again"),
        "{body}"
    );
    assert_eq!(sheet.source(), concurrent);
}

#[tokio::test]
async fn a_turn_conflict_requires_a_fresh_preview() {
    let sheet = Sheet::copy(POWER).await;
    let tool = "rotate_schematic_component";
    let mut args = json!({"reference":"C1","rotation":90,"autoplace_fields":true});
    let preview = sheet.call(tool, args.clone()).await;
    args["revision"] = preview["revision"].clone();
    args["turn"] = preview["turn"].clone();
    args["approvals"] = json!(approvals(&preview, "C1"));
    let concurrent = sheet.source() + "\n";
    copperpour_sexp::writer::test_hooks::save_before_final_check(&sheet.path, concurrent.clone());
    let (error, body) = sheet.invoke(tool, args).await;
    assert!(error, "{body}");
    assert_eq!(body["error"]["kind"], "conflict", "{body}");
    assert!(
        body["message"].as_str().unwrap().contains("preview again"),
        "{body}"
    );
    assert_eq!(sheet.source(), concurrent);
}

#[tokio::test]
async fn a_legacy_turn_conflict_does_not_ask_for_a_preview() {
    let sheet = Sheet::copy(POWER).await;
    let concurrent = sheet.source() + "\n";
    copperpour_sexp::writer::test_hooks::save_before_final_check(&sheet.path, concurrent.clone());
    let (error, body) = sheet
        .invoke(
            "rotate_schematic_component",
            json!({"reference":"C1","rotation":90}),
        )
        .await;
    assert!(error, "{body}");
    assert_eq!(body["error"]["kind"], "conflict", "{body}");
    assert!(
        !body["message"].as_str().unwrap().contains("preview again"),
        "{body}"
    );
    assert_eq!(sheet.source(), concurrent);
}

#[tokio::test]
async fn rotate_without_opt_in_keeps_the_default_field_carry() {
    let sheet = Sheet::copy(POWER).await;
    let before = sheet.source();
    let field = placed_field(&before, "C1", "Value");
    sheet
        .call(
            "rotate_schematic_component",
            json!({"reference":"C1","rotation":90}),
        )
        .await;
    let saved = placed_field(&sheet.source(), "C1", "Value");
    assert_eq!(field.rotation_degrees, saved.rotation_degrees);
    assert_eq!(field.justify, saved.justify);
    assert_ne!((field.x_mm, field.y_mm), (saved.x_mm, saved.y_mm));
}

#[tokio::test]
async fn a_served_edit_keeps_bare_and_yes_marker_evidence() {
    let sheet = Sheet::copy(LANES).await;
    let original = sheet.source();
    for marker in ["(fields_autoplaced)", "(fields_autoplaced yes)"] {
        let source = c1_with_nondefault_justification(original.clone(), marker);
        fs::write(&sheet.path, source).unwrap();
        // The non-opt-in move round-trips the placed symbol through the editor.
        sheet
            .call(
                "move_schematic_component",
                json!({"reference":"C1", "x": 71.12, "y": 60.96}),
            )
            .await;
        let saved = sheet.source();
        let tree = parse_sexp(&saved).unwrap();
        let part = placed_symbols(&tree, true)
            .into_iter()
            .find(|p| p.instance.reference == "C1")
            .unwrap();
        assert!(
            matches!(
                part.field_evidence(
                    "Reference",
                    &Position::read(property(part.node, "Reference").unwrap()).unwrap()
                ),
                Some(FieldEvidence::FieldsAutoplaced)
            ),
            "{marker}"
        );
        let preview = sheet.call(OPERATION, json!({})).await;
        assert_eq!(
            field(&preview, "C1", "Reference")["evidence"],
            "fields_autoplaced"
        );
    }
}

#[tokio::test]
async fn a_served_edit_does_not_promote_an_explicit_no_marker() {
    let sheet = Sheet::copy(LANES).await;
    let source = c1_with_nondefault_justification(sheet.source(), "(fields_autoplaced no)");
    fs::write(&sheet.path, source).unwrap();
    sheet
        .call(
            "move_schematic_component",
            json!({"reference":"C1", "x":71.12, "y":60.96}),
        )
        .await;
    let saved = sheet.source();
    let tree = parse_sexp(&saved).unwrap();
    let part = placed_symbols(&tree, true)
        .into_iter()
        .find(|p| p.instance.reference == "C1")
        .unwrap();
    assert_ne!(part.node.find_str("fields_autoplaced"), Some("yes"));
    let preview = sheet.call(OPERATION, json!({})).await;
    assert!(field(&preview, "C1", "Reference")["evidence"].is_null());
}

fn replace_test_field(source: String, uuid: &str, name: &str, property: &str) -> String {
    let (start, end) = item_range(&source, "symbol", uuid).unwrap();
    let (p, q) = direct_children_with_tag(&source[start..end], "symbol", "property")
        .into_iter()
        .find(|&(p, q)| {
            property_name(&parse_sexp(&source[start + p..start + q]).unwrap()) == Some(name)
        })
        .unwrap();
    apply_edits(
        source,
        vec![SexpEdit::replace(start + p, start + q, property)],
    )
}

/// Observe the actual leave-as-is carry through the legacy served path. The
/// exact legacy verdict and net check qualify it as a clearance-only negative.
async fn turn_only_collision(
    sheet: &Sheet,
    reference: &str,
    expected: FieldCollision,
) -> (BTreeSet<FieldCollision>, BTreeSet<FieldCollision>) {
    let tool = "rotate_schematic_component";
    let before = sheet.source();
    sheet
        .call(tool, json!({"reference": reference, "rotation": 180}))
        .await;
    let original = parse_sexp(&before).unwrap();
    let turned = parse_sexp(&sheet.source()).unwrap();
    assert!(
        snapshot_net_changes(&original, &turned, &BTreeSet::new(), NetComparison::Layout,)
            .is_empty()
    );
    assert_eq!(layout_verdict(&original, &turned), "unchanged");
    fs::write(&sheet.path, &before).unwrap();
    let mut args = json!({"reference": reference, "rotation": 180, "autoplace_fields": true});
    let preview = sheet.call(tool, args.clone()).await;
    assert_eq!(preview["layout_verdict_without_field_changes"], "unchanged");
    assert_eq!(sheet.source(), before);
    args["revision"] = preview["revision"].clone();
    args["turn"] = preview["turn"].clone();
    args["approvals"] = json!([]);
    let (error, body) = sheet.invoke(tool, args).await;
    assert!(error, "{body}");
    assert_eq!(body["error"]["kind"], "plan_blocked", "{body}");
    assert!(
        body["error"]["reasons"][0]
            .as_str()
            .unwrap()
            .contains("new field collision"),
        "{body}"
    );
    assert_eq!(sheet.source(), before);

    let old = field_collisions(&original);
    let new = field_collisions(&turned);
    assert_eq!(new.difference(&old).collect::<Vec<_>>(), vec![&expected]);
    (old, new)
}

#[tokio::test]
async fn a_turn_without_field_approvals_refuses_text_on_a_neighbour_pin() {
    let sheet = Sheet::copy(POWER).await;
    let part = sheet
        .call(
            "add_schematic_component",
            json!({
                "lib_id": "Device:C", "reference": "C90", "x": 260.35, "y": 180.34,
            }),
        )
        .await;
    let neighbour = sheet
        .call(
            "add_schematic_component",
            json!({
                "lib_id": "Device:R", "reference": "R90", "x": 250.19, "y": 184.15,
            }),
        )
        .await;
    let id = part["uuid"].as_str().unwrap();
    let mut source = replace_test_field(
        sheet.source(),
        id,
        "Reference",
        r#"(property "Reference" "C90" (at 270.51 180.34 0) (effects (font (size 1.27 1.27))))"#,
    );
    source = replace_test_field(
        source,
        id,
        "Value",
        r#"(property "Value" "C" (at 260.35 180.34 0)
        (effects (font (size 1.27 1.27)) (hide yes)))"#,
    );
    fs::write(&sheet.path, source).unwrap();
    turn_only_collision(
        &sheet,
        "C90",
        FieldCollision {
            symbol: id.to_owned(),
            field: "Reference".to_owned(),
            obstacle: FieldObstacle::Pin(neighbour["uuid"].as_str().unwrap().to_owned(), 0),
        },
    )
    .await;
}

#[tokio::test]
async fn a_turn_without_field_approvals_refuses_text_on_its_own_body() {
    let sheet = Sheet::copy(POWER).await;
    let part = sheet
        .call(
            "add_schematic_component",
            json!({
                "lib_id": "Device:C", "reference": "C90", "x": 260.35, "y": 180.34,
            }),
        )
        .await;
    let id = part["uuid"].as_str().unwrap();
    // The upright ink has more reach above its anchor than below. A half
    // turn carries I below the body without turning the ink upside down.
    let mut source = replace_test_field(
        sheet.source(),
        id,
        "Value",
        r#"(property "Value" "I" (at 261.62 178.878 0) (effects (font (size 1.27 1.27))))"#,
    );
    source = replace_test_field(
        source,
        id,
        "Reference",
        r#"(property "Reference" "C90"
        (at 260.35 180.34 0) (effects (font (size 1.27 1.27)) (hide yes)))"#,
    );
    fs::write(&sheet.path, source).unwrap();
    turn_only_collision(
        &sheet,
        "C90",
        FieldCollision {
            symbol: id.to_owned(),
            field: "Value".to_owned(),
            obstacle: FieldObstacle::OwnBody,
        },
    )
    .await;
}

#[tokio::test]
async fn a_turn_cannot_trade_one_visible_power_reference_crossing_for_another() {
    let sheet = Sheet::copy(POWER).await;
    let part = sheet
        .call(
            "add_schematic_component",
            json!({
                "lib_id": "power:GND", "reference": "#PWR90", "x": 260.35, "y": 180.34,
            }),
        )
        .await;
    let id = part["uuid"].as_str().unwrap();
    let reference = part["reference"].as_str().unwrap();
    let source = replace_test_field(
        sheet.source(),
        id,
        "Reference",
        &format!(
            r#"(property "Reference" "{reference}" (at 270.51 180.34 0) (effects (font (size 1.27 1.27))))"#
        ),
    );
    let source = replace_test_field(
        source,
        id,
        "Value",
        r#"(property "Value" "GND" (at 260.35 180.34 0)
        (effects (font (size 1.27 1.27)) (hide yes)))"#,
    );
    let new_wire = "cdeb86fa-7310-401a-b457-7352fae01104";
    let end = source.rfind(')').unwrap();
    let source = apply_edits(
        source,
        vec![SexpEdit::insert(
            end,
            format!(
                r#"
        (wire (pts (xy 270.51 179.07) (xy 270.51 181.61)) (stroke (width 0) (type default))
            (uuid "21bc27e7-53db-4ecb-8506-be900c4981d5"))
        (wire (pts (xy 250.19 179.07) (xy 250.19 181.61)) (stroke (width 0) (type default))
            (uuid "{new_wire}"))
    "#
            ),
        )],
    );
    fs::write(&sheet.path, &source).unwrap();
    let legacy = sheet.call("check_schematic_overlaps", json!({})).await;
    assert!(!legacy["overlaps"]
        .as_array()
        .unwrap()
        .iter()
        .any(|o| { o["type"] == "wire_field_overlap" && o["b"]["symbol_uuid"] == id }));
    let (old, new) = turn_only_collision(
        &sheet,
        reference,
        FieldCollision {
            symbol: id.to_owned(),
            field: "Reference".to_owned(),
            obstacle: FieldObstacle::Reported(json!(["wire", new_wire]).to_string()),
        },
    )
    .await;
    assert_eq!(old.len(), new.len());
}

#[tokio::test]
async fn explicit_field_positioning_clears_autoplacement_in_single_and_batch() {
    let sheet = Sheet::copy(LANES).await;
    let before = sheet.source();
    let marked = before.replacen(
        r#"(property "Reference" "C1""#,
        r#"(fields_autoplaced yes) (property "Reference" "C1""#,
        1,
    );
    let placements = json!({"reference":"C1", "field_placements":{"Reference":{"x":73}}});
    for (name, args) in [
        ("edit_schematic_component", placements.clone()),
        (
            "batch_edit_schematic_components",
            json!({"edits":[placements]}),
        ),
    ] {
        fs::write(&sheet.path, &marked).unwrap();
        sheet.call(name, args).await;
        let saved = sheet.source();
        let tree = parse_sexp(&saved).unwrap();
        let part = tree
            .find_all("symbol")
            .into_iter()
            .find(|node| {
                property(node, "Reference")
                    .and_then(|p| p.get(2))
                    .and_then(SexpNode::as_str)
                    == Some("C1")
            })
            .unwrap();
        assert!(part.find("fields_autoplaced").is_none(), "{name}");
        assert_eq!(placed_field(&saved, "C1", "Reference").x_mm, 73.0, "{name}");
    }
}

#[tokio::test]
async fn unreadable_sheets_are_typed_refusals_without_writing() {
    let sheet = Sheet::copy(LANES).await;
    let missing = sheet.path.with_file_name("missing.kicad_sch");
    let result = served_call(
        &sheet.handler,
        OPERATION,
        json!({"schematic": missing, "approvals": []}),
    )
    .await;
    assert!(result.is_error);
    let body: Value = serde_json::from_str(text_of(&result)).unwrap();
    assert_eq!(body["error"]["kind"], "file_not_found", "{body}");
    assert!(!missing.exists());
    let not_a_sheet = b"(kicad_pcb (version 20240108))".to_vec();
    let unbalanced = b"(kicad_sch (version 20250114)".to_vec();
    let not_utf8 = b"(kicad_sch (version 20250114) \xff)".to_vec();
    for bytes in [not_a_sheet, unbalanced, not_utf8] {
        fs::write(&sheet.path, &bytes).unwrap();
        for (tool, args) in [
            (OPERATION, json!({})),
            (
                "rotate_schematic_component",
                json!({"reference":"C1","rotation":90,"autoplace_fields":true}),
            ),
        ] {
            let (is_error, body) = sheet.invoke(tool, args).await;
            assert!(is_error, "{tool}: {body}");
            assert_eq!(body["error"]["kind"], "malformed_file", "{tool}: {body}");
            assert_eq!(fs::read(&sheet.path).unwrap(), bytes);
        }
    }
}

#[tokio::test]
async fn stale_approvals_conflict_before_decoding_or_resolving_targets() {
    let sheet = Sheet::copy(POWER).await;
    let original = sheet.source();
    let field = sheet.call(OPERATION, json!({})).await;
    let tool = "rotate_schematic_component";
    let mut args = json!({"reference":"C1", "rotation":90, "autoplace_fields":true});
    let turn = sheet.call(tool, args.clone()).await;
    args["revision"] = turn["revision"].clone();
    args["turn"] = turn["turn"].clone();
    args["approvals"] = json!(approvals(&turn, "C1"));

    for bytes in [
        original
            .replace(
                r#"(property "Reference" "C1""#,
                r#"(property "Reference" "C99""#,
            )
            .into_bytes(),
        b"(kicad_sch (version 20250114)".to_vec(),
        b"(kicad_sch (version 20250114) \xff)".to_vec(),
    ] {
        assert_ne!(bytes, original.as_bytes());
        fs::write(&sheet.path, &bytes).unwrap();
        for (tool, args) in [
            (
                OPERATION,
                json!({"revision": field["revision"], "approvals": approvals(&field, "R2")}),
            ),
            (tool, args.clone()),
        ] {
            let (error, body) = sheet.invoke(tool, args).await;
            assert!(error, "{tool}: {body}");
            assert_eq!(body["error"]["kind"], "conflict", "{tool}: {body}");
            assert!(body["message"].as_str().unwrap().contains("preview again"));
            assert_eq!(fs::read(&sheet.path).unwrap(), bytes);
        }
    }
}

fn save_before_snapshot(path: &Path) {
    crate::tools::sch_components::COMPONENT_SAVE_BEFORE_SNAPSHOT
        .lock()
        .unwrap()
        .push((path.to_path_buf(), "\n".to_owned()));
}

fn uncertain(body: &Value) {
    assert_eq!(
        body["error"]["kind"], "mutation_outcome_uncertain",
        "{body}"
    );
    assert_eq!(body["outcome"]["status"], "uncertain", "{body}");
}

#[tokio::test]
async fn rotation_keeps_verified_turn_and_marker_when_fields_are_uncertain() {
    let sheet = Sheet::copy(POWER).await;
    sheet
        .call("add_no_connect", json!({"x":111.76, "y":46.99}))
        .await;
    let tool = "rotate_schematic_component";
    let mut args = json!({"reference":"C1", "rotation":90, "autoplace_fields":true});
    let preview = sheet.call(tool, args.clone()).await;
    let uuid = symbol(&preview, "C1")["symbol_uuid"].as_str().unwrap();
    args["revision"] = preview["revision"].clone();
    args["turn"] = preview["turn"].clone();
    args["approvals"] = json!(approvals(&preview, "C1"));
    save_before_snapshot(&sheet.path);
    let (error, body) = sheet.invoke(tool, args).await;
    assert!(error, "{body}");
    assert_eq!(
        body["error"]["kind"], "mutation_outcome_uncertain",
        "{body}"
    );
    let observed = &body["observed_before_uncertainty"];
    assert_eq!(observed["placements"][0]["uuid"], uuid, "{body}");
    assert_eq!(observed["placements"][0]["rotation"], 90.0, "{body}");
    let markers = observed["no_connects_moved"].as_array().unwrap();
    assert_eq!(markers.len(), 1, "{body}");
    let marker = &markers[0];
    assert_eq!(marker["to"], json!({"x":107.95, "y":50.8}), "{body}");
    let saved = parse_sexp(&sheet.source()).unwrap();
    let marker = saved
        .find_all("no_connect")
        .into_iter()
        .find(|node| node.find_str("uuid") == marker["uuid"].as_str())
        .unwrap();
    assert_eq!(parse_at(marker), Some((107.95, 50.8, 0.0)));
    assert!(body["approved_field_placements"].is_null());
}

#[tokio::test]
async fn single_placement_keeps_its_readback_when_fields_cannot_be_verified() {
    let sheet = Sheet::copy(POWER).await;
    save_before_snapshot(&sheet.path);
    let (error, body) = sheet
        .invoke(
            "add_schematic_component",
            json!({"lib_id":"Device:C", "reference":"C91", "x":180, "y":70, "rotation":90,
                "autoplace_fields":true}),
        )
        .await;
    assert!(error, "{body}");
    uncertain(&body);
    assert_eq!(body["outcome"]["completed"], 1);
    assert_eq!(body["placed"]["reference"], "C91", "{body}");
    assert!(body["autoplaced_fields"].is_null());
}

#[tokio::test]
async fn batch_placement_keeps_placed_parts_and_failures_when_fields_cannot_be_verified() {
    let sheet = Sheet::copy(POWER).await;
    save_before_snapshot(&sheet.path);
    let (error, body) = sheet
        .invoke(
            "batch_place_components",
            json!({"components":[
                {"lib_id":"Device:R", "reference":"R90", "x":160, "y":70, "autoplace_fields":true},
                {"lib_id":"Device:NoSuchPart", "x":180, "y":70, "autoplace_fields":true}
            ]}),
        )
        .await;
    assert!(error, "{body}");
    uncertain(&body);
    assert_eq!(body["outcome"]["completed"], 1);
    assert_eq!(body["outcome"]["failed"], 1);
    assert_eq!(body["placed_count"], 1, "{body}");
    assert_eq!(body["placed"][0]["reference"], "R90", "{body}");
    assert_eq!(body["failures"][0]["index"], 1, "{body}");
    assert!(body["autoplaced_fields"].is_null());
}

#[tokio::test]
async fn a_field_the_caller_placed_is_never_moved() {
    let sheet = Sheet::copy(POWER).await;
    let placed = json!({"x": 160.02, "y": 134.62, "rotation": 0});
    sheet
        .call(
            "edit_schematic_component",
            json!({"reference":"R13", "field_placements":{"Reference": placed}}),
        )
        .await;
    let content = sheet.source();
    let offset = content.rfind(')').unwrap();
    let source = apply_edits(
        content,
        vec![SexpEdit::insert(
            offset,
            r#"(wire (pts (xy 156.21 134.62) (xy 163.83 134.62)) (stroke (width 0) (type default))
            (uuid "5d0c4b8e-3f8a-4f7e-9a51-0d1f6b7c2e11"))"#,
        )],
    );
    fs::write(&sheet.path, &source).unwrap();
    let overlaps = sheet.call("check_schematic_overlaps", json!({})).await;
    assert!(overlaps["overlaps"].as_array().unwrap().iter().any(|o| {
        o["type"] == "wire_field_overlap"
            && o["b"]["reference"] == "R13"
            && o["b"]["field"] == "Reference"
    }));
    let before = placed_field(&source, "R13", "Reference");
    let preview = sheet.call(OPERATION, json!({})).await;
    let reference = field(&preview, "R13", "Reference");
    assert_eq!(reference["reason"], "positioned", "{reference}");
    assert!(reference["approval"].is_null());
    let all = preview["symbols"]
        .as_array()
        .unwrap()
        .iter()
        .flat_map(|part| part["fields"].as_array().unwrap())
        .filter_map(|field| field["approval"].as_str())
        .collect::<Vec<_>>();
    assert!(!all.is_empty());
    sheet
        .call(
            OPERATION,
            json!({"revision": preview["revision"], "approvals": all}),
        )
        .await;
    assert_eq!(placed_field(&sheet.source(), "R13", "Reference"), before);
}

/// The issue's two library-default cases: R2's Value over its body after a
/// 90° placement, and C2's vertical fields.
#[tokio::test]
async fn rotated_parts_get_horizontal_fields_off_their_bodies() {
    let sheet = Sheet::copy(POWER).await;
    let wanted = [("R2", "Value"), ("C2", "Reference"), ("C2", "Value")];
    // Whether each field is drawn horizontally and clear of its own body.
    let readable = |source: &str| {
        let tree = parse_sexp(source).unwrap();
        let parts = placed_symbols(&tree, true);
        wanted.map(|(reference, name)| {
            let part = parts
                .iter()
                .find(|p| p.instance.reference == reference)
                .unwrap();
            let field = property(part.node, name).unwrap();
            let run = part.measure(field).unwrap();
            let body = part.geometry.as_ref().unwrap().body.unwrap();
            let angle = Position::read(field).unwrap().rotation_degrees;
            (
                field_drawn_orientation(angle, part.instance.rotation) == "horizontal",
                !run.interiors_intersect(body),
            )
        })
    };
    assert_eq!(
        readable(&sheet.source()),
        [(true, false), (false, true), (false, true)]
    );
    let preview = sheet.call(OPERATION, json!({})).await;
    let ids = wanted
        .iter()
        .map(|(reference, name)| {
            let proposal = field(&preview, reference, name);
            assert_eq!(proposal["evidence"], "library_default", "{proposal}");
            proposal["approval"].clone()
        })
        .collect::<Vec<_>>();
    sheet
        .call(
            OPERATION,
            json!({"revision": preview["revision"], "approvals": ids}),
        )
        .await;
    assert_eq!(readable(&sheet.source()), [(true, true); 3]);
}

/// A clear spot nearer another part's body than its own is reported, not
/// proposed, and the part's fields stay where they are.
#[tokio::test]
async fn a_spot_that_would_label_a_neighbour_is_reported() {
    let sheet = Sheet::copy(POWER).await;
    sheet
        .call(
            "add_schematic_component",
            json!({"lib_id":"Device:R", "reference":"R90", "x":160.02, "y":69.85,
                "value":"1k 1% 0603 thin film"}),
        )
        .await;
    let alone = sheet.call(OPERATION, json!({})).await;
    for name in ["Reference", "Value"] {
        assert_eq!(field(&alone, "R90", name)["reason"], "clear_position");
    }
    for (reference, x) in [("R91", 170.18), ("R92", 149.86)] {
        sheet
            .call(
                "add_schematic_component",
                json!({"lib_id":"Device:R", "reference":reference, "x":x, "y":77.47}),
            )
            .await;
    }
    let source = sheet.source();
    let crowded = sheet.call(OPERATION, json!({})).await;
    for name in ["Reference", "Value"] {
        let report = field(&crowded, "R90", name);
        assert_eq!(report["reason"], "would_label_neighbour", "{report}");
        assert!(report["approval"].is_null());
    }
    assert_eq!(sheet.source(), source);
}

/// C1 with both fields moved off their library justification and the symbol
/// marked `(fields_autoplaced yes)`, so the marker is their only evidence.
fn marked_off_default(mut source: String, value: &str) -> String {
    for name in [format!("Value\" \"{value}"), "Reference\" \"C1".to_owned()] {
        let offset = source.find(&format!("(property \"{name}\"")).unwrap();
        let justify = offset + source[offset..].find("(justify left)").unwrap();
        source = apply_edits(
            source,
            vec![SexpEdit::replace(
                justify,
                justify + "(justify left)".len(),
                "(justify left bottom)",
            )],
        );
    }
    let offset = source.find(r#"(property "Reference" "C1""#).unwrap();
    apply_edits(
        source,
        vec![SexpEdit::insert(offset, "(fields_autoplaced yes)\n\t\t")],
    )
}

/// Approving one field clears the symbol's marker, so a second field offered
/// only on that marker's evidence is reported as withdrawn.
#[tokio::test]
async fn approving_one_marked_field_withdraws_the_other_offer() {
    let sheet = Sheet::copy(LANES).await;
    let source = marked_off_default(sheet.source(), "100nF");
    fs::write(&sheet.path, &source).unwrap();
    let preview = sheet.call(OPERATION, json!({})).await;
    for name in ["Reference", "Value"] {
        assert_eq!(field(&preview, "C1", name)["evidence"], "fields_autoplaced");
    }
    let applied = sheet
        .call(
            OPERATION,
            json!({
                "revision": preview["revision"],
                "approvals": [field(&preview, "C1", "Reference")["approval"]]
            }),
        )
        .await;
    let withdrawn = applied["withdrawn_offers"].as_array().unwrap();
    assert_eq!(withdrawn.len(), 1, "{applied}");
    assert_eq!(withdrawn[0]["field"], "Value");
    assert_eq!(withdrawn[0]["reference"], "C1");
    let after = sheet.call(OPERATION, json!({})).await;
    assert_eq!(field(&after, "C1", "Value")["reason"], "positioned");
}

#[tokio::test]
async fn an_opt_in_turn_reports_the_marked_offer_it_withdraws() {
    let sheet = Sheet::copy(POWER).await;
    let source = marked_off_default(sheet.source(), "100n");
    fs::write(&sheet.path, &source).unwrap();
    let mut args = json!({"reference":"C1","rotation":270,"autoplace_fields":true});
    let preview = sheet.call("rotate_schematic_component", args.clone()).await;
    for name in ["Reference", "Value"] {
        let offer = field(&preview, "C1", name);
        assert_eq!(offer["evidence"], "fields_autoplaced", "{offer}");
        assert!(offer["approval"].is_string(), "{offer}");
    }
    args["revision"] = preview["revision"].clone();
    args["turn"] = preview["turn"].clone();
    args["approvals"] = json!([field(&preview, "C1", "Reference")["approval"]]);
    let result = sheet.call("rotate_schematic_component", args).await;
    let withdrawn = result["withdrawn_offers"].as_array().unwrap();
    assert_eq!(withdrawn.len(), 1, "{result}");
    assert_eq!(withdrawn[0]["field"], "Value");
}

/// The planner treats a visible power Reference crossed by a wire as unclear,
/// although the legacy overlap report leaves that text out.
#[tokio::test]
async fn a_wire_across_a_visible_power_reference_is_not_clear() {
    let sheet = Sheet::copy(POWER).await;
    let content = sheet.source();
    let offset = content.find(r##"(property "Reference" "#PWR01""##).unwrap();
    let hide = offset + content[offset..].find("(hide yes)").unwrap();
    let end = content.rfind(')').unwrap();
    let source = apply_edits(
        content,
        vec![
            SexpEdit::delete(hide, hide + "(hide yes)".len()),
            SexpEdit::insert(
                end,
                r#"(wire (pts (xy 223.52 143.51) (xy 233.68 143.51)) (stroke (width 0) (type default))
                (uuid "0c9a4f2e-6b1d-4a57-8f43-2e7d5b9c1a60"))"#,
            ),
        ],
    );
    fs::write(&sheet.path, &source).unwrap();
    let preview = sheet.call(OPERATION, json!({})).await;
    let reference = field(&preview, "#PWR01", "Reference");
    assert_ne!(reference["reason"], "already_clear", "{reference}");
    assert_ne!(reference["reason"], "hidden", "{reference}");
}
