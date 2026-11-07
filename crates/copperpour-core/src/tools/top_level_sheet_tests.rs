//! A project whose `.kicad_pro` lists a second top-level sheet in
//! `schematic.top_level_sheets` (#130), and one that keys a top-level sheet to
//! the uuid its entry gives in place of the sheet's own (#125). The fixture is
//! described in `tests/fixtures/top_level_sheets_kicad10/README.md`; the list
//! itself, which no tool writes, is set by each test.

// `stock_reference_prefix_libraries` returns a guard over the process-wide
// `KICAD*_DIR` variables that the awaited calls read, so the tests hold it
// across their `.await`s, as `sch_components`' tests hold theirs.
#![allow(clippy::await_holding_lock)]

use super::ServerConfig;
use copperpour_schematic_editor as cse;
use copperpour_sexp::{parse_sexp, SexpNode};
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::sync::MutexGuard;
use tempfile::TempDir;

// Identities from the fixture.
const TOPLEVEL: &str = "b603aa20-aac9-4b43-91f9-885f100bae62";
const CHILD_SHEET: &str = "4e754337-e3f1-4d84-85ed-5ff5976e9393";
const SECOND: &str = "611adf86-3690-4960-a96b-31b4bdfb37a4";
const NIL: &str = "00000000-0000-0000-0000-000000000000";
/// Uuids a project gives its top-level sheets in place of their own.
const LISTED_TOPLEVEL: &str = "2f6f2d4e-7b1a-4c3e-9d58-0a1b2c3d4e5f";
const LISTED_SECOND: &str = "8e3b1c27-5d4a-4f60-a7c9-1e2d3c4b5a69";

struct Project {
    dir: TempDir,
    _env: MutexGuard<'static, ()>,
}

impl Project {
    /// The fixture beside a `sym-lib-table` resolving `Device:R`, its project
    /// listing `sheets` as its top-level sheets, or nothing when `None`.
    fn new(sheets: Option<Value>) -> Self {
        let (dir, env) = super::stock_reference_prefix_libraries();
        let source =
            Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/top_level_sheets_kicad10");
        for name in [
            "toplevel.kicad_pro",
            "toplevel.kicad_sch",
            "child.kicad_sch",
            "second.kicad_sch",
        ] {
            std::fs::copy(source.join(name), dir.path().join(name)).unwrap();
        }
        let project = Self { dir, _env: env };
        if let Some(sheets) = sheets {
            let path = project.file("toplevel.kicad_pro");
            let mut settings: Value =
                serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
            settings["schematic"]["top_level_sheets"] = sheets;
            std::fs::write(path, serde_json::to_string_pretty(&settings).unwrap()).unwrap();
        }
        project
    }

    fn file(&self, name: &str) -> PathBuf {
        self.dir.path().join(name)
    }

    /// Every sheet file's bytes, to show a refusal wrote nothing.
    fn saved(&self) -> Vec<(String, Vec<u8>)> {
        let mut files: Vec<_> = std::fs::read_dir(self.dir.path())
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .filter(|path| path.extension().is_some_and(|ext| ext == "kicad_sch"))
            .map(|path| {
                let name = path.file_name().unwrap().to_string_lossy().into_owned();
                (name, std::fs::read(&path).unwrap())
            })
            .collect();
        files.sort();
        files
    }
}

/// The project's two top-level sheets, keyed to `toplevel` and `second`.
fn both(toplevel: &str, second: &str) -> Value {
    json!([
        {"filename": "toplevel.kicad_sch", "name": "toplevel", "uuid": toplevel},
        {"filename": "second.kicad_sch", "name": "second", "uuid": second}
    ])
}

async fn served(tool: &str, arguments: Value) -> (bool, Value) {
    let handler = crate::mcp::handler::McpHandler::new(ServerConfig {
        kicad_cli: String::new(),
        kicad_binary: String::new(),
        ipc_address: String::new(),
        project_dir: None,
        jlcpcb_db_path: None,
        auto_load_toolsets: true,
        eager_toolsets: false,
    })
    .await
    .unwrap();
    let result = handler
        .handle_message(json!({"jsonrpc": "2.0", "id": 130, "method": "tools/call",
            "params": {"name": tool, "arguments": arguments}}))
        .await
        .unwrap()
        .result
        .unwrap();
    let text = result["content"][0]["text"].as_str().unwrap();
    let body = serde_json::from_str(text).unwrap_or_else(|_| json!(text));
    (result["isError"] == json!(true), body)
}

async fn place(schematic: &Path, reference: &str) -> (bool, Value) {
    served(
        "add_schematic_component",
        json!({"schematic": schematic, "lib_id": "Device:R", "reference": reference,
            "x": 76.2, "y": 76.2}),
    )
    .await
}

async fn add_sheet(parent: &Path, file: &str, name: &str) -> (bool, Value) {
    add_sheet_at(parent, file, name, 152.4).await
}

async fn add_sheet_at(parent: &Path, file: &str, name: &str, x: f64) -> (bool, Value) {
    served(
        "add_hierarchical_sheet",
        json!({"schematic": parent, "sheet_file": file, "sheet_name": name,
            "x": x, "y": 38.1, "width": 40, "height": 25}),
    )
    .await
}

fn sheet_uuid(file: &Path, name: &str) -> String {
    cse::Schematic::load(file)
        .unwrap()
        .sheets
        .by_name(name)
        .unwrap()
        .uuid
        .clone()
}

/// Every `(project, path)` the symbol `reference` in `file` records, sorted,
/// read from the raw file rather than the editor model the code uses.
fn recorded(file: &Path, reference: &str) -> Vec<(String, String)> {
    let tree = parse_sexp(&std::fs::read_to_string(file).unwrap()).unwrap();
    let symbol = tree
        .find_all("symbol")
        .into_iter()
        .filter(|symbol| symbol.find("lib_id").is_some())
        .find(|symbol| {
            symbol.find_all("property").into_iter().any(|property| {
                property.get(1).and_then(SexpNode::as_str) == Some("Reference")
                    && property.get(2).and_then(SexpNode::as_str) == Some(reference)
            })
        })
        .unwrap_or_else(|| panic!("{reference} is not in {}", file.display()));
    let mut paths = Vec::new();
    for project in symbol.find("instances").unwrap().find_all("project") {
        let name = project.get(1).and_then(SexpNode::as_str).unwrap();
        for path in project.find_all("path") {
            let path = path.get(1).and_then(SexpNode::as_str).unwrap();
            paths.push((name.to_owned(), path.to_owned()));
        }
    }
    paths.sort();
    paths
}

/// Every `(project, path)` the sheet `name` in `file` records, sorted.
fn sheet_recorded(file: &Path, name: &str) -> Vec<(String, String)> {
    let tree = parse_sexp(&std::fs::read_to_string(file).unwrap()).unwrap();
    let sheet = tree
        .find_all("sheet")
        .into_iter()
        .find(|sheet| {
            sheet.find_all("property").into_iter().any(|property| {
                property.get(1).and_then(SexpNode::as_str) == Some("Sheetname")
                    && property.get(2).and_then(SexpNode::as_str) == Some(name)
            })
        })
        .unwrap_or_else(|| panic!("sheet {name} is not in {}", file.display()));
    let mut paths = Vec::new();
    for project in sheet.find("instances").unwrap().find_all("project") {
        let project_name = project.get(1).and_then(SexpNode::as_str).unwrap();
        for path in project.find_all("path") {
            let path = path.get(1).and_then(SexpNode::as_str).unwrap();
            paths.push((project_name.to_owned(), path.to_owned()));
        }
    }
    paths.sort();
    paths
}

fn toplevel(paths: &[String]) -> Vec<(String, String)> {
    let mut paths: Vec<_> = paths
        .iter()
        .map(|path| ("toplevel".to_owned(), path.clone()))
        .collect();
    paths.sort();
    paths
}

fn kind(body: &Value) -> &str {
    body["error"]["kind"].as_str().unwrap_or_default()
}

#[tokio::test]
async fn a_project_that_lists_no_second_sheet_still_refuses_it() {
    for sheets in [
        None,
        Some(json!([{"filename": "toplevel.kicad_sch", "uuid": NIL}])),
    ] {
        let project = Project::new(sheets);
        let before = project.saved();
        let (refused, body) = place(&project.file("second.kicad_sch"), "R3").await;
        assert!(refused, "{body}");
        assert_eq!(kind(&body), "conflict", "{body}");
        assert_eq!(project.saved(), before);
    }
}

#[tokio::test]
async fn writes_reach_every_sheet_under_a_second_top_level_sheet() {
    let project = Project::new(Some(both(NIL, NIL)));
    let second = project.file("second.kicad_sch");
    let (refused, body) = place(&second, "R3").await;
    assert!(!refused, "{body}");
    assert_eq!(recorded(&second, "R3"), toplevel(&[format!("/{SECOND}")]));

    let (refused, body) = add_sheet(&second, "sub.kicad_sch", "Sub").await;
    assert!(!refused, "{body}");
    assert_eq!(
        sheet_recorded(&second, "Sub"),
        toplevel(&[format!("/{SECOND}")])
    );
    let sub_sheet = sheet_uuid(&second, "Sub");
    let sub = project.file("sub.kicad_sch");
    let (refused, body) = place(&sub, "R4").await;
    assert!(!refused, "{body}");
    assert_eq!(
        recorded(&sub, "R4"),
        toplevel(&[format!("/{SECOND}/{sub_sheet}")])
    );

    // The first top-level sheet and its child are written as before.
    let (refused, body) = place(&project.file("child.kicad_sch"), "R5").await;
    assert!(!refused, "{body}");
    assert_eq!(
        recorded(&project.file("child.kicad_sch"), "R5"),
        toplevel(&[format!("/{TOPLEVEL}/{CHILD_SHEET}")])
    );
}

/// The fixture records R1 and R2 under `toplevel.kicad_sch`'s own uuid; this
/// files them under `LISTED_TOPLEVEL`, as eeschema saves them when the project
/// gives the sheet that uuid, an edit no tool makes.
fn file_under_listed_uuid(project: &Project) {
    for name in ["toplevel.kicad_sch", "child.kicad_sch"] {
        let path = project.file(name);
        let text = std::fs::read_to_string(&path).unwrap();
        let moved = text.replace(&format!("\"/{TOPLEVEL}"), &format!("\"/{LISTED_TOPLEVEL}"));
        assert_ne!(moved, text);
        std::fs::write(&path, moved).unwrap();
    }
}

/// #125's own case: the project lists its one top-level sheet under another
/// uuid. Hierarchy annotation still refuses it.
#[tokio::test]
async fn a_sole_entry_keys_the_projects_sheet_to_its_uuid() {
    let project = Project::new(Some(json!([
        {"filename": "toplevel.kicad_sch", "name": "toplevel", "uuid": LISTED_TOPLEVEL}
    ])));
    file_under_listed_uuid(&project);
    let root = project.file("toplevel.kicad_sch");
    let child = project.file("child.kicad_sch");
    for (file, reference, path) in [
        (&root, "R6", format!("/{LISTED_TOPLEVEL}")),
        (&child, "R7", format!("/{LISTED_TOPLEVEL}/{CHILD_SHEET}")),
    ] {
        let (refused, body) = place(file, reference).await;
        assert!(!refused, "{reference}: {body}");
        assert_eq!(recorded(file, reference), toplevel(&[path]), "{reference}");
    }
    let before = project.saved();
    let (refused, body) = served(
        "annotate_schematic",
        json!({"schematic": root, "schematic_scope": "hierarchy"}),
    )
    .await;
    assert!(refused, "{body}");
    assert_eq!(body["error"]["field"], "schematic_scope", "{body}");
    assert_eq!(project.saved(), before);
}

/// A project that lists sheets loads those alone, so a file it leaves out,
/// its own `toplevel.kicad_sch` here, is no root: writes to it are refused.
#[tokio::test]
async fn a_file_its_project_does_not_list_is_no_root() {
    let project = Project::new(Some(json!([{"filename": "second.kicad_sch", "uuid": NIL}])));
    let root = project.file("toplevel.kicad_sch");
    let before = project.saved();
    for (refused, body) in [
        place(&root, "R6").await,
        place(&project.file("child.kicad_sch"), "R7").await,
        add_sheet(&root, "more.kicad_sch", "More").await,
    ] {
        assert!(refused, "{body}");
        assert_eq!(kind(&body), "conflict", "{body}");
    }
    assert_eq!(project.saved(), before);
}

/// eeschema keys a top-level sheet to its entry's uuid unless it is nil
/// (#125), the second as well as the first.
#[tokio::test]
async fn a_listed_uuid_keys_its_sheet_in_place_of_the_sheets_own() {
    let project = Project::new(Some(both(LISTED_TOPLEVEL, LISTED_SECOND)));
    file_under_listed_uuid(&project);
    let root = project.file("toplevel.kicad_sch");
    let child = project.file("child.kicad_sch");
    let second = project.file("second.kicad_sch");
    for (file, reference, path) in [
        (&root, "R6", format!("/{LISTED_TOPLEVEL}")),
        (&child, "R7", format!("/{LISTED_TOPLEVEL}/{CHILD_SHEET}")),
        (&second, "R8", format!("/{LISTED_SECOND}")),
    ] {
        let (refused, body) = place(file, reference).await;
        assert!(!refused, "{reference}: {body}");
        assert_eq!(recorded(file, reference), toplevel(&[path]), "{reference}");
    }

    // A sheet added inside the first top-level sheet keys through its uuid too.
    let (refused, body) = add_sheet(&root, "more.kicad_sch", "More").await;
    assert!(!refused, "{body}");
    assert_eq!(
        sheet_recorded(&root, "More"),
        toplevel(&[format!("/{LISTED_TOPLEVEL}")])
    );
    let more_sheet = sheet_uuid(&root, "More");
    let (refused, body) = place(&project.file("more.kicad_sch"), "R9").await;
    assert!(!refused, "{body}");
    assert_eq!(
        recorded(&project.file("more.kicad_sch"), "R9"),
        toplevel(&[format!("/{LISTED_TOPLEVEL}/{more_sheet}")])
    );
}

/// A file placed under both top-level sheets keeps the paths each gives it.
#[tokio::test]
async fn a_file_placed_under_both_top_level_sheets_keeps_both_paths() {
    let project = Project::new(Some(both(NIL, NIL)));
    let child = project.file("child.kicad_sch");
    let second = project.file("second.kicad_sch");
    let (refused, body) = add_sheet(&second, "child.kicad_sch", "Again").await;
    assert!(!refused, "{body}");
    let again = sheet_uuid(&second, "Again");
    let both_paths = [
        format!("/{TOPLEVEL}/{CHILD_SHEET}"),
        format!("/{SECOND}/{again}"),
    ];
    assert_eq!(recorded(&child, "R2"), toplevel(&both_paths));

    let (refused, body) = place(&child, "R10").await;
    assert!(!refused, "{body}");
    assert_eq!(recorded(&child, "R10"), toplevel(&both_paths));

    // Deleting the first placement leaves the second's path alone.
    let root = project.file("toplevel.kicad_sch");
    let (refused, body) = served(
        "delete_sheet",
        json!({"schematic": root, "sheet_name": "Child"}),
    )
    .await;
    assert!(!refused, "{body}");
    assert_eq!(
        recorded(&child, "R2"),
        toplevel(&[format!("/{SECOND}/{again}")])
    );

    // A file under the second sheet, placed again from the first, keeps its
    // path under the second.
    let (refused, body) = add_sheet_at(&second, "sub.kicad_sch", "Sub", 25.4).await;
    assert!(!refused, "{body}");
    let sub_sheet = sheet_uuid(&second, "Sub");
    let sub = project.file("sub.kicad_sch");
    let (refused, body) = place(&sub, "R11").await;
    assert!(!refused, "{body}");
    let (refused, body) = add_sheet(&root, "sub.kicad_sch", "SubAgain").await;
    assert!(!refused, "{body}");
    let sub_again = sheet_uuid(&root, "SubAgain");
    assert_eq!(
        recorded(&sub, "R11"),
        toplevel(&[
            format!("/{SECOND}/{sub_sheet}"),
            format!("/{TOPLEVEL}/{sub_again}")
        ])
    );
}

/// eeschema connects and numbers every top-level sheet as one design; a tool
/// that reads one sheet's hierarchy as the whole refuses and writes nothing.
#[tokio::test]
async fn tools_that_read_one_hierarchy_as_the_design_refuse_a_second_sheet() {
    let project = Project::new(Some(both(NIL, NIL)));
    let child = project.file("child.kicad_sch");
    let (refused, body) = served(
        "add_schematic_net_label",
        json!({"schematic": child, "net": "SIG", "x": 76.2, "y": 50.8}),
    )
    .await;
    assert!(!refused, "{body}");
    let before = project.saved();

    let (refused, body) = served(
        "rename_schematic_net_label",
        json!({"schematic": child, "net": "SIG", "scope": "sheet", "new_name": "SIG2"}),
    )
    .await;
    assert!(refused, "{body}");
    assert_eq!(kind(&body), "invalid_argument", "{body}");
    assert_eq!(body["error"]["field"], "schematic", "{body}");
    assert!(body.to_string().contains("second.kicad_sch"), "{body}");

    // As hierarchy-scope annotation refuses the same project.
    let (refused, body) = served(
        "annotate_schematic",
        json!({"schematic": child, "resolve_duplicates": true}),
    )
    .await;
    assert!(refused, "{body}");
    assert_eq!(kind(&body), "invalid_argument", "{body}");
    assert_eq!(body["error"]["field"], "schematic_scope", "{body}");
    assert!(body.to_string().contains("second.kicad_sch"), "{body}");
    assert_eq!(project.saved(), before);
}

/// eeschema locks the schematic named after the project, whichever
/// top-level sheet a write reaches.
#[tokio::test]
async fn the_projects_editor_lock_guards_a_second_top_level_sheet() {
    let project = Project::new(Some(both(NIL, NIL)));
    let second = project.file("second.kicad_sch");
    let (refused, body) = served(
        "add_schematic_text",
        json!({"schematic": second, "text": "note", "x": 50.8, "y": 25.4}),
    )
    .await;
    assert!(!refused, "{body}");
    let (_, listed) = served("list_schematic_texts", json!({"schematic": second})).await;
    let lock = project.file("~toplevel.kicad_sch.lck");
    std::fs::write(&lock, "").unwrap();
    let before = project.saved();
    let (refused, body) = served(
        "edit_schematic_text",
        json!({"schematic": second, "uuid": listed["texts"][0]["uuid"],
            "revision": listed["revision"], "text": "changed"}),
    )
    .await;
    assert!(refused, "{body}");
    assert_eq!(kind(&body), "conflict", "{body}");
    assert_eq!(
        body["error"]["paths"],
        json!([project.file("toplevel.kicad_sch")]),
        "{body}"
    );
    assert_eq!(project.saved(), before);
}

/// A project may list one top-level sheet that is not its own schematic;
/// eeschema still opens, and locks, the schematic named after the project.
#[tokio::test]
async fn a_sole_listed_sheet_is_guarded_by_the_projects_editor_lock() {
    let project = Project::new(Some(json!([{"filename": "second.kicad_sch", "uuid": NIL}])));
    let second = project.file("second.kicad_sch");
    let (refused, body) = served(
        "add_schematic_net_label",
        json!({"schematic": second, "net": "SIG", "x": 76.2, "y": 50.8}),
    )
    .await;
    assert!(!refused, "{body}");
    std::fs::write(project.file("~toplevel.kicad_sch.lck"), "").unwrap();
    let before = project.saved();
    for (tool, arguments) in [
        (
            "set_schematic_title_block",
            json!({"schematic": second, "title": "Second"}),
        ),
        (
            "rename_schematic_net_label",
            json!({"schematic": second, "net": "SIG", "scope": "sheet", "new_name": "SIG2"}),
        ),
        (
            "annotate_schematic",
            json!({"schematic": second, "schematic_scope": "hierarchy"}),
        ),
    ] {
        let (refused, body) = served(tool, arguments).await;
        assert!(refused, "{tool}: {body}");
        assert_eq!(kind(&body), "conflict", "{tool}: {body}");
        let text = body.to_string();
        assert!(text.contains("KiCad editor lock"), "{tool}: {body}");
        assert!(text.contains("toplevel.kicad_sch"), "{tool}: {body}");
        assert_eq!(project.saved(), before, "{tool}");
    }
}

/// kicad-cli takes the project from the name of the file it is handed, and
/// 10.0.6 checks that file's hierarchy alone, so no file it can be handed
/// checks a sheet under a second top-level sheet with its project.
#[tokio::test]
async fn erc_under_a_second_top_level_sheet_is_refused() {
    let project = Project::new(Some(both(NIL, NIL)));
    let second = project.file("second.kicad_sch");
    let (refused, body) = add_sheet(&second, "sub.kicad_sch", "Sub").await;
    assert!(!refused, "{body}");
    let before = project.saved();
    for file in [second, project.file("sub.kicad_sch")] {
        let (refused, body) = served("run_erc", json!({ "schematic": file })).await;
        assert!(refused, "{body}");
        assert_eq!(kind(&body), "unsupported_capability", "{body}");
        let text = body.to_string();
        assert!(text.contains("toplevel.kicad_sch"), "{body}");
        assert!(text.contains("second.kicad_sch"), "{body}");
    }
    assert_eq!(project.saved(), before);
}

/// How `schematic.top_level_sheets` is read: as eeschema 10 reads it, but for
/// an entry without a uuid, read as nil. The project files are inline: no
/// tool writes the list.
#[test]
fn how_the_list_is_read() {
    let dir = tempfile::tempdir().unwrap();
    let pro = dir.path().join("demo.kicad_pro");
    let own = dir.path().join("demo.kicad_sch");
    let other = dir.path().join("other.kicad_sch");
    std::fs::write(&own, super::blank_schematic_template()).unwrap();
    std::fs::write(&other, super::blank_schematic_template()).unwrap();
    let read = |text: &str| {
        std::fs::write(&pro, text).unwrap();
        super::listed_top_level_sheets(&pro)
    };
    let alone = vec![(own.clone(), None)];
    for unlisted in [
        "",
        "not json",
        "{}",
        r#"{"schematic": {}}"#,
        r#"{"schematic": {"top_level_sheets": null}}"#,
        r#"{"schematic": {"top_level_sheets": {}}}"#,
        r#"{"schematic": {"top_level_sheets": []}}"#,
    ] {
        assert_eq!(read(unlisted), alone, "{unlisted:?}");
    }
    let listed =
        |sheets: Value| read(&json!({"schematic": {"top_level_sheets": sheets}}).to_string());
    // One field that is not a string drops the whole list, as KiCad drops it.
    for bad in [
        json!({"uuid": 7}),
        json!({"name": null}),
        json!({"filename": []}),
    ] {
        let mut entry = json!({"filename": "other.kicad_sch", "uuid": LISTED_SECOND});
        entry
            .as_object_mut()
            .unwrap()
            .extend(bad.as_object().unwrap().clone());
        assert_eq!(
            listed(json!([{"filename": "other.kicad_sch"}, entry])),
            alone,
            "{bad}"
        );
    }
    assert_eq!(
        listed(json!([
            {"filename": "demo.kicad_sch", "uuid": NIL},
            {"filename": "other.kicad_sch", "uuid": LISTED_SECOND}
        ])),
        vec![
            (own.clone(), None),
            (other.clone(), Some(LISTED_SECOND.to_owned()))
        ]
    );
    // A missing file is the project's own when that exists, as a renamed
    // template's list is mended, keeping its uuid; listed twice, it is two
    // top-level sheets, as eeschema loads it twice.
    assert_eq!(
        listed(json!([{"filename": "template.kicad_sch", "uuid": LISTED_SECOND}])),
        vec![(own.clone(), Some(LISTED_SECOND.to_owned()))]
    );
    assert_eq!(
        listed(json!([
            {"filename": "template.kicad_sch", "uuid": NIL},
            {"filename": "demo.kicad_sch"}
        ])),
        vec![(own.clone(), None), (own.clone(), None)]
    );
    std::fs::remove_file(&own).unwrap();
    assert_eq!(
        listed(json!([
            {"filename": "gone.kicad_sch"},
            {"filename": "other.kicad_sch"}
        ])),
        vec![(other.clone(), None)]
    );
    // With nothing left to load, the project's own file stands, unreadable.
    assert_eq!(listed(json!([{"filename": "gone.kicad_sch"}])), alone);
    // A project file that cannot be read lists nothing.
    std::fs::remove_file(&pro).unwrap();
    std::fs::create_dir(&pro).unwrap();
    assert_eq!(super::listed_top_level_sheets(&pro), alone);
}
