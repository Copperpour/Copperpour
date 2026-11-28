use super::super::tests::{CARRIED_NO_CONNECT, REDUNDANT_POWER};
use super::*;
use crate::mcp::handler::McpHandler;
use crate::tools::project_file_writer_tests::{served_call, text_of};
use crate::tools::sch_wiring::draw_route_with_drawn_pins;
use crate::tools::ServerConfig;
use copperpour_sexp::schematic::{extract_wires, parse_at, symbol_instance};
use copperpour_sexp::writer::{find_direct_child_blocks, test_hooks};
use copperpour_sexp::{apply_edits, SexpEdit, SexpNode};
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::PathBuf;
use std::sync::Mutex;

const FIXTURES: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/tests/fixtures/region_cleanup_kicad10"
);
const PREVIEW: &str = "plan_schematic_cleanup";

fn fixture(name: &str, extension: &str) -> String {
    fs::read_to_string(format!("{FIXTURES}/{name}/{name}.{extension}")).unwrap()
}

struct SavedSheet {
    _dir: tempfile::TempDir,
    path: PathBuf,
    original: String,
    handler: McpHandler,
}

impl SavedSheet {
    async fn new(name: &str) -> Self {
        Self::from_source(name, fixture(name, "kicad_sch")).await
    }

    async fn from_source(name: &str, original: String) -> Self {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(format!("{name}.kicad_sch"));
        fs::write(&path, &original).unwrap();
        fs::write(path.with_extension("kicad_pro"), fixture(name, "kicad_pro")).unwrap();
        let handler = McpHandler::new(ServerConfig {
            auto_load_toolsets: true,
            ..ServerConfig::default()
        })
        .await
        .unwrap();
        Self {
            _dir: dir,
            path,
            original,
            handler,
        }
    }

    fn args(&self, movable: &[&str]) -> Value {
        json!({
            "schematic": self.path, "x1": 0, "y1": 0, "x2": 200, "y2": 200, "movable": movable
        })
    }

    fn written(&self) -> String {
        fs::read_to_string(&self.path).unwrap()
    }

    fn set_source(&mut self, source: String) {
        fs::write(&self.path, &source).unwrap();
        self.original = source;
    }

    async fn preview(&self, movable: &[&str]) -> (Value, Value) {
        let args = self.args(movable);
        let before = self.written();
        let result = served_call(&self.handler, PREVIEW, args.clone()).await;
        assert!(!result.is_error, "{}", text_of(&result));
        let preview: Value = serde_json::from_str(text_of(&result)).unwrap();
        assert_eq!(self.written(), before);
        let mut apply = args;
        apply["revision"] = preview["revision"].clone();
        (preview, apply)
    }

    async fn apply(&self, args: Value) -> (CallToolResult, Value) {
        let result = served_call(&self.handler, TOOL, args).await;
        let body = serde_json::from_str(text_of(&result)).unwrap();
        (result, body)
    }

    async fn refuses(&self, args: Value, expected: &str) -> Value {
        let (result, body) = self.apply(args).await;
        assert_eq!(body["error"]["kind"], expected, "{body}");
        assert!(result.is_error, "{body}");
        body["error"].clone()
    }

    fn arm_fault(&self, fault: Fault) {
        FAULTS.lock().unwrap().insert(self.path.clone(), fault);
    }
}

fn wires(tree: &SexpNode) -> BTreeMap<String, [(i64, i64); 2]> {
    extract_wires(tree)
        .into_iter()
        .map(|wire| {
            let pos = |x, y| super::super::schematic_point(x, y);
            let mut segment = [pos(wire.x1, wire.y1), pos(wire.x2, wire.y2)];
            segment.sort_unstable();
            (wire.uuid.unwrap(), segment)
        })
        .collect()
}

fn symbols(tree: &SexpNode) -> BTreeMap<String, &SexpNode> {
    tree.find_all("symbol")
        .into_iter()
        .map(|node| (symbol_instance(node).unwrap().reference, node))
        .collect()
}

#[tokio::test]
async fn crowded_placement_writes_only_the_previewed_items_then_noops() {
    assert_crowded_apply(&["C1"], &[(114.3, 76.2), (119.38, 76.2)]).await;
}

#[tokio::test]
async fn crowded_reroute_writes_only_the_previewed_items_then_noops() {
    assert_crowded_apply(&[], &[(63.5, 105.41), (119.38, 105.41), (119.38, 76.2)]).await;
}

async fn assert_crowded_apply(movable: &[&str], expected_path: &[(f64, f64)]) {
    let sheet = SavedSheet::new("crowded").await;
    let (preview, args) = sheet.preview(movable).await;
    let (result, applied) = sheet.apply(args).await;
    assert!(!result.is_error, "{applied}");
    assert_eq!(applied["unchanged"], false, "{applied}");
    assert_eq!(applied["source"], "saved_schematic");
    assert_eq!(applied["steps"], preview["steps"]);
    assert_eq!(applied["steps"].as_array().unwrap().len(), 1);
    let step = &applied["steps"][0];
    assert_eq!(step["route"], json!(expected_path));
    assert_ne!(applied["revision"], preview["revision"]);
    let source = sheet.written();
    assert!(
        !source
            .lines()
            .any(|line| !line.is_empty() && line.bytes().all(|b| b == b'\t')),
        "removed blocks left tab-only lines in the saved schematic"
    );
    assert_eq!(
        applied["revision"],
        DocumentRevision::of(&source).to_string()
    );
    let before = parse_sexp(&sheet.original).unwrap();
    let after = parse_sexp(&source).unwrap();
    let (was, now) = (wires(&before), wires(&after));
    let removed: BTreeSet<_> = was.keys().filter(|id| !now.contains_key(*id)).collect();
    let added: Vec<_> = now.keys().filter(|id| !was.contains_key(*id)).collect();
    assert_eq!(removed.len(), 6);
    let reported = |key: &str| {
        step[key]
            .as_array()
            .unwrap()
            .iter()
            .map(|value| value.as_str().unwrap())
            .collect::<BTreeSet<_>>()
    };
    assert_eq!(
        removed
            .iter()
            .map(|id| id.as_str())
            .collect::<BTreeSet<_>>(),
        reported("wires_removed")
    );
    assert_eq!(
        added.iter().map(|id| id.as_str()).collect::<BTreeSet<_>>(),
        reported("wire_uuids_added")
    );
    let mut segments: Vec<_> = added.iter().map(|id| now[*id]).collect();
    segments.sort_unstable();
    let mut expected_segments = expected_path
        .windows(2)
        .map(|pair| {
            let mut segment: [(i64, i64); 2] =
                [0, 1].map(|i| super::super::schematic_point(pair[i].0, pair[i].1));
            segment.sort_unstable();
            segment
        })
        .collect::<Vec<_>>();
    expected_segments.sort_unstable();
    assert_eq!(segments, expected_segments);
    for (id, segment) in &was {
        if !removed.contains(id) {
            assert_eq!(now.get(id), Some(segment));
        }
    }
    let junctions: Vec<_> = after
        .find_all("junction")
        .into_iter()
        .map(|dot| (dot.find_str("uuid").unwrap(), parse_at(dot).unwrap()))
        .collect();
    assert_eq!(junctions.len(), 1, "{junctions:?}");
    assert_eq!(junctions[0].1, (119.38, 76.2, 0.0));
    assert_eq!(step["junction_uuids_added"], json!([junctions[0].0]));
    assert_eq!(step["junctions_added"], json!([[119.38, 76.2]]));
    assert_eq!(step["junctions_removed"], json!([]));
    let (was, now) = (symbols(&before), symbols(&after));
    assert_eq!(
        was.keys().collect::<Vec<_>>(),
        now.keys().collect::<Vec<_>>()
    );
    let moved = if movable.is_empty() {
        BTreeMap::new()
    } else {
        BTreeMap::from([("C1", (114.3, 80.01)), ("#PWR05", (114.3, 83.82))])
    };
    for (reference, node) in &was {
        let readback = now[reference];
        if let Some(&(x, y)) = moved.get(reference.as_str()) {
            assert_eq!(parse_at(readback).map(|at| (at.0, at.1)), Some((x, y)));
            assert_eq!(node.find("uuid"), readback.find("uuid"));
            assert_eq!(node.find("instances"), readback.find("instances"));
        } else {
            assert_eq!(*node, readback, "{reference}");
        }
    }
    for tag in ["label", "global_label", "hierarchical_label"] {
        assert_eq!(before.find_all(tag), after.find_all(tag));
    }

    let (second_preview, second_args) = sheet.preview(movable).await;
    assert_eq!(second_preview["revision"], applied["revision"]);
    assert_eq!(second_preview["steps"], json!([]));
    assert_no_write(&sheet, second_args, &source).await;
}

async fn assert_no_write(sheet: &SavedSheet, args: Value, source: &str) {
    let revision = args["revision"].clone();
    // A byte-identical rewrite would still hit the writer's lock check.
    // Use an existing editor lock so the test detects reaching the writer.
    let lock = copperpour_sexp::writer::kicad_editor_lock_path(&sheet.path).unwrap();
    fs::write(lock, "").unwrap();
    let (result, applied) = sheet.apply(args).await;
    assert_eq!(applied["unchanged"], true, "{applied}");
    assert!(!result.is_error);
    assert_eq!(applied["steps"], json!([]));
    assert_eq!(applied["revision"], revision);
    assert_eq!(sheet.written(), source);
}

#[tokio::test]
async fn constrained_plan_does_not_disconnect_c1_or_write() {
    let sheet = SavedSheet::new("constrained").await;
    let (preview, args) = sheet.preview(&[]).await;
    assert_eq!(preview["steps"], json!([]));
    assert_no_write(&sheet, args, &sheet.original).await;
}

#[tokio::test]
async fn apply_carries_the_same_no_connect_to_c1s_new_pin_position() {
    let sheet = SavedSheet::from_source("crowded", CARRIED_NO_CONNECT.to_owned()).await;
    let (preview, args) = sheet.preview(&["C1"]).await;
    let (result, applied) = sheet.apply(args).await;
    assert!(!result.is_error, "{applied}");
    assert_eq!(applied["steps"], preview["steps"]);
    let before = parse_sexp(&sheet.original).unwrap();
    let observed = parse_sexp(&sheet.written()).unwrap();
    let [old]: [&SexpNode; 1] = before.find_all("no_connect").try_into().unwrap();
    let [new]: [&SexpNode; 1] = observed.find_all("no_connect").try_into().unwrap();
    assert_eq!(old.find("uuid"), new.find("uuid"));
    assert_eq!(parse_at(old), Some((63.5, 113.03, 0.0)));
    assert_eq!(parse_at(new), Some((114.3, 83.82, 0.0)));
}

#[tokio::test]
async fn apply_removes_the_redundant_symbol_wire_and_junction() {
    let sheet = SavedSheet::from_source("crowded", REDUNDANT_POWER.to_owned()).await;
    let before = parse_sexp(&sheet.original).unwrap();
    let removed_dot = redundant_stub_dot(&before);
    let (preview, args) = sheet.preview(&["#PWR99"]).await;
    let (result, applied) = sheet.apply(args).await;
    assert!(!result.is_error, "{applied}");
    assert_eq!(applied["steps"], preview["steps"]);
    let removal = applied["steps"]
        .as_array()
        .unwrap()
        .iter()
        .find(|step| step["junctions_removed"] == json!([removed_dot]))
        .expect("the plan removes the redundant stub's dot");
    let observed = parse_sexp(&sheet.written()).unwrap();
    let removed_wire = removal["wires_removed"][0].as_str().unwrap();
    let point = super::super::schematic_point;
    assert_eq!(
        wires(&before).get(removed_wire),
        Some(&[point(139.7, 68.58), point(139.7, 76.2)])
    );
    assert!(!wires(&observed).contains_key(removed_wire));
    assert!(symbols(&before).contains_key("#PWR99"));
    assert!(!symbols(&observed).contains_key("#PWR99"));
    assert_eq!(junction_at(&observed, removed_dot), None);
    let (_, args) = sheet.preview(&["#PWR99"]).await;
    assert_eq!(args["revision"], applied["revision"]);
    assert_no_write(&sheet, args, &sheet.written()).await;
}

#[tokio::test]
async fn readback_detects_a_junction_left_behind_by_a_removal() {
    let sheet = SavedSheet::from_source("crowded", REDUNDANT_POWER.to_owned()).await;
    let before = parse_sexp(&sheet.original).unwrap();
    let removed_dot = redundant_stub_dot(&before);
    let (_, args) = sheet.preview(&["#PWR99"]).await;
    let dot = find_direct_child_blocks(&sheet.original, "kicad_sch")
        .into_iter()
        .map(|(start, end)| &sheet.original[start..end])
        .find(|block| {
            let node = parse_sexp(block).unwrap();
            node.head() == Some("junction") && node.find_str("uuid") == Some(removed_dot)
        })
        .unwrap()
        .to_owned();
    sheet.arm_fault(Fault::RetainDot(dot));
    let observed = assert_uncertain_readback(&sheet, args).await;
    assert!(!symbols(&observed).contains_key("#PWR99"));
    assert_eq!(
        junction_at(&observed, removed_dot),
        Some((139.7, 76.2, 0.0))
    );
}

fn redundant_stub_dot(tree: &SexpNode) -> &str {
    tree.find_all("junction")
        .into_iter()
        .find(|dot| parse_at(dot) == Some((139.7, 76.2, 0.0)))
        .and_then(|dot| dot.find_str("uuid"))
        .expect("the input has a dot at the redundant stub's tip")
}

fn junction_at(tree: &SexpNode, uuid: &str) -> Option<(f64, f64, f64)> {
    tree.find_all("junction")
        .into_iter()
        .find(|dot| dot.find_str("uuid") == Some(uuid))
        .and_then(parse_at)
}

#[cfg(unix)]
#[tokio::test]
async fn apply_refuses_an_alias_without_replacing_the_symlink_or_its_target() {
    let mut sheet = SavedSheet::new("crowded").await;
    let real = sheet.path.clone();
    sheet.path = real.with_file_name("alias.kicad_sch");
    std::os::unix::fs::symlink(&real, &sheet.path).unwrap();

    let (preview, args) = sheet.preview(&[]).await;
    assert_eq!(preview["steps"].as_array().unwrap().len(), 1);
    let refusal = sheet.refuses(args, "invalid_argument").await;
    assert_eq!(refusal["field"], "schematic");
    assert_eq!(fs::read_link(&sheet.path).unwrap(), real);
    assert_eq!(fs::read_to_string(real).unwrap(), sheet.original);
}

#[tokio::test]
async fn apply_refuses_a_directory_as_the_schematic() {
    let sheet = SavedSheet::new("crowded").await;
    let (_, mut args) = sheet.preview(&[]).await;
    let directory = sheet.path.with_file_name("directory.kicad_sch");
    fs::create_dir(&directory).unwrap();
    fs::write(directory.join("sentinel"), b"preserve").unwrap();
    args["schematic"] = json!(directory);
    let refusal = sheet.refuses(args, "invalid_argument").await;
    assert_eq!(refusal["field"], "schematic");
    assert_eq!(fs::read(directory.join("sentinel")).unwrap(), b"preserve");
    assert_eq!(fs::read_dir(directory).unwrap().count(), 1);
    assert_eq!(sheet.written(), sheet.original);
}

#[cfg(unix)]
#[tokio::test]
async fn a_same_byte_symlink_swapped_in_before_rename_is_a_conflict() {
    let sheet = SavedSheet::new("crowded").await;
    let (_, args) = sheet.preview(&[]).await;
    let real = sheet.path.with_file_name("other.kicad_sch");
    fs::copy(&sheet.path, &real).unwrap();
    test_hooks::link_before_final_check(&sheet.path, &real);

    let refusal = sheet.refuses(args, "conflict").await;
    assert_eq!(refusal["paths"], json!([sheet.path]));
    assert_eq!(fs::read_link(&sheet.path).unwrap(), real);
    assert_eq!(fs::read_to_string(real).unwrap(), sheet.original);
    assert!(!test_hooks::armed(&sheet.path));
}

#[tokio::test]
async fn a_target_deleted_before_rename_is_not_recreated() {
    let sheet = SavedSheet::new("crowded").await;
    let (_, args) = sheet.preview(&[]).await;
    test_hooks::remove_before_final_check(&sheet.path);
    let refusal = sheet.refuses(args, "conflict").await;
    assert_eq!(refusal["paths"], json!([sheet.path]));
    assert!(!test_hooks::armed(&sheet.path));
    assert_eq!(
        fs::symlink_metadata(&sheet.path).unwrap_err().kind(),
        std::io::ErrorKind::NotFound
    );
    assert_eq!(
        fs::read_dir(sheet.path.parent().unwrap()).unwrap().count(),
        1
    );
}

#[tokio::test]
async fn a_directory_swapped_in_before_rename_is_a_conflict() {
    let sheet = SavedSheet::new("crowded").await;
    let (_, args) = sheet.preview(&[]).await;
    test_hooks::directory_before_final_check(&sheet.path, "preserve");
    let refusal = sheet.refuses(args, "conflict").await;
    assert_eq!(refusal["paths"], json!([sheet.path]));
    assert!(!test_hooks::armed(&sheet.path));
    assert_eq!(fs::read(sheet.path.join("sentinel")).unwrap(), b"preserve");
    assert_eq!(fs::read_dir(&sheet.path).unwrap().count(), 1);
    assert_eq!(
        fs::read_dir(sheet.path.parent().unwrap()).unwrap().count(),
        2
    );
}

#[tokio::test]
async fn a_matching_revision_does_not_make_a_duplicate_item_uuid_valid() {
    let mut sheet = SavedSheet::new("crowded").await;
    let (start, end) = find_direct_child_blocks(&sheet.original, "kicad_sch")
        .into_iter()
        .find(|&(start, end)| {
            parse_sexp(&sheet.original[start..end]).unwrap().head() == Some("wire")
        })
        .unwrap();
    let at = sheet.original.rfind(')').unwrap();
    sheet.set_source(format!(
        "{}\n{}\n{}",
        &sheet.original[..at],
        &sheet.original[start..end],
        &sheet.original[at..]
    ));
    let mut args = sheet.args(&[]);
    args["revision"] = json!(DocumentRevision::of(&sheet.original).to_string());
    let refusal = sheet.refuses(args, "malformed_file").await;
    assert_eq!(refusal["path"], json!(sheet.path));
    assert!(refusal["reason"].as_str().unwrap().contains("unique UUID"));
    assert_eq!(sheet.written(), sheet.original);
}

#[tokio::test]
async fn stale_revision_refuses_before_planning_even_if_the_new_sheet_is_malformed() {
    let sheet = SavedSheet::new("crowded").await;
    let (_, args) = sheet.preview(&[]).await;
    let saved = fixture("constrained", "kicad_sch");
    fs::write(&sheet.path, &saved).unwrap();
    let refusal = sheet.refuses(args.clone(), "conflict").await;
    assert_eq!(refusal["paths"], json!([sheet.path]));
    assert_eq!(sheet.written(), saved);

    // Same bytes as the KiCad fixture, but a truncated concurrent save.
    let truncated = saved.lines().next().unwrap();
    fs::write(&sheet.path, truncated).unwrap();
    sheet.refuses(args, "conflict").await;
    assert_eq!(sheet.written(), truncated);
}

#[tokio::test]
async fn a_non_utf8_save_is_still_a_revision_conflict() {
    let sheet = SavedSheet::new("crowded").await;
    let (_, args) = sheet.preview(&[]).await;
    let saved = [0xff, 0xfe, 0x00];
    test_hooks::save_bytes_before_final_check(&sheet.path, saved.to_vec());
    let refusal = sheet.refuses(args.clone(), "conflict").await;
    assert_eq!(refusal["paths"], json!([sheet.path]));
    assert!(!test_hooks::armed(&sheet.path));
    assert_eq!(fs::read(&sheet.path).unwrap(), saved);
    sheet.refuses(args, "conflict").await;
    assert_eq!(fs::read(&sheet.path).unwrap(), saved);
}

#[tokio::test]
async fn a_save_before_the_first_comparison_is_preserved() {
    assert_save_is_preserved(test_hooks::save_before_compare).await;
}

#[tokio::test]
async fn a_save_before_the_final_comparison_is_preserved() {
    assert_save_is_preserved(test_hooks::save_before_final_check).await;
}

async fn assert_save_is_preserved(arm_save: impl FnOnce(&Path, String)) {
    let sheet = SavedSheet::new("crowded").await;
    let (_, args) = sheet.preview(&[]).await;
    let saved = fixture("constrained", "kicad_sch");
    arm_save(&sheet.path, saved.clone());
    let refusal = sheet.refuses(args, "conflict").await;
    assert_eq!(refusal["paths"], json!([sheet.path]));
    assert!(!test_hooks::armed(&sheet.path));
    assert_eq!(sheet.written(), saved);
}

#[tokio::test]
async fn kicad_editor_lock_refuses_nonempty_apply() {
    let sheet = SavedSheet::new("crowded").await;
    let (_, args) = sheet.preview(&[]).await;
    let lock = copperpour_sexp::writer::kicad_editor_lock_path(&sheet.path).unwrap();
    fs::write(lock, "").unwrap();
    let refusal = sheet.refuses(args, "conflict").await;
    assert_eq!(refusal["paths"], json!([sheet.path]));
    assert_eq!(sheet.written(), sheet.original);
}

#[tokio::test]
async fn apply_checks_pin_net_changes_in_the_final_candidate() {
    let sheet = SavedSheet::new("crowded").await;
    let (_, args) = sheet.preview(&[]).await;
    sheet.arm_fault(Fault::DropRoute);
    let refusal = sheet.refuses(args, "plan_blocked").await;
    assert_eq!(refusal["operation"], TOOL);
    assert_eq!(
        refusal["reasons"],
        json!([
            "the net of [#FLG01 pin 1, #PWR01 pin 1, #PWR07 pin 1, C1 pin 1, U1 pin 3] would lose [C1 pin 1]"
        ])
    );
    assert_eq!(sheet.written(), sheet.original);
}

#[tokio::test]
async fn apply_checks_foreign_joins_in_a_drawn_step() {
    let sheet = SavedSheet::new("crowded").await;
    let (_, args) = sheet.preview(&[]).await;
    sheet.arm_fault(Fault::JoinSigA);
    let refusal = sheet.refuses(args, "plan_blocked").await;
    assert_eq!(refusal["operation"], TOOL);
    assert_eq!(
        refusal["reasons"],
        json!([
            "J1 pin 1 at (71.12, 81.28) on net SIG_A",
            "global label SIG_A at (83.82, 81.28) on net SIG_A"
        ])
    );
    assert_eq!(sheet.written(), sheet.original);
}

#[tokio::test]
async fn apply_refuses_a_step_whose_route_geometry_cannot_be_checked() {
    let sheet = SavedSheet::new("crowded").await;
    let project = fs::read(sheet.path.with_extension("kicad_pro")).unwrap();
    let (preview, args) = sheet.preview(&[]).await;
    assert_eq!(preview["steps"].as_array().unwrap().len(), 1);
    sheet.arm_fault(Fault::ShallowDiagonal);
    let refusal = sheet.refuses(args, "plan_blocked").await;
    assert_eq!(refusal["operation"], TOOL);
    assert_eq!(
        refusal["reasons"],
        json!(["the cleanup route cannot be checked on a sheet with a near-axis diagonal wire"])
    );
    assert_eq!(sheet.written(), sheet.original);
    assert_eq!(
        fs::read(sheet.path.with_extension("kicad_pro")).unwrap(),
        project
    );
}

#[tokio::test]
async fn unfinished_plan_is_blocked_without_writing_a_prefix() {
    let sheet = SavedSheet::new("crowded").await;
    let (_, args) = sheet.preview(&["C1", "#PWR05"]).await;
    sheet.arm_fault(Fault::StepLimit(1));
    let refusal = sheet.refuses(args, "plan_blocked").await;
    assert_eq!(
        refusal["reasons"],
        json!(["the region still has cleanup steps after 1 edits; select a smaller region"])
    );
    assert_eq!(sheet.written(), sheet.original);
}

#[tokio::test]
async fn writer_readback_mismatch_is_uncertain_not_a_refusal() {
    let sheet = SavedSheet::new("crowded").await;
    let (_, args) = sheet.preview(&[]).await;
    let saved = fixture("constrained", "kicad_sch");
    test_hooks::save_after_replace(&sheet.path, saved.clone());
    let refusal = sheet.refuses(args, "mutation_outcome_uncertain").await;
    assert_eq!(refusal["operation"], TOOL);
    assert_eq!(refusal["path"], json!(sheet.path));
    assert!(!test_hooks::armed(&sheet.path));
    assert_eq!(sheet.written(), saved);
}

#[tokio::test]
async fn directory_sync_failure_after_rename_is_uncertain() {
    let sheet = SavedSheet::new("crowded").await;
    let (_, args) = sheet.preview(&[]).await;
    test_hooks::fail_directory_sync_after_replace(&sheet.path);
    let refusal = sheet.refuses(args, "mutation_outcome_uncertain").await;
    assert_eq!(refusal["operation"], TOOL);
    assert_eq!(refusal["path"], json!(sheet.path));
    assert!(refusal["reason"]
        .as_str()
        .unwrap()
        .contains("directory sync or readback"));
    assert_ne!(sheet.written(), sheet.original);
    assert!(!test_hooks::armed(&sheet.path));
}

#[tokio::test]
async fn the_existing_add_wire_path_also_reports_post_rename_sync_failure() {
    let sheet = SavedSheet::new("crowded").await;
    test_hooks::fail_directory_sync_after_replace(&sheet.path);
    let result = served_call(
        &sheet.handler,
        "add_wire",
        json!({"schematic": sheet.path, "x1": 12.7, "y1": 12.7, "x2": 25.4, "y2": 12.7}),
    )
    .await;
    let body: Value = serde_json::from_str(text_of(&result)).unwrap();
    assert!(result.is_error, "{body}");
    assert_eq!(
        body["error"]["kind"], "mutation_outcome_uncertain",
        "{body}"
    );
    assert_eq!(body["error"]["operation"], "add_wire");
    assert_eq!(body["error"]["path"], json!(sheet.path));
    let before = parse_sexp(&sheet.original).unwrap();
    let after = parse_sexp(&sheet.written()).unwrap();
    assert_eq!(
        extract_wires(&after).len(),
        extract_wires(&before).len() + 1
    );
    assert!(!test_hooks::armed(&sheet.path));
}

#[cfg(unix)]
#[tokio::test]
async fn add_wire_refuses_an_existing_symlink_without_suggesting_a_retry() {
    let sheet = SavedSheet::new("crowded").await;
    let alias = sheet.path.with_file_name("alias.kicad_sch");
    std::os::unix::fs::symlink(&sheet.path, &alias).unwrap();
    let result = served_call(
        &sheet.handler,
        "add_wire",
        json!({"schematic": alias, "x1": 12.7, "y1": 12.7, "x2": 25.4, "y2": 12.7}),
    )
    .await;
    let body: Value = serde_json::from_str(text_of(&result)).unwrap();
    assert!(result.is_error, "{body}");
    assert_eq!(body["error"]["kind"], "invalid_write_target", "{body}");
    assert_eq!(body["error"]["path"], json!(alias), "{body}");
    assert!(
        body["error"]["reason"]
            .as_str()
            .is_some_and(|reason| reason.contains("use the actual file path, not a symbolic link")),
        "{body}"
    );
    assert!(body["message"].as_str().unwrap().contains("wrote nothing"));
    assert_eq!(fs::read_link(&alias).unwrap(), sheet.path);
    assert_eq!(sheet.written(), sheet.original);
}

#[tokio::test]
async fn readback_detects_a_missing_added_junction() {
    let sheet = SavedSheet::new("crowded").await;
    let (_, args) = sheet.preview(&["C1"]).await;
    sheet.arm_fault(Fault::LoseDot);
    let observed = assert_uncertain_readback(&sheet, args).await;
    assert_eq!(observed.find_all("junction").len(), 0);
    assert_eq!(
        parse_at(symbols(&observed)["C1"]),
        Some((114.3, 80.01, 0.0))
    );
}

async fn assert_uncertain_readback(sheet: &SavedSheet, args: Value) -> SexpNode {
    let refusal = sheet.refuses(args, "mutation_outcome_uncertain").await;
    assert_eq!(refusal["path"], json!(sheet.path));
    assert_eq!(refusal["operation"], TOOL);
    parse_sexp(&sheet.written()).unwrap()
}

#[test]
fn semantic_readback_does_not_certify_a_later_format_only_save() {
    let source = fixture("crowded", "kicad_sch");
    let tree = parse_sexp(&source).unwrap();
    let saved = format!("{source}\n");
    assert_eq!(parse_sexp(&saved).unwrap(), tree);
    assert_eq!(
        verified_revision(&tree, &tree, &[], &source, &saved),
        Err("a later save changed the written bytes".into())
    );
}

#[tokio::test]
async fn served_schema_rejects_a_plan_or_missing_revision_before_writing() {
    let sheet = SavedSheet::new("crowded").await;
    let args = sheet.args(&[]);
    let refusal = sheet.refuses(args, "invalid_argument").await;
    assert_eq!(refusal["field"], "revision");
    let (preview, mut args) = sheet.preview(&[]).await;
    args["steps"] = preview["steps"].clone();
    let refusal = sheet.refuses(args, "invalid_argument").await;
    assert_eq!(refusal["field"], "steps");
    assert_eq!(sheet.written(), sheet.original);
}

// The normal planner rejects unsafe candidates. Simulate a planner defect
// or an outside save on this test's copy without disabling any apply check.
#[derive(Default)]
pub(super) enum Fault {
    #[default]
    None,
    DropRoute,
    JoinSigA,
    ShallowDiagonal,
    LoseDot,
    RetainDot(String),
    StepLimit(usize),
}

static FAULTS: Mutex<BTreeMap<PathBuf, Fault>> = Mutex::new(BTreeMap::new());

pub(super) fn take_fault(path: &Path) -> Fault {
    FAULTS.lock().unwrap().remove(path).unwrap_or_default()
}

impl Fault {
    pub(super) fn write_candidate<'a>(&self, source: &'a str) -> std::borrow::Cow<'a, str> {
        match self {
            Self::LoseDot => {
                assert_eq!(parse_sexp(source).unwrap().find_all("junction").len(), 1);
                drop_blocks(source.to_owned(), "junction", |_| true).into()
            }
            Self::RetainDot(dot) => {
                let at = source.rfind(')').unwrap();
                format!("{}\n{dot}\n{}", &source[..at], &source[at..]).into()
            }
            _ => source.into(),
        }
    }

    pub(super) fn step_limit(&self, limit: usize) -> usize {
        match self {
            Self::StepLimit(limit) => *limit,
            _ => limit,
        }
    }

    pub(super) fn route_candidate<'a>(&self, source: &'a str) -> std::borrow::Cow<'a, str> {
        match self {
            Self::JoinSigA => draw_route_with_drawn_pins(
                source,
                &[(119.38, 76.2), (119.38, 81.28), (83.82, 81.28)],
            )
            .unwrap()
            .into(),
            Self::ShallowDiagonal => {
                // Copy the diagonal from the KiCad-resaved route witness into
                // the in-memory candidate, where the apply recheck sees it.
                let fixture = include_str!(
                    "../../tests/fixtures/label_attachment_kicad10/shallow-wire-route/opamp_stage.kicad_sch"
                );
                let diagonal = find_direct_child_blocks(fixture, "kicad_sch")
                    .into_iter()
                    .map(|(start, end)| &fixture[start..end])
                    .find(|block| {
                        let node = parse_sexp(block).unwrap();
                        node.head() == Some("wire") && {
                            let points = node.find("pts").unwrap().find_all("xy");
                            points[0].get_f64(1) != points[1].get_f64(1)
                                && points[0].get_f64(2) != points[1].get_f64(2)
                        }
                    })
                    .expect("the shallow-wire fixture contains a diagonal");
                let at = source.rfind(')').unwrap();
                format!("{}\n{diagonal}\n{}", &source[..at], &source[at..]).into()
            }
            _ => source.into(),
        }
    }

    pub(super) fn candidate(&self, source: String) -> String {
        match self {
            Self::DropRoute => drop_blocks(source, "wire", |node| {
                node.find("pts")
                    .unwrap()
                    .find_all("xy")
                    .iter()
                    .any(|xy| (xy.get_f64(1), xy.get_f64(2)) == (Some(63.5), Some(105.41)))
            }),
            _ => source,
        }
    }
}

fn drop_blocks(source: String, tag: &str, matches: impl Fn(&SexpNode) -> bool) -> String {
    let edits = find_direct_child_blocks(&source, "kicad_sch")
        .into_iter()
        .filter_map(|(start, end)| {
            let block = &source[start..end];
            let node = parse_sexp(block).unwrap();
            (node.head() == Some(tag) && matches(&node)).then(|| SexpEdit::delete(start, end))
        })
        .collect();
    apply_edits(source, edits)
}
