use super::{assert_verdict, handler, ok, rpc, symbol_uuid, McpHandler, Sheet, SPARSE, TOOL};
use serde_json::{json, Value};
use std::fs;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

#[derive(Debug, PartialEq)]
struct SavedInput {
    files: Vec<(PathBuf, Vec<u8>)>,
    modified: SystemTime,
    #[cfg(unix)]
    inode: (u64, u64),
}

fn saved_input(sheet: &Sheet) -> SavedInput {
    let metadata = fs::metadata(&sheet.path).unwrap();
    SavedInput {
        files: sheet.files(),
        modified: metadata.modified().unwrap(),
        #[cfg(unix)]
        inode: {
            use std::os::unix::fs::MetadataExt;
            (metadata.dev(), metadata.ino())
        },
    }
}

async fn read_only_call(
    handler: &McpHandler,
    tool: &str,
    args: Value,
    inputs: &[&Sheet],
) -> (bool, Value) {
    let before: Vec<_> = inputs.iter().map(|sheet| saved_input(sheet)).collect();
    let result = rpc(handler, tool, args).await;
    let after: Vec<_> = inputs.iter().map(|sheet| saved_input(sheet)).collect();
    assert_eq!(after, before, "{tool} changed a saved input or sidecar");
    result
}

async fn assert_blocked(before: &Sheet, after: &Sheet, approvals: &[String], reasons: &[String]) {
    let (error, result) = read_only_call(
        &handler().await,
        TOOL,
        json!({
            "baseline": before.path,
            "schematic": after.path,
            "removed_power_symbol_uuids": approvals,
        }),
        &[before, after],
    )
    .await;
    assert!(!error, "{result}");
    assert_verdict(&result, "blocked");
    assert_eq!(result["reasons"], json!(reasons), "{result}");
}

fn replace_once(before: &Sheet, old: &str, new: &str) -> Sheet {
    // Build one-field gate inputs from the saved fixture. Some deliberately
    // have missing or duplicate fields and cannot be produced by the writer.
    let after = before.fork();
    let source = String::from_utf8(before.bytes()).unwrap();
    assert_eq!(source.matches(old).count(), 1, "{old}");
    fs::write(&after.path, source.replacen(old, new, 1)).unwrap();
    after
}

#[tokio::test]
async fn a_power_symbol_still_present_cannot_be_approved_for_removal() {
    let before = Sheet::input("power_orientation_kicad10/power_orientation.kicad_sch");
    let after = before.fork();
    let uuid = symbol_uuid(&before, "#PWR03");
    assert_blocked(
        &before,
        &after,
        std::slice::from_ref(&uuid),
        &[format!("{uuid} is not a removed power symbol")],
    )
    .await;
}

#[tokio::test]
async fn adding_a_placed_symbol_reports_its_identity() {
    let after = Sheet::input(SPARSE);
    let uuid = symbol_uuid(&after, "R1");
    let before = after.fork();
    ok(
        &handler().await,
        &before,
        "batch_delete",
        json!({"uuids": [uuid]}),
    )
    .await;
    assert_blocked(
        &before,
        &after,
        &[],
        &[
            format!("symbol {uuid} was added"),
            "the move changes which pins the sheet places".into(),
        ],
    )
    .await;
}

#[tokio::test]
async fn replacing_a_symbol_uuid_reports_both_changed_identities() {
    let before = Sheet::input(SPARSE);
    let uuid = symbol_uuid(&before, "R1");
    let replacement = "915d8b53-de33-4f21-ad28-ef7af5dce6f4";
    let after = replace_once(
        &before,
        &format!("(uuid \"{uuid}\")"),
        &format!("(uuid \"{replacement}\")"),
    );
    assert_blocked(
        &before,
        &after,
        &[],
        &[
            format!("symbol {uuid} was removed"),
            format!("symbol {replacement} was added"),
            "the move changes which pins the sheet places".into(),
        ],
    )
    .await;
}

async fn assert_invalid_identity(before: &Sheet, after: &Sheet) {
    assert_blocked(
        before,
        after,
        &[],
        &["a placed symbol has missing or duplicate identity".into()],
    )
    .await;
}

#[tokio::test]
async fn a_placed_symbol_without_a_uuid_is_refused() {
    let before = Sheet::input(SPARSE);
    let uuid = symbol_uuid(&before, "R1");
    let after = replace_once(&before, &format!("(uuid \"{uuid}\")"), "");
    assert_invalid_identity(&before, &after).await;
}

#[tokio::test]
async fn a_placed_symbol_without_a_library_id_is_refused() {
    let before = Sheet::input(SPARSE);
    let after = replace_once(
        &before,
        "(lib_id \"Device:R\")\n\t\t(at 38.1 38.1 0)",
        "(at 38.1 38.1 0)",
    );
    assert_invalid_identity(&before, &after).await;
}

#[tokio::test]
async fn two_placed_symbols_with_one_uuid_are_refused() {
    let before = Sheet::input(SPARSE);
    let first = symbol_uuid(&before, "R1");
    let second = symbol_uuid(&before, "R2");
    let after = replace_once(
        &before,
        &format!("(uuid \"{second}\")"),
        &format!("(uuid \"{first}\")"),
    );
    assert_invalid_identity(&before, &after).await;
}

#[tokio::test]
async fn a_library_id_change_without_a_pin_change_is_refused() {
    let before = Sheet::input(SPARSE);
    let uuid = symbol_uuid(&before, "R1");
    let after = replace_once(
        &before,
        "(lib_id \"Device:R\")\n\t\t(at 38.1 38.1 0)",
        "(lib_id \"Other:R\") (lib_name \"Device:R\")\n\t\t(at 38.1 38.1 0)",
    );
    assert_blocked(
        &before,
        &after,
        &[],
        &[format!("symbol {uuid} changed its lib_id or instances")],
    )
    .await;
}

#[tokio::test]
async fn an_instance_change_without_a_pin_change_is_refused() {
    let before = Sheet::input(SPARSE);
    let source = String::from_utf8(before.bytes()).unwrap();
    let marker = "(instances\n";
    let offset = source.find(marker).unwrap();
    let after = before.fork();
    let reason = "changed its lib_id or instances";
    let tree = before.tree();
    let first = tree
        .find_all("symbol")
        .into_iter()
        .find(|placement| placement.find("instances").is_some())
        .unwrap();
    let uuid = first.find_str("uuid").unwrap();
    let mut changed = source;
    changed.insert_str(offset + marker.len(), "(project \"extra\")\n");
    fs::write(&after.path, changed).unwrap();
    assert_blocked(&before, &after, &[], &[format!("symbol {uuid} {reason}")]).await;
}

async fn assert_file_error(
    good: &Sheet,
    bad: &Path,
    kind: &str,
    reason: Option<&str>,
    inputs: &[&Sheet],
) {
    let handler = handler().await;
    for (schematic, baseline) in [(bad, good.path.as_path()), (good.path.as_path(), bad)] {
        let (error, result) = read_only_call(
            &handler,
            TOOL,
            json!({"schematic": schematic, "baseline": baseline}),
            inputs,
        )
        .await;
        assert!(error, "{result}");
        assert_eq!(result["error"]["kind"], kind, "{result}");
        assert_eq!(result["error"]["path"], json!(bad), "{result}");
        if let Some(reason) = reason {
            assert_eq!(result["error"]["reason"], reason, "{result}");
        } else if kind == "malformed_file" {
            assert!(
                !result["error"]["reason"].as_str().unwrap().is_empty(),
                "{result}"
            );
        }
    }
}

async fn assert_malformed(bytes: &[u8], reason: Option<&str>) {
    let good = Sheet::input(SPARSE);
    let bad = good.fork();
    fs::write(&bad.path, bytes).unwrap();
    assert_file_error(&good, &bad.path, "malformed_file", reason, &[&good, &bad]).await;
}

#[tokio::test]
async fn a_versioned_board_is_not_a_schematic() {
    assert_malformed(
        b"(kicad_pcb (version 20260206))",
        Some("expected a KiCad schematic sheet"),
    )
    .await;
}

#[tokio::test]
async fn a_schematic_header_without_a_version_is_refused() {
    assert_malformed(b"(kicad_sch)", Some("expected a KiCad schematic sheet")).await;
}

#[tokio::test]
async fn an_incomplete_s_expression_names_the_failed_path() {
    assert_malformed(b"(kicad_sch", None).await;
}

#[tokio::test]
async fn a_non_utf8_sheet_names_the_failed_path() {
    assert_malformed(b"\xff", None).await;
}

#[tokio::test]
async fn a_missing_sheet_names_the_failed_path() {
    let good = Sheet::input(SPARSE);
    let missing = good.path.with_file_name("missing.kicad_sch");
    assert_file_error(&good, &missing, "file_not_found", None, &[&good]).await;
}

#[tokio::test]
async fn a_directory_in_place_of_a_sheet_names_the_failed_path() {
    let good = Sheet::input(SPARSE);
    let directory = good.dir.path();
    assert_file_error(&good, directory, "file_not_found", None, &[&good]).await;
}

async fn assert_invalid_argument(
    args: Value,
    inputs: &[&Sheet],
    field: &str,
    reason: Option<&str>,
) {
    let (error, result) = read_only_call(&handler().await, TOOL, args, inputs).await;
    assert!(error, "{result}");
    assert_eq!(result["error"]["kind"], "invalid_argument", "{result}");
    assert_eq!(result["error"]["field"], field, "{result}");
    if let Some(reason) = reason {
        assert_eq!(result["error"]["reason"], reason, "{result}");
    }
}

#[tokio::test]
async fn a_non_string_baseline_is_refused() {
    let sheet = Sheet::input(SPARSE);
    assert_invalid_argument(
        json!({"schematic": sheet.path, "baseline": 123}),
        &[&sheet],
        "baseline",
        None,
    )
    .await;
}

#[tokio::test]
async fn a_missing_schematic_argument_is_refused() {
    let baseline = Sheet::input(SPARSE);
    assert_invalid_argument(
        json!({"baseline": baseline.path}),
        &[&baseline],
        "schematic",
        Some("missing"),
    )
    .await;
}

#[tokio::test]
async fn a_non_string_schematic_argument_is_refused() {
    let baseline = Sheet::input(SPARSE);
    assert_invalid_argument(
        json!({"schematic": 123, "baseline": baseline.path}),
        &[&baseline],
        "schematic",
        None,
    )
    .await;
}

#[tokio::test]
async fn a_non_list_removal_approval_argument_is_refused() {
    let before = Sheet::input(SPARSE);
    let after = before.fork();
    assert_invalid_argument(
        json!({
            "baseline": before.path,
            "schematic": after.path,
            "removed_power_symbol_uuids": "not an array",
        }),
        &[&before, &after],
        "removed_power_symbol_uuids",
        None,
    )
    .await;
}

#[tokio::test]
async fn nonempty_removal_approvals_require_a_baseline() {
    let sheet = Sheet::input(SPARSE);
    assert_invalid_argument(
        json!({"schematic": sheet.path, "removed_power_symbol_uuids": ["not-a-symbol"]}),
        &[&sheet],
        "removed_power_symbol_uuids",
        Some("requires a baseline"),
    )
    .await;
}

#[tokio::test]
async fn empty_removal_approvals_do_not_require_a_baseline() {
    let sheet = Sheet::input(SPARSE);
    let handler = handler().await;
    let (error, omitted) =
        read_only_call(&handler, TOOL, json!({"schematic": sheet.path}), &[&sheet]).await;
    assert!(!error, "{omitted}");
    let (error, explicit) = read_only_call(
        &handler,
        TOOL,
        json!({"schematic": sheet.path, "removed_power_symbol_uuids": []}),
        &[&sheet],
    )
    .await;
    assert!(!error, "{explicit}");
    assert_eq!(explicit, omitted);
    assert_eq!(explicit.get("baseline_score"), Some(&Value::Null));
    assert_eq!(explicit.get("delta"), Some(&Value::Null));
    assert_eq!(explicit.get("verdict"), Some(&Value::Null));
    assert_eq!(explicit.get("reasons"), Some(&json!([])));
}

#[tokio::test]
async fn null_removal_approvals_do_not_require_a_baseline() {
    let sheet = Sheet::input(SPARSE);
    let handler = handler().await;
    let (error, omitted) =
        read_only_call(&handler, TOOL, json!({"schematic": sheet.path}), &[&sheet]).await;
    assert!(!error, "{omitted}");
    let (error, result) = read_only_call(
        &handler,
        TOOL,
        json!({"schematic": sheet.path, "removed_power_symbol_uuids": null}),
        &[&sheet],
    )
    .await;
    assert!(!error, "{result}");
    assert_eq!(result, omitted);
}
