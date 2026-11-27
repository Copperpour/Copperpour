// `stub_symbol_dir` returns a MutexGuard that the async tests then hold across
// their `.await`s, which is what `await_holding_lock` warns about. It is
// deliberate and safe here: the lock serialises process-wide `KICAD*_DIR`
// environment variables, which the awaited calls read, so releasing it early
// would defeat its only purpose. cargo runs each test on its own OS thread with
// its own current-thread runtime, and each runtime drives exactly one task, so
// there is no second task that could contend for the guard and deadlock.
#![allow(clippy::await_holding_lock)]

use super::*;
use crate::router::ToolRouter;
use crate::tools::sch_hierarchy::handle_create_schematic;
use crate::tools::ServerConfig;
use std::sync::Arc;

/// A context whose `jlcpcb_db_path` setting names `db_path`, as a caller
/// configures the catalog: the placement tools take no path argument.
#[cfg(feature = "jlcpcb")]
fn ctx_with_catalog(db_path: &std::path::Path) -> ToolContext {
    ToolContext::new(
        ServerConfig {
            jlcpcb_db_path: Some(db_path.to_path_buf()),
            ..Default::default()
        },
        Arc::new(ToolRouter::new()),
    )
}

fn test_ctx() -> ToolContext {
    ToolContext::new(
        ServerConfig {
            kicad_cli: String::new(),
            kicad_binary: String::new(),
            ipc_address: String::new(),
            project_dir: None,
            jlcpcb_db_path: None,
            auto_load_toolsets: false,
            eager_toolsets: false,
        },
        Arc::new(ToolRouter::new()),
    )
}

/// Serializes tests that set KICAD10_SYMBOL_DIR (process-wide env), shared
/// with every other module that does so.
use crate::tools::KICAD_ENV_LOCK as SYMBOL_DIR_ENV;

/// Only the stub carries this, so asserting on it proves a placement
/// resolved the fixture and not a KiCad library installed on the machine.
const STUB_MARKER: &str = "stub://device";

/// A stub symbol library so component adds resolve without an installed
/// KiCad (CI has none): Device:R and Device:C_Polarized in the KiCad 10
/// symdir layout, plus a `sym-lib-table` registering them.
///
/// The returned tempdir doubles as the project directory — put the test's
/// schematic in it, so the project table is the one consulted.
fn stub_symbol_dir() -> (tempfile::TempDir, std::sync::MutexGuard<'static, ()>) {
    let guard = SYMBOL_DIR_ENV.lock().unwrap_or_else(|e| e.into_inner());
    let dir = tempfile::tempdir().unwrap();
    let symdir = dir.path().join("Device.kicad_symdir");
    std::fs::create_dir_all(&symdir).unwrap();
    let symbol = |name: &str| {
        format!(
                "(kicad_symbol_lib\n\t(version 20241209)\n\t(generator \"test\")\n\t(symbol \"{name}\"\n\t\t(property \"Reference\" \"R\" (at 0 0 0))\n\t\t(property \"Value\" \"{name}\" (at 0 0 0))\n\t\t(property \"Datasheet\" \"{STUB_MARKER}\" (at 0 0 0))\n\t\t(symbol \"{name}_0_1\"\n\t\t\t(pin passive line (at 0 3.81 270) (length 1.27)\n\t\t\t\t(name \"~\" (effects (font (size 1.27 1.27))))\n\t\t\t\t(number \"1\" (effects (font (size 1.27 1.27))))\n\t\t\t)\n\t\t\t(pin passive line (at 0 -3.81 90) (length 1.27)\n\t\t\t\t(name \"~\" (effects (font (size 1.27 1.27))))\n\t\t\t\t(number \"2\" (effects (font (size 1.27 1.27))))\n\t\t\t)\n\t\t)\n\t)\n)\n"
            )
    };
    std::fs::write(symdir.join("R.kicad_sym"), symbol("R")).unwrap();
    std::fs::write(symdir.join("C_Polarized.kicad_sym"), symbol("C_Polarized")).unwrap();
    std::fs::write(
            symdir.join("WITH_DEFAULTS.kicad_sym"),
            format!(
                "(kicad_symbol_lib\n\t(version 20241209)\n\t(generator \"eeschema\")\n\t(symbol \"WITH_DEFAULTS\"\n\t\t(property \"Reference\" \"U\" (at 0 0 0))\n\t\t(property \"Value\" \"Library Default\" (at 0 0 0))\n\t\t(property \"Footprint\" \"Package_DIP:DIP-8_W7.62mm\" (at 0 0 0) hide)\n\t\t(property \"Datasheet\" \"{STUB_MARKER}\" (at 0 0 0) hide)\n\t\t(property \"Description\" \"Library-owned placement defaults\" (at 0 0 0) hide)\n\t\t(symbol \"WITH_DEFAULTS_0_1\"\n\t\t\t(pin passive line (at 0 3.81 270) (length 1.27)\n\t\t\t\t(name \"~\" (effects (font (size 1.27 1.27))))\n\t\t\t\t(number \"1\" (effects (font (size 1.27 1.27))))\n\t\t\t)\n\t\t)\n\t)\n)\n"
            ),
        )
        .unwrap();
    std::fs::write(
            symdir.join("WITH_CUSTOM_FIELDS.kicad_sym"),
            format!(
                "(kicad_symbol_lib\n\t(version 20241209)\n\t(generator \"eeschema\")\n\t(symbol \"WITH_CUSTOM_FIELDS\"\n\t\t(property \"Reference\" \"U\" (at 0 0 0))\n\t\t(property \"Value\" \"Custom Part\" (at 0 0 0))\n\t\t(property \"Footprint\" \"Package_SO:SOIC-8\" (at 0 0 0) hide)\n\t\t(property \"Datasheet\" \"{STUB_MARKER}\" (at 0 0 0) hide)\n\t\t(property \"Description\" \"Custom part description\" (at 0 0 0) hide)\n\t\t(property \"LCSC\" \"C9999\" (at 0 0 0) hide)\n\t\t(property \"MPN\" \"MY-MPN-100\" (at 0 0 0) hide)\n\t\t(property \"Manufacturer\" \"MyMfr\" (at 0 0 0) hide)\n\t\t(symbol \"WITH_CUSTOM_FIELDS_0_1\"\n\t\t\t(pin passive line (at 0 3.81 270) (length 1.27)\n\t\t\t\t(name \"~\" (effects (font (size 1.27 1.27))))\n\t\t\t\t(number \"1\" (effects (font (size 1.27 1.27))))\n\t\t\t)\n\t\t)\n\t)\n)\n"
            ),
        )
        .unwrap();
    std::fs::write(
        symdir.join("WITH_LEGACY_HIDDEN_FIELD.kicad_sym"),
        "(kicad_symbol_lib\n\t(version 20241209)\n\t(generator \"eeschema\")\n\t(symbol \"WITH_LEGACY_HIDDEN_FIELD\"\n\t\t(property \"Reference\" \"U\" (at 0 0 0))\n\t\t(property \"Value\" \"Legacy hidden value\" (at 0 0 0) (effects (font (size 1.27 1.27)) hide))\n\t\t(symbol \"WITH_LEGACY_HIDDEN_FIELD_0_1\"\n\t\t\t(pin passive line (at 0 3.81 270) (length 1.27)\n\t\t\t\t(name \"~\" (effects (font (size 1.27 1.27))))\n\t\t\t\t(number \"1\" (effects (font (size 1.27 1.27))))\n\t\t\t)\n\t\t)\n\t)\n)\n",
    )
    .unwrap();
    // LM2904-style multi-unit part: unit 1 = pins 1-3, unit 2 = pins 5-7,
    // unit 3 = power pins 4/8 (#35 repro shape).
    let pin = |num: &str, x: f64, y: f64, angle: u32| {
        format!(
                "\t\t\t(pin passive line (at {x} {y} {angle}) (length 2.54)\n\t\t\t\t(name \"~\" (effects (font (size 1.27 1.27))))\n\t\t\t\t(number \"{num}\" (effects (font (size 1.27 1.27))))\n\t\t\t)\n"
            )
    };
    let opamp = format!(
            "(kicad_symbol_lib\n\t(version 20241209)\n\t(generator \"test\")\n\t(symbol \"OPAMP_DUAL\"\n\t\t(property \"Reference\" \"U\" (at 0 0 0))\n\t\t(property \"Value\" \"OPAMP_DUAL\" (at 0 0 0))\n\t\t(symbol \"OPAMP_DUAL_1_1\"\n{}{}{}\t\t)\n\t\t(symbol \"OPAMP_DUAL_2_1\"\n{}{}{}\t\t)\n\t\t(symbol \"OPAMP_DUAL_3_1\"\n{}{}\t\t)\n\t)\n)\n",
            pin("1", -7.62, 2.54, 0),
            pin("2", -7.62, -2.54, 0),
            pin("3", 7.62, 0.0, 180),
            pin("5", -7.62, 2.54, 0),
            pin("6", -7.62, -2.54, 0),
            pin("7", 7.62, 0.0, 180),
            pin("4", 0.0, -7.62, 90),
            pin("8", 0.0, 7.62, 270),
        );
    std::fs::write(symdir.join("OPAMP_DUAL.kicad_sym"), opamp).unwrap();
    // Derived symbol: an extends stub with no drawing of its own, like
    // Amplifier_Operational:NE5532 → LM2904.
    std::fs::write(
            symdir.join("OPAMP_DERIVED.kicad_sym"),
            "(kicad_symbol_lib\n\t(version 20241209)\n\t(generator \"test\")\n\t(symbol \"OPAMP_DERIVED\"\n\t\t(extends \"OPAMP_DUAL\")\n\t\t(property \"Reference\" \"U\" (at 0 0 0))\n\t\t(property \"Value\" \"OPAMP_DERIVED\" (at 0 0 0))\n\t)\n)\n",
        )
        .unwrap();
    // A project sym-lib-table, checked before the global one, is what
    // makes this hermetic: KICAD10_SYMBOL_DIR alone is not enough, because
    // the global table's own `Device` entry resolves to whatever KiCad the
    // developer has installed and would shadow the stub.
    std::fs::write(
            dir.path().join("sym-lib-table"),
            format!(
                "(sym_lib_table\n  (version 7)\n  (lib (name \"Device\") (type \"KiCad\") (uri \"{}\") (options \"\") (descr \"\"))\n)\n",
                symdir.display()
            ),
        )
        .unwrap();
    std::env::set_var("KICAD10_SYMBOL_DIR", dir.path());
    (dir, guard)
}

/// #204: on a child sheet both halves of the instance key came from the
/// child file — its own stem as the project name, its own uuid as the
/// whole path. KiCad matches that against nothing, so every symbol placed
/// on a sub-sheet read as unannotated.
#[tokio::test]
async fn a_child_sheet_keys_instances_to_the_root_not_itself() {
    let (_symdir, _env) = stub_symbol_dir();
    let dir = tempfile::tempdir().unwrap();
    let ctx = test_ctx();
    std::fs::write(dir.path().join("board.kicad_pro"), "{}").unwrap();
    let root = dir.path().join("board.kicad_sch");
    let child = dir.path().join("amp.kicad_sch");
    std::fs::write(
            &root,
            "(kicad_sch\n\t(version 20250610)\n\t(generator \"eeschema\")\n\t(uuid \"ROOTUUID\")\n\t(paper \"A4\")\n\t(lib_symbols)\n\t(sheet\n\t\t(at 50 50)\n\t\t(size 20 20)\n\t\t(uuid \"SHEETUUID\")\n\t\t(property \"Sheetname\" \"amp\")\n\t\t(property \"Sheetfile\" \"amp.kicad_sch\")\n\t)\n)\n",
        )
        .unwrap();
    std::fs::write(
            &child,
            "(kicad_sch\n\t(version 20250610)\n\t(generator \"eeschema\")\n\t(uuid \"CHILDUUID\")\n\t(paper \"A4\")\n\t(lib_symbols)\n)\n",
        )
        .unwrap();

    let placed = handle_add_schematic_component(
        &json!({ "schematic": child.display().to_string(), "lib_id": "Device:R",
                     "reference": "R1", "x": 100.0, "y": 100.0 }),
        &ctx,
    )
    .await
    .unwrap();
    assert!(!placed.is_error, "{placed:?}");

    let written = std::fs::read_to_string(&child).unwrap();
    assert!(
        written.contains("(project \"board\""),
        "the project name is the .kicad_pro stem, not the child file stem:\n{written}"
    );
    assert!(
        written.contains("/ROOTUUID/SHEETUUID"),
        "the path must run root -> sheet:\n{written}"
    );
    assert!(
        !written.contains("(path \"/CHILDUUID\""),
        "the child's own uuid must not be the whole path:\n{written}"
    );
}

#[tokio::test]
async fn placement_refuses_ambiguous_project_ownership_without_writing() {
    let outer = tempfile::tempdir().unwrap();
    let nested = outer.path().join("nested");
    std::fs::create_dir_all(&nested).unwrap();
    std::fs::write(outer.path().join("outer.kicad_pro"), "{}").unwrap();
    std::fs::write(nested.join("inner.kicad_pro"), "{}").unwrap();
    let root = |root_uuid: &str, sheet_uuid: &str, child: &str| {
        format!(
            r#"(kicad_sch
	(version 20250610)
	(generator "eeschema")
	(uuid "{root_uuid}")
	(paper "A4")
	(lib_symbols)
	(sheet
		(at 20 20)
		(size 40 20)
		(uuid "{sheet_uuid}")
		(property "Sheetname" "Child" (at 20 19.365 0))
		(property "Sheetfile" "{child}" (at 20 40.635 0))
	)
	(sheet_instances (path "/" (page "1")))
)
"#,
        )
    };
    std::fs::write(
        outer.path().join("outer.kicad_sch"),
        root("outer-root", "outer-path", "nested/child.kicad_sch"),
    )
    .unwrap();
    std::fs::write(
        nested.join("inner.kicad_sch"),
        root("inner-root", "inner-path", "child.kicad_sch"),
    )
    .unwrap();
    let child = nested.join("child.kicad_sch");
    std::fs::write(&child, crate::tools::blank_schematic_template()).unwrap();
    let before = std::fs::read(&child).unwrap();

    let result = handle_add_schematic_component(
        &json!({
            "schematic": child.display().to_string(),
            "lib_id": "Device:R",
            "reference": "R1",
            "x": 100.0,
            "y": 100.0
        }),
        &test_ctx(),
    )
    .await
    .unwrap();

    assert!(result.is_error);
    assert_eq!(
        crate::mcp::error::extract_error_kind(&result).as_deref(),
        Some("conflict")
    );
    assert_eq!(std::fs::read(&child).unwrap(), before);
}

/// A standalone sheet — no project file, no parent — keeps the old
/// behaviour: it is its own root.
#[tokio::test]
async fn a_standalone_sheet_still_keys_instances_to_itself() {
    let (_symdir, _env) = stub_symbol_dir();
    let dir = tempfile::tempdir().unwrap();
    let ctx = test_ctx();
    let path = dir.path().join("loose.kicad_sch");
    handle_create_schematic(&json!({ "path": path.display().to_string() }), &ctx)
        .await
        .unwrap();
    handle_add_schematic_component(
        &json!({ "schematic": path.display().to_string(), "lib_id": "Device:R",
                     "reference": "R1", "x": 100.0, "y": 100.0 }),
        &ctx,
    )
    .await
    .unwrap();

    let written = std::fs::read_to_string(&path).unwrap();
    let sch = cse::Schematic::load(&path).unwrap();
    let own = sch.uuid.clone().unwrap();
    assert!(
        written.contains(&format!("(path \"/{own}\"")),
        "a loose sheet is its own root:\n{written}"
    );
    assert!(written.contains("(project \"loose\""), "{written}");
}

#[tokio::test]
async fn add_component_writes_eeschema_style_instance_path() {
    let (dir, _env) = stub_symbol_dir();
    let path = dir.path().join("amp.kicad_sch");
    let ctx = test_ctx();

    handle_create_schematic(&json!({ "path": path.display().to_string() }), &ctx)
        .await
        .unwrap();
    let result = handle_add_schematic_component(
        &json!({
            "schematic": path.display().to_string(),
            "lib_id": "Device:R",
            "x": 100.0, "y": 80.0,
            "reference": "R1"
        }),
        &ctx,
    )
    .await
    .unwrap();
    assert!(!result.is_error);
    let ToolContent::Text { text } = &result.content[0] else {
        panic!("expected text")
    };
    let response: serde_json::Value = serde_json::from_str(text).unwrap();
    assert_eq!(response["outcome"]["status"], "complete");
    assert_eq!(response["outcome"]["source"], "saved_file_readback");

    // Guards the fixture itself: the project sym-lib-table must win over
    // any real Device library the developer has installed, or these tests
    // silently stop exercising the stub they set up.
    let raw = std::fs::read_to_string(&path).unwrap();
    assert!(
        raw.contains(STUB_MARKER),
        "Device:R must resolve from the stub, not an installed KiCad library"
    );

    let sch = cse::Schematic::load(&path).unwrap();
    let root_uuid = sch.uuid.clone().expect("root uuid present");
    let sym = sch.symbols.by_reference("R1").unwrap();
    // KiCAD only forms wire-only nets when the instance path is exactly
    // "/<root-uuid>"; the project key mirrors eeschema (file stem).
    assert!(
        sym.has_instance_path("amp", &format!("/{}", root_uuid)),
        "instance path must be /<root-uuid> under the file-stem project name"
    );
    assert!(
        !raw.lines()
            .any(|line| line.ends_with(' ') || line.ends_with('\t')),
        "component placement must not leave trailing whitespace: {raw:?}"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn add_component_reports_uncertain_when_saved_readback_fails_after_write() {
    let (dir, _env) = stub_symbol_dir();
    let path = dir.path().join("uncertain.kicad_sch");
    let ctx = test_ctx();
    handle_create_schematic(&json!({"path": path.display().to_string()}), &ctx)
        .await
        .unwrap();
    COMPONENT_COMMITTED_LOAD_FAULT.with(|fault| {
        *fault.borrow_mut() = Some("injected saved-file readback failure".to_string());
    });

    let result = handle_add_schematic_component(
        &json!({
            "schematic": path.display().to_string(),
            "lib_id": "Device:R",
            "reference": "R1",
            "x": 100.0,
            "y": 80.0
        }),
        &ctx,
    )
    .await
    .unwrap();

    assert!(result.is_error);
    assert_eq!(
        crate::mcp::error::extract_error_kind(&result).as_deref(),
        Some("mutation_outcome_uncertain")
    );
    let ToolContent::Text { text } = &result.content[0] else {
        panic!("expected text")
    };
    let response: serde_json::Value = serde_json::from_str(text).unwrap();
    assert_eq!(response["outcome"]["status"], "uncertain");
    assert_eq!(response["outcome"]["retry"]["safe"], false);
    assert_eq!(response["outcome"]["retry"]["scope"], "inspect_target");
    assert!(
        cse::Schematic::load(&path)
            .unwrap()
            .symbols
            .by_reference("R1")
            .is_some(),
        "the error occurs after the placement committed"
    );
}

#[tokio::test]
async fn placement_uses_library_value_and_footprint_unless_explicitly_overridden() {
    let (dir, _env) = stub_symbol_dir();
    let path = dir.path().join("defaults.kicad_sch");
    let ctx = test_ctx();
    handle_create_schematic(&json!({ "path": path.display().to_string() }), &ctx)
        .await
        .unwrap();

    let inherited = handle_add_schematic_component(
        &json!({
            "schematic": path.display().to_string(),
            "verbose": true,
            "lib_id": "Device:WITH_DEFAULTS",
            "x": 100.0, "y": 80.0,
            "reference": "U1"
        }),
        &ctx,
    )
    .await
    .unwrap();
    let inherited: serde_json::Value =
        serde_json::from_str(&content_text(&inherited)).expect("placement JSON");
    assert_eq!(inherited["fields"]["Value"], "Library Default");
    assert_eq!(
        inherited["fields"]["Footprint"],
        "Package_DIP:DIP-8_W7.62mm"
    );

    let overridden = handle_add_schematic_component(
        &json!({
            "schematic": path.display().to_string(),
            "verbose": true,
            "lib_id": "Device:WITH_DEFAULTS",
            "x": 120.0, "y": 80.0,
            "reference": "U2",
            "value": "Explicit Value",
            "footprint": "Package_DIP:DIP-14_W7.62mm"
        }),
        &ctx,
    )
    .await
    .unwrap();
    let overridden: serde_json::Value =
        serde_json::from_str(&content_text(&overridden)).expect("placement JSON");
    assert_eq!(overridden["fields"]["Value"], "Explicit Value");
    assert_eq!(
        overridden["fields"]["Footprint"],
        "Package_DIP:DIP-14_W7.62mm"
    );
}

#[test]
fn placement_field_resolution_preserves_empty_library_footprints_and_old_value_fallback() {
    let metadata = cse::library::SymbolMetadata::default();
    let fields = PlacementFields::resolve("Device:R", &metadata, None, None);
    assert_eq!(fields.value, "R");
    assert_eq!(fields.footprint, "");

    let fields = PlacementFields::resolve(
        "Device:R",
        &metadata,
        Some("10k"),
        Some("Resistor_THT:R_Axial"),
    );
    assert_eq!(fields.value, "10k");
    assert_eq!(fields.footprint, "Resistor_THT:R_Axial");
}

/// A placement without `reference` keeps the stock library prefix, so
/// annotation yields `R1` and `#FLG01`, not `1` and `2` (#669).
#[tokio::test]
async fn add_component_without_reference_keeps_library_prefix() {
    let (dir, _env) = crate::tools::stock_reference_prefix_libraries();
    let path = dir.path().join("prefix.kicad_sch");
    let ctx = test_ctx();
    handle_create_schematic(&json!({ "path": path.display().to_string() }), &ctx)
        .await
        .unwrap();
    for (lib_id, x) in [("Device:R", 101.6), ("power:PWR_FLAG", 127.0)] {
        let result = handle_add_schematic_component(
            &json!({
                "schematic": path.display().to_string(),
                "lib_id": lib_id,
                "x": x, "y": 101.6,
            }),
            &ctx,
        )
        .await
        .unwrap();
        assert!(!result.is_error, "{lib_id}: {result:?}");
    }
    assert_eq!(
        crate::tools::placed_references(&path),
        vec![
            ("Device:R".into(), "R?".into(), vec!["R?".into()]),
            (
                "power:PWR_FLAG".into(),
                "#FLG?".into(),
                vec!["#FLG?".into()]
            ),
        ]
    );

    let result =
        handle_annotate_schematic(&json!({ "schematic": path.display().to_string() }), &ctx)
            .await
            .unwrap();
    assert!(!result.is_error, "{result:?}");
    assert_eq!(
        crate::tools::placed_references(&path),
        vec![
            ("Device:R".into(), "R1".into(), vec!["R1".into()]),
            (
                "power:PWR_FLAG".into(),
                "#FLG01".into(),
                vec!["#FLG01".into()]
            ),
        ]
    );
}

/// The mid-wire junction pass must look the part up by its resolved `R?`:
/// keyed on the omitted reference it finds nothing, adds no dot, and still
/// succeeds, leaving both pins unconnected.
#[tokio::test]
async fn served_add_component_without_reference_joins_pins_landing_mid_wire() {
    let (dir, _env) = crate::tools::stock_reference_prefix_libraries();
    let path = dir.path().join("prefix.kicad_sch");
    let schematic = path.display().to_string();
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
    let call = |name: &str, arguments: serde_json::Value| {
        let message = json!({"jsonrpc": "2.0", "id": 669, "method": "tools/call",
                "params": {"name": name, "arguments": arguments}});
        let handler = &handler;
        async move {
            let result = handler
                .handle_message(message)
                .await
                .unwrap()
                .result
                .unwrap();
            assert_ne!(result["isError"], true, "{result}");
            serde_json::from_str::<serde_json::Value>(
                result["content"][0]["text"].as_str().unwrap(),
            )
            .unwrap()
        }
    };

    call("create_schematic", json!({ "path": schematic })).await;
    // Device:R at (101.6, 101.6) puts its pins at y 97.79 and 105.41.
    for y in [97.79, 105.41] {
        call(
            "add_wire",
            json!({ "schematic": schematic, "x1": 96.52, "y1": y, "x2": 106.68, "y2": y }),
        )
        .await;
    }
    let placed = call(
        "add_schematic_component",
        json!({ "schematic": schematic, "lib_id": "Device:R", "x": 101.6, "y": 101.6 }),
    )
    .await;

    let pins = [(101.6, 97.79), (101.6, 105.41)];
    let matches = |points: Vec<(f64, f64)>| {
        points.len() == pins.len()
            && pins.iter().all(|&(px, py)| {
                points
                    .iter()
                    .any(|&(x, y)| copperpour_sexp::geometry::points_coincident(px, py, x, y, 0.01))
            })
    };
    let reported = placed["junctions_added"]
        .as_array()
        .unwrap()
        .iter()
        .map(|point| (point["x"].as_f64().unwrap(), point["y"].as_f64().unwrap()))
        .collect();
    assert!(matches(reported), "{placed}");
    let saved = cse::Schematic::load(&path).unwrap();
    assert!(
        matches(saved.junctions.iter().map(|j| (j.x, j.y)).collect()),
        "without these dots KiCad leaves both pins of R? unconnected"
    );
}

#[tokio::test]
async fn add_component_writes_requested_unit() {
    let (dir, _env) = stub_symbol_dir();
    let path = dir.path().join("multi.kicad_sch");
    let ctx = test_ctx();

    handle_create_schematic(&json!({ "path": path.display().to_string() }), &ctx)
        .await
        .unwrap();
    let result = handle_add_schematic_component(
        &json!({
            "schematic": path.display().to_string(),
            "verbose": true,
            "lib_id": "Device:OPAMP_DUAL",
            "x": 100.0, "y": 80.0,
            "reference": "U1",
            "unit": 3
        }),
        &ctx,
    )
    .await
    .unwrap();
    assert!(!result.is_error, "unit 3 of a 3-unit part must be accepted");
    let response: serde_json::Value =
        serde_json::from_str(&content_text(&result)).expect("placement JSON");
    assert_eq!(response["schematic"], path.display().to_string());
    assert_eq!(response["unit"], 3);
    assert_eq!(response["unit_count"], 1);
    assert_eq!(response["units"][0]["unit"], 3);
    assert_eq!(response["units"][0]["fields"]["Reference"], "U1");

    let sch = cse::Schematic::load(&path).unwrap();
    let sym = sch.symbols.by_reference("U1").unwrap();
    assert_eq!(sym.unit, 3, "symbol (unit N) must match the requested unit");
    let root_uuid = sch.uuid.clone().unwrap();
    // Instance entry must carry the same unit, not a hardcoded 1.
    let raw = std::fs::read_to_string(&path).unwrap();
    assert!(raw.contains(&format!("/{}", root_uuid)));
    assert!(raw.contains("(unit 3)"), "instance unit must be 3");
}

#[tokio::test]
async fn direct_add_component_refuses_a_malformed_unit_without_writing() {
    let (dir, _env) = stub_symbol_dir();
    let path = dir.path().join("malformed-unit.kicad_sch");
    let ctx = test_ctx();

    handle_create_schematic(&json!({ "path": path.display().to_string() }), &ctx)
        .await
        .unwrap();
    let before = std::fs::read_to_string(&path).unwrap();

    for unit in [json!("two"), json!(2.7)] {
        let result = handle_add_schematic_component(
            &json!({
                "schematic": path.display().to_string(),
                "lib_id": "Device:OPAMP_DUAL",
                "x": 100.0,
                "y": 80.0,
                "reference": "U1",
                "unit": unit
            }),
            &ctx,
        )
        .await
        .unwrap();

        assert!(result.is_error);
        assert_eq!(
            crate::mcp::error::extract_error_kind(&result).as_deref(),
            Some("invalid_argument")
        );
        let body: serde_json::Value = serde_json::from_str(&content_text(&result)).unwrap();
        assert_eq!(body["error"]["field"], "unit");
        assert_eq!(std::fs::read_to_string(&path).unwrap(), before);
    }
}

fn content_text(res: &CallToolResult) -> String {
    match res.content.first() {
        Some(crate::mcp::protocol::ToolContent::Text { text }) => text.clone(),
        other => panic!("expected text content, got {other:?}"),
    }
}

#[tokio::test]
async fn add_component_rejects_out_of_range_unit() {
    let (dir, _env) = stub_symbol_dir();
    let path = dir.path().join("units.kicad_sch");
    let ctx = test_ctx();

    handle_create_schematic(&json!({ "path": path.display().to_string() }), &ctx)
        .await
        .unwrap();
    let before = std::fs::read_to_string(&path).unwrap();

    for bad_unit in [0, 99] {
        let result = handle_add_schematic_component(
            &json!({
                "schematic": path.display().to_string(),
                "lib_id": "Device:OPAMP_DUAL",
                "x": 100.0, "y": 80.0,
                "reference": "U1",
                "unit": bad_unit
            }),
            &ctx,
        )
        .await
        .unwrap();
        assert!(result.is_error, "unit {bad_unit} must be rejected");
        let text = content_text(&result);
        assert!(
            text.contains("3 unit"),
            "error must state the unit count: {text}"
        );
    }
    // A single-unit symbol only accepts unit 1.
    let result = handle_add_schematic_component(
        &json!({
            "schematic": path.display().to_string(),
            "lib_id": "Device:R",
            "x": 100.0, "y": 80.0,
            "reference": "R1",
            "unit": 2
        }),
        &ctx,
    )
    .await
    .unwrap();
    assert!(
        result.is_error,
        "unit 2 of a 1-unit symbol must be rejected"
    );
    assert_eq!(
        std::fs::read_to_string(&path).unwrap(),
        before,
        "rejected placements must not modify the schematic"
    );
}

/// The units of one multi-unit part share its designator. A unit already
/// placed, another value or another part under it is refused, and nothing
/// is written.
#[tokio::test]
async fn only_another_unit_of_the_part_with_its_value_joins_a_designator() {
    let (dir, _env) = stub_symbol_dir();
    let path = dir.path().join("units.kicad_sch");
    let ctx = test_ctx();

    handle_create_schematic(&json!({ "path": path.display().to_string() }), &ctx)
        .await
        .unwrap();
    for (unit, x) in [(1, 100.0), (2, 150.0)] {
        let placed = handle_add_schematic_component(
            &json!({
                "schematic": path.display().to_string(),
                "lib_id": "Device:OPAMP_DUAL",
                "x": x, "y": 80.0,
                "reference": "U1",
                "unit": unit
            }),
            &ctx,
        )
        .await
        .unwrap();
        assert!(!placed.is_error, "unit {unit}: {}", content_text(&placed));
    }
    let before = std::fs::read_to_string(&path).unwrap();
    for (lib_id, unit, value) in [
        ("Device:OPAMP_DUAL", 2, None),
        ("Device:OPAMP_DUAL", 3, Some("LM358")),
        ("Device:R", 1, None),
    ] {
        let mut args = json!({
            "schematic": path.display().to_string(),
            "lib_id": lib_id,
            "x": 200.0, "y": 80.0,
            "reference": "U1",
            "unit": unit
        });
        if let Some(value) = value {
            args["value"] = json!(value);
        }
        let refused = handle_add_schematic_component(&args, &ctx).await.unwrap();
        assert_eq!(
            crate::mcp::error::extract_error_kind(&refused).as_deref(),
            Some("invalid_argument"),
            "{lib_id} unit {unit}: {}",
            content_text(&refused)
        );
        let body: serde_json::Value = serde_json::from_str(&content_text(&refused)).unwrap();
        assert_eq!(body["error"]["field"], "reference");
    }
    assert_eq!(std::fs::read_to_string(&path).unwrap(), before);
}

#[tokio::test]
async fn pin_locations_are_unit_aware() {
    // The #35 repro: an LM2904-style dual op-amp placed as unit 1 and as
    // unit 2 must report DISJOINT pin sets, not all units superimposed.
    let (dir, _env) = stub_symbol_dir();
    let path = dir.path().join("dual.kicad_sch");
    let ctx = test_ctx();

    handle_create_schematic(&json!({ "path": path.display().to_string() }), &ctx)
        .await
        .unwrap();
    for (reference, unit, x) in [("U1", 1, 100.0), ("U2", 2, 150.0)] {
        let res = handle_add_schematic_component(
            &json!({
                "schematic": path.display().to_string(),
                "lib_id": "Device:OPAMP_DUAL",
                "x": x, "y": 80.0,
                "reference": reference,
                "unit": unit
            }),
            &ctx,
        )
        .await
        .unwrap();
        assert!(!res.is_error, "placing {reference}: {:?}", res.content);
    }

    let pin_numbers = |res: &CallToolResult| -> Vec<String> {
        let out: serde_json::Value = serde_json::from_str(&content_text(res)).unwrap();
        let mut nums: Vec<String> = out["pins"]
            .as_array()
            .unwrap()
            .iter()
            .map(|p| p["number"].as_str().unwrap().to_string())
            .collect();
        nums.sort();
        nums
    };

    let u1 = handle_get_schematic_pin_locations(
        &json!({ "schematic": path.display().to_string(), "reference": "U1" }),
        &ctx,
    )
    .await
    .unwrap();
    assert!(!u1.is_error);
    assert_eq!(pin_numbers(&u1), vec!["1", "2", "3"], "unit 1 pins only");

    let u2 = handle_get_schematic_pin_locations(
        &json!({ "schematic": path.display().to_string(), "reference": "U2" }),
        &ctx,
    )
    .await
    .unwrap();
    assert!(!u2.is_error);
    assert_eq!(pin_numbers(&u2), vec!["5", "6", "7"], "unit 2 pins only");

    // Batch variant agrees.
    let batch = handle_batch_get_pin_locations(
        &json!({
            "schematic": path.display().to_string(),
            "references": ["U1", "U2"]
        }),
        &ctx,
    )
    .await
    .unwrap();
    let out: serde_json::Value = serde_json::from_str(&content_text(&batch)).unwrap();
    let comps = out["components"].as_array().unwrap();
    let nums = |i: usize| -> Vec<String> {
        let mut v: Vec<String> = comps[i]["pins"]
            .as_array()
            .unwrap()
            .iter()
            .map(|p| p["number"].as_str().unwrap().to_string())
            .collect();
        v.sort();
        v
    };
    assert_eq!(nums(0), vec!["1", "2", "3"]);
    assert_eq!(nums(1), vec!["5", "6", "7"]);
}

#[tokio::test]
async fn pin_locations_error_on_extends_stub_with_zero_pins() {
    // A pre-flattening schematic: the embedded definition for the derived
    // symbol is an (extends "Parent") stub with no pins. The #34 guard
    // only catches MISSING definitions; a resolving-but-pinless stub must
    // be a structured error too, not pins:[] (#35).
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("stub.kicad_sch");
    std::fs::write(
            &path,
            "(kicad_sch\n\t(version 20250610)\n\t(generator \"konnect\")\n\t(uuid \"11111111-2222-3333-4444-555555555555\")\n\t(lib_symbols\n\t\t(symbol \"Device:OPAMP_DERIVED\"\n\t\t\t(extends \"Device:OPAMP_DUAL\")\n\t\t\t(property \"Reference\" \"U\" (at 0 0 0))\n\t\t)\n\t)\n\t(symbol\n\t\t(lib_id \"Device:OPAMP_DERIVED\")\n\t\t(at 100 80 0)\n\t\t(unit 1)\n\t\t(uuid \"aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee\")\n\t\t(property \"Reference\" \"U1\"\n\t\t\t(at 102 78 0)\n\t\t)\n\t)\n)\n",
        )
        .unwrap();
    let ctx = test_ctx();

    let res = handle_get_schematic_pin_locations(
        &json!({ "schematic": path.display().to_string(), "reference": "U1" }),
        &ctx,
    )
    .await
    .unwrap();
    assert!(res.is_error, "extends stub with zero pins must be an error");
    let text = content_text(&res);
    assert!(
        text.contains("Device:OPAMP_DERIVED"),
        "error must name the lib_id: {text}"
    );
    assert!(
        text.contains("Device:OPAMP_DUAL"),
        "error must name the extends target: {text}"
    );

    // Batch variant reports it per-entry.
    let batch = handle_batch_get_pin_locations(
        &json!({
            "schematic": path.display().to_string(),
            "references": ["U1"]
        }),
        &ctx,
    )
    .await
    .unwrap();
    let out: serde_json::Value = serde_json::from_str(&content_text(&batch)).unwrap();
    let err = out["components"][0]["error"].as_str().unwrap_or("");
    assert!(
        err.contains("Device:OPAMP_DUAL"),
        "batch entry must carry the stub error: {out}"
    );
}

#[tokio::test]
async fn pin_locations_resolve_through_lib_name_not_lib_id() {
    // eeschema stores a locally edited library symbol under a derived name
    // and points the instance at it with (lib_name …). Resolving on lib_id
    // alone picks the *base* definition, whose pins sit elsewhere — the
    // wrong answer is returned silently, and every wire placed from it
    // lands off-pin (#143).
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("derived.kicad_sch");
    std::fs::write(
            &path,
            "(kicad_sch\n\t(version 20250114)\n\t(generator \"eeschema\")\n\t(uuid \"11111111-2222-3333-4444-555555555555\")\n\t(lib_symbols\n\t\t(symbol \"Device:R\"\n\t\t\t(symbol \"R_1_1\"\n\t\t\t\t(pin passive line (at 0 3.81 270) (length 1.27) (name \"~\") (number \"1\"))\n\t\t\t)\n\t\t)\n\t\t(symbol \"R_1\"\n\t\t\t(symbol \"R_1_1_1\"\n\t\t\t\t(pin passive line (at 0 6.35 270) (length 1.27) (name \"~\") (number \"1\"))\n\t\t\t)\n\t\t)\n\t\t(symbol \"C_1\"\n\t\t\t(symbol \"C_1_1_1\"\n\t\t\t\t(pin passive line (at 0 3.81 270) (length 3.048) (name \"~\") (number \"1\"))\n\t\t\t)\n\t\t)\n\t)\n\t(symbol\n\t\t(lib_name \"R_1\")\n\t\t(lib_id \"Device:R\")\n\t\t(at 88.9 63.5 0)\n\t\t(unit 1)\n\t\t(uuid \"aaaaaaaa-bbbb-cccc-dddd-000000000001\")\n\t\t(property \"Reference\" \"R2\" (at 91.44 62.23 0))\n\t)\n\t(symbol\n\t\t(lib_name \"C_1\")\n\t\t(lib_id \"Device:C\")\n\t\t(at 139.7 63.5 0)\n\t\t(unit 1)\n\t\t(uuid \"aaaaaaaa-bbbb-cccc-dddd-000000000002\")\n\t\t(property \"Reference\" \"C1\" (at 142.24 62.23 0))\n\t)\n)\n",
        )
        .unwrap();
    let ctx = test_ctx();

    let res = handle_get_schematic_pin_locations(
        &json!({ "schematic": path.display().to_string(), "reference": "R2" }),
        &ctx,
    )
    .await
    .unwrap();
    assert!(!res.is_error, "{}", content_text(&res));
    let out: serde_json::Value = serde_json::from_str(&content_text(&res)).unwrap();
    // R_1's pin sits at local +6.35 => 63.5 - 6.35; Device:R's would be
    // 63.5 - 3.81 = 59.69.
    assert_eq!(out["pins"][0]["y"].as_f64().unwrap(), 57.15);

    // Device:C is not embedded at all — only the derived C_1 is. Matching
    // on lib_id reported "no embedded definition ... nonexistent lib_id",
    // which is both wrong and dangerous advice.
    let batch = handle_batch_get_pin_locations(
        &json!({
            "schematic": path.display().to_string(),
            "references": ["C1"]
        }),
        &ctx,
    )
    .await
    .unwrap();
    let out: serde_json::Value = serde_json::from_str(&content_text(&batch)).unwrap();
    assert!(
        out["components"][0]["error"].is_null(),
        "C1 must resolve through C_1: {out}"
    );
    assert_eq!(
        out["components"][0]["pins"][0]["y"].as_f64().unwrap(),
        59.69
    );
}

#[tokio::test]
async fn replace_component_sets_validated_unit() {
    let (dir, _env) = stub_symbol_dir();
    let path = dir.path().join("swap.kicad_sch");
    let ctx = test_ctx();

    handle_create_schematic(&json!({ "path": path.display().to_string() }), &ctx)
        .await
        .unwrap();
    handle_add_schematic_component(
        &json!({
            "schematic": path.display().to_string(),
            "lib_id": "Device:OPAMP_DUAL",
            "x": 100.0, "y": 80.0,
            "reference": "U1",
            "unit": 1
        }),
        &ctx,
    )
    .await
    .unwrap();

    // Out-of-range unit on the new symbol is rejected before any write.
    let before = std::fs::read_to_string(&path).unwrap();
    let bad = handle_replace_component(
        &json!({
            "schematic": path.display().to_string(),
            "reference": "U1",
            "new_lib_id": "Device:OPAMP_DUAL",
            "unit": 99
        }),
        &ctx,
    )
    .await
    .unwrap();
    assert!(bad.is_error, "unit 99 must be rejected");
    assert!(content_text(&bad).contains("3 unit"));
    assert_eq!(std::fs::read_to_string(&path).unwrap(), before);

    // Valid unit is written to the symbol and its instances entry.
    let ok = handle_replace_component(
        &json!({
            "schematic": path.display().to_string(),
            "reference": "U1",
            "new_lib_id": "Device:OPAMP_DUAL",
            "unit": 2
        }),
        &ctx,
    )
    .await
    .unwrap();
    assert!(!ok.is_error, "{:?}", ok.content);
    let raw = std::fs::read_to_string(&path).unwrap();
    assert!(
        raw.contains("(unit 2)"),
        "unit must be updated to 2:\n{raw}"
    );
    assert!(
        !raw.contains("(unit 1)"),
        "no stale (unit 1) may remain in the instance:\n{raw}"
    );
    let sch = cse::Schematic::load(&path).unwrap();
    assert_eq!(sch.symbols.by_reference("U1").unwrap().unit, 2);
}

#[tokio::test]
async fn add_component_repairs_legacy_file_without_root_uuid() {
    let (dir, _env) = stub_symbol_dir();
    let path = dir.path().join("legacy.kicad_sch");
    // File shape produced by Konnect before root UUIDs were written.
    std::fs::write(
            &path,
            "(kicad_sch\n\t(version 20250610)\n\t(generator \"konnect\")\n\t(generator_version \"10.0\")\n\t(paper \"A4\")\n\t(lib_symbols\n\t)\n)\n",
        )
        .unwrap();
    let ctx = test_ctx();

    let result = handle_add_schematic_component(
        &json!({
            "schematic": path.display().to_string(),
            "lib_id": "Device:R",
            "x": 50.0, "y": 50.0,
            "reference": "R1"
        }),
        &ctx,
    )
    .await
    .unwrap();
    assert!(!result.is_error);

    let sch = cse::Schematic::load(&path).unwrap();
    let root_uuid = sch.uuid.clone().expect("legacy file gains a root uuid");
    let sym = sch.symbols.by_reference("R1").unwrap();
    assert!(sym.has_instance_path("legacy", &format!("/{}", root_uuid)));
}

#[tokio::test]
async fn add_component_with_nonexistent_lib_id_errors_with_suggestion() {
    let (dir, _env) = stub_symbol_dir();
    let path = dir.path().join("ghost.kicad_sch");
    let ctx = test_ctx();

    handle_create_schematic(&json!({ "path": path.display().to_string() }), &ctx)
        .await
        .unwrap();
    let before = std::fs::read_to_string(&path).unwrap();

    // Device:CP is the KiCAD ≤9 name; 10 renamed it to C_Polarized (#34).
    let result = handle_add_schematic_component(
        &json!({
            "schematic": path.display().to_string(),
            "lib_id": "Device:CP",
            "x": 100.0, "y": 80.0,
            "reference": "C1"
        }),
        &ctx,
    )
    .await
    .unwrap();
    assert!(result.is_error, "nonexistent lib_id must be an error");
    let msg = format!("{:?}", result.content);
    assert!(msg.contains("Device:CP"), "names the bad lib_id: {msg}");
    assert!(
        msg.contains("C_Polarized"),
        "did-you-mean should surface the rename: {msg}"
    );

    // And nothing was written: no ghost instance in the file.
    assert_eq!(std::fs::read_to_string(&path).unwrap(), before);
}

#[tokio::test]
async fn add_component_with_unknown_library_says_so() {
    let (dir, _env) = stub_symbol_dir();
    let path = dir.path().join("nolib.kicad_sch");
    let ctx = test_ctx();

    handle_create_schematic(&json!({ "path": path.display().to_string() }), &ctx)
        .await
        .unwrap();
    let result = handle_add_schematic_component(
        &json!({
            "schematic": path.display().to_string(),
            "lib_id": "Transistor_FET_xyzzy:IRF830",
            "x": 100.0, "y": 80.0
        }),
        &ctx,
    )
    .await
    .unwrap();
    assert!(result.is_error);
    let msg = format!("{:?}", result.content);
    assert!(
        msg.contains("Library 'Transistor_FET_xyzzy' not found"),
        "distinguishes missing library from missing symbol: {msg}"
    );
}

#[tokio::test]
async fn pin_locations_error_when_definition_not_embedded() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("noembed.kicad_sch");
    // A symbol instance whose lib_id has NO lib_symbols entry — the file
    // shape a ghost lib_id used to leave behind (#34).
    std::fs::write(
            &path,
            "(kicad_sch\n\t(version 20250610)\n\t(generator \"konnect\")\n\t(uuid \"11111111-2222-3333-4444-555555555555\")\n\t(lib_symbols\n\t)\n\t(symbol\n\t\t(lib_id \"Device:CP\")\n\t\t(at 100 80 0)\n\t\t(unit 1)\n\t\t(uuid \"aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee\")\n\t\t(property \"Reference\" \"C1\"\n\t\t\t(at 102 78 0)\n\t\t)\n\t)\n)\n",
        )
        .unwrap();
    let ctx = test_ctx();

    let result = handle_get_schematic_pin_locations(
        &json!({
            "schematic": path.display().to_string(),
            "reference": "C1"
        }),
        &ctx,
    )
    .await
    .unwrap();
    assert!(
        result.is_error,
        "missing embedded definition must be an error, not pins: []"
    );
    let msg = format!("{:?}", result.content);
    assert!(msg.contains("Device:CP"));
    assert!(msg.contains("no embedded definition"));
}

/// Fields follow the library anchor through the instance rotation (#101).
/// `Device:R` anchors Reference beside the body at (2.032, 0) rotated 90°,
/// so an upright resistor labels its right-hand side vertically and a
/// 90°-rotated one labels above, horizontally — a fixed ±3.81 offset at 0°
/// put both beside the wrong edge.
async fn place_rotated_resistor(rotation: f64) -> (String, String) {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("rot.kicad_sch");
    std::fs::write(
            &path,
            "(kicad_sch\n  (version 20250610)\n  (generator \"konnect\")\n  (uuid \"aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee\")\n  (paper \"A4\")\n  (lib_symbols\n    (symbol \"Device:R\"\n      (property \"Reference\" \"R\" (at 2.032 0 90))\n      (property \"Value\" \"R\" (at 0 0 90))\n    )\n  )\n)\n",
        )
        .unwrap();

    let result = handle_add_schematic_component(
        &json!({
            "schematic": path.display().to_string(),
            "lib_id": "Device:R",
            // Already on the 1.27mm grid the placement snaps to, so the
            // expected field coordinates are the anchors plus the origin.
            "x": 101.6,
            "y": 50.8,
            "rotation": rotation,
            "reference": "R1",
            "value": "10k"
        }),
        &test_ctx(),
    )
    .await
    .unwrap();
    assert!(!result.is_error, "{result:?}");

    let sch = cse::Schematic::load(&path).unwrap();
    let sym = sch
        .symbols
        .iter()
        .find(|s| s.reference() == Some("R1"))
        .expect("placed resistor");
    let field = |name: &str| {
        cse::sexp::writer::write(
            &sym.properties
                .iter()
                .find(|p| p.name == name)
                .unwrap()
                .to_sexp(),
        )
    };
    (field("Reference"), field("Value"))
}

/// An anchor without its justification collides: this symbol anchors
/// Reference and Value on the same row and relies on `justify left` to
/// keep `U2` off `AP2112K-3.3`. Device:R, which the tests above place,
/// justifies nothing — centred stays spelled as no `(justify …)`.
#[tokio::test]
async fn placement_carries_the_librarys_field_justification() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("justify.kicad_sch");
    std::fs::write(
            &path,
            "(kicad_sch\n  (version 20250610)\n  (generator \"konnect\")\n  (uuid \"aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee\")\n  (paper \"A4\")\n  (lib_symbols\n    (symbol \"Regulator_Linear:AP2112K-3.3\"\n      (property \"Reference\" \"U\" (at -5.08 5.715 0) (effects (font (size 1.27 1.27)) (justify left)))\n      (property \"Value\" \"AP2112K-3.3\" (at 0 5.715 0) (effects (font (size 1.27 1.27)) (justify left)))\n    )\n  )\n)\n",
        )
        .unwrap();

    let result = handle_add_schematic_component(
        &json!({
            "schematic": path.display().to_string(),
            "lib_id": "Regulator_Linear:AP2112K-3.3",
            "x": 101.6,
            "y": 50.8,
            "reference": "U2",
            "value": "AP2112K-3.3"
        }),
        &test_ctx(),
    )
    .await
    .unwrap();
    assert!(!result.is_error, "{result:?}");

    let sch = cse::Schematic::load(&path).unwrap();
    let sym = sch
        .symbols
        .iter()
        .find(|s| s.reference() == Some("U2"))
        .expect("placed regulator");
    let field = |name: &str| {
        cse::sexp::writer::write(
            &sym.properties
                .iter()
                .find(|p| p.name == name)
                .unwrap()
                .to_sexp(),
        )
    };
    for name in ["Reference", "Value"] {
        let written = field(name);
        assert!(
            written.contains("(justify left)"),
            "{name} must keep the library's justification: {written}"
        );
    }
    // Hidden fields have no library anchor here, so they stay centred.
    assert!(!field("Footprint").contains("justify"));

    let (reference, _) = place_rotated_resistor(0.0).await;
    assert!(
        !reference.contains("justify"),
        "a centred library field must not gain a justify: {reference}"
    );
}

#[tokio::test]
async fn placement_copies_library_datasheet_and_description() {
    // Pre-seed two real KiCad field shapes so this remains independent of
    // an installed symbol library: one URL and one no-datasheet sentinel.
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("metadata.kicad_sch");
    std::fs::write(
            &path,
            "(kicad_sch\n  (version 20250610)\n  (generator \"konnect\")\n  (uuid \"aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee\")\n  (paper \"A4\")\n  (lib_symbols\n    (symbol \"Device:R\"\n      (property \"Reference\" \"R\" (at 2.032 0 90))\n      (property \"Value\" \"R\" (at 0 0 90))\n      (property \"Datasheet\" \"https://example.com/resistor.pdf\" (at 0 0 0))\n      (property \"Description\" \"Resistor\" (at 0 0 0))\n    )\n    (symbol \"Device:C\"\n      (property \"Reference\" \"C\" (at 2.032 0 90))\n      (property \"Value\" \"C\" (at 0 0 90))\n      (property \"Datasheet\" \"~\" (at 0 0 0))\n    )\n  )\n)\n",
        )
        .unwrap();

    for (lib_id, reference, x) in [("Device:R", "R1", 100.0), ("Device:C", "C1", 120.0)] {
        let result = handle_add_schematic_component(
            &json!({
                "schematic": path.display().to_string(),
                "lib_id": lib_id,
                "reference": reference,
                "x": x,
                "y": 50.0
            }),
            &test_ctx(),
        )
        .await
        .unwrap();
        assert!(!result.is_error, "{result:?}");
    }

    let sch = cse::Schematic::load(&path).unwrap();
    let field = |reference: &str, name: &str| {
        sch.symbols
            .iter()
            .find(|symbol| symbol.reference() == Some(reference))
            .and_then(|symbol| symbol.properties.iter().find(|p| p.name == name))
            .map(|property| property.value.as_str())
    };
    assert_eq!(
        field("R1", "Datasheet"),
        Some("https://example.com/resistor.pdf")
    );
    assert_eq!(field("R1", "Description"), Some("Resistor"));
    assert_eq!(field("C1", "Datasheet"), Some("~"));
    assert_eq!(
        field("C1", "Description"),
        Some(""),
        "KiCad writes the mandatory Description field even when empty"
    );
}

#[tokio::test]
async fn placement_carries_library_custom_fields_automatically() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("custom_fields.kicad_sch");
    std::fs::write(
            &path,
            "(kicad_sch\n  (version 20250610)\n  (generator \"konnect\")\n  (uuid \"aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee\")\n  (paper \"A4\")\n  (lib_symbols\n    (symbol \"Custom:IC\"\n      (property \"Reference\" \"U\" (at 0 0 0))\n      (property \"Value\" \"IC\" (at 0 0 0))\n      (property \"Datasheet\" \"~\" (at 0 0 0))\n      (property \"Description\" \"Custom IC\" (at 0 0 0))\n      (property \"LCSC\" \"C9999\" (at 0 0 0))\n      (property \"MPN\" \"MY-PART-100\" (at 0 0 0))\n      (property \"Manufacturer\" \"Acme Corp\" (at 0 0 0))\n      (property \"ki_keywords\" \"ic internal\" (at 0 0 0))\n    )\n  )\n)\n",
        )
        .unwrap();

    let result = handle_add_schematic_component(
        &json!({
            "schematic": path.display().to_string(),
            "lib_id": "Custom:IC",
            "reference": "U1",
            "x": 100.0,
            "y": 50.0
        }),
        &test_ctx(),
    )
    .await
    .unwrap();
    assert!(!result.is_error, "{result:?}");

    let sch = cse::Schematic::load(&path).unwrap();
    let field = |name: &str| {
        sch.symbols
            .iter()
            .find(|s| s.reference() == Some("U1"))
            .and_then(|s| s.properties.iter().find(|p| p.name == name))
            .map(|p| p.value.as_str())
    };
    assert_eq!(field("LCSC"), Some("C9999"));
    assert_eq!(field("MPN"), Some("MY-PART-100"));
    assert_eq!(field("Manufacturer"), Some("Acme Corp"));
    assert_eq!(
        field("ki_keywords"),
        None,
        "ki_* properties must not be copied"
    );
}

/// `lcsc_id` fills fields from the catalog, so a build without it doesn't
/// offer the argument, in either placement tool (#210).
#[test]
fn placement_offers_lcsc_id_only_with_the_catalog() {
    for (toolset, name) in [
        ("sch_components", "add_schematic_component"),
        ("sch_batch", "batch_place_components"),
    ] {
        let tool = crate::router::registry::tools_for(toolset)
            .unwrap()
            .into_iter()
            .find(|tool| tool.name == name)
            .unwrap();
        let schema = tool.input_schema.to_string();
        assert_eq!(
            schema.contains("\"lcsc_id\""),
            cfg!(feature = "jlcpcb"),
            "{name}"
        );
        assert!(schema.contains("\"fields\""), "{name}");
    }
}

#[tokio::test]
#[cfg(feature = "jlcpcb")]
async fn placement_enriches_from_jlcpcb_db_using_lcsc_id() {
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("jlcpcb.db");
    let conn = rusqlite::Connection::open(&db_path).unwrap();
    conn.execute(
            "CREATE TABLE components (LCSC TEXT, MFR_Part TEXT, Package TEXT, Manufacturer TEXT, Library_Type TEXT, Description TEXT, Datasheet TEXT, Price REAL, Stock INTEGER, Category TEXT)",
            [],
        ).unwrap();
    conn.execute(
            "INSERT INTO components VALUES ('C2856837', 'FPC-05FB-40PH20', '0.5mm', 'XUNPU', 'Extended', '0.5mm Pitch 40 Pin FPC Connector', 'https://example.com/fpc.pdf', 0.5, 100, 'Connectors')",
            [],
        ).unwrap();

    let path = dir.path().join("enrich.kicad_sch");
    std::fs::write(
            &path,
            "(kicad_sch\n  (version 20250610)\n  (generator \"konnect\")\n  (uuid \"aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee\")\n  (paper \"A4\")\n  (lib_symbols\n    (symbol \"Device:CONN\"\n      (property \"Reference\" \"J\" (at 0 0 0))\n      (property \"Value\" \"CONN\" (at 0 0 0))\n      (property \"Datasheet\" \"~\" (at 0 0 0))\n      (property \"Description\" \"\" (at 0 0 0))\n    )\n  )\n)\n",
        )
        .unwrap();

    let result = handle_add_schematic_component(
        &json!({
            "schematic": path.display().to_string(),
            "lib_id": "Device:CONN",
            "reference": "J1",
            "x": 100.0,
            "y": 50.0,
            "lcsc_id": "C2856837",
        }),
        &ctx_with_catalog(&db_path),
    )
    .await
    .unwrap();
    assert!(!result.is_error, "{result:?}");

    let sch = cse::Schematic::load(&path).unwrap();
    let field = |name: &str| {
        sch.symbols
            .iter()
            .find(|s| s.reference() == Some("J1"))
            .and_then(|s| s.properties.iter().find(|p| p.name == name))
            .map(|p| p.value.as_str())
    };
    assert_eq!(field("LCSC"), Some("C2856837"));
    assert_eq!(field("MPN"), Some("FPC-05FB-40PH20"));
    assert_eq!(field("Manufacturer"), Some("XUNPU"));
    assert_eq!(field("Datasheet"), Some("https://example.com/fpc.pdf"));

    // The response says the catalog was read and had the part (#209).
    let body: serde_json::Value = match result.content.first() {
        Some(crate::mcp::protocol::ToolContent::Text { text }) => {
            serde_json::from_str(text).unwrap()
        }
        other => panic!("expected text content, got {other:?}"),
    };
    assert_eq!(body["lcsc_catalog"]["read"], true, "{body}");
    assert_eq!(body["lcsc_catalog"]["part_found"], true, "{body}");
    assert_eq!(body["lcsc_catalog"]["path"], json!(db_path), "{body}");
    assert_eq!(
        field("Description"),
        Some("0.5mm Pitch 40 Pin FPC Connector")
    );
}

/// A catalog that can't be read doesn't stop the placement, and the
/// response says why the fields weren't filled, as every catalog tool
/// reports it (#209). Before, the lookup failed silently.
#[tokio::test]
#[cfg(feature = "jlcpcb")]
async fn placement_reports_an_unreadable_catalog() {
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("jlcpcb.db");
    std::fs::write(&db_path, "not a database").unwrap();
    let path = dir.path().join("unreadable.kicad_sch");
    std::fs::write(
            &path,
            "(kicad_sch\n  (version 20250610)\n  (generator \"konnect\")\n  (uuid \"aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee\")\n  (paper \"A4\")\n  (lib_symbols\n    (symbol \"Device:CONN\"\n      (property \"Reference\" \"J\" (at 0 0 0))\n      (property \"Value\" \"CONN\" (at 0 0 0))\n    )\n  )\n)\n",
        )
        .unwrap();

    let result = handle_add_schematic_component(
        &json!({
            "schematic": path.display().to_string(),
            "lib_id": "Device:CONN",
            "reference": "J1",
            "x": 100.0,
            "y": 50.0,
            "lcsc_id": "C2856837",
        }),
        &ctx_with_catalog(&db_path),
    )
    .await
    .unwrap();
    assert!(!result.is_error, "{result:?}");
    let body: serde_json::Value = match result.content.first() {
        Some(crate::mcp::protocol::ToolContent::Text { text }) => {
            serde_json::from_str(text).unwrap()
        }
        other => panic!("expected text content, got {other:?}"),
    };
    let report = &body["lcsc_catalog"];
    assert_eq!(report["read"], false, "{body}");
    assert!(report.get("part_found").is_none(), "{body}");
    assert!(
        report["reason"]
            .as_str()
            .unwrap()
            .starts_with("cannot read the catalog at "),
        "{body}"
    );
    let sch = cse::Schematic::load(&path).unwrap();
    let lcsc = sch
        .symbols
        .iter()
        .find(|s| s.reference() == Some("J1"))
        .and_then(|s| s.properties.iter().find(|p| p.name == "LCSC"))
        .map(|p| p.value.clone());
    assert_eq!(lcsc.as_deref(), Some("C2856837"));
}

#[tokio::test]
async fn placement_explicit_fields_override_and_add() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("fields_override.kicad_sch");
    std::fs::write(
            &path,
            "(kicad_sch\n  (version 20250610)\n  (generator \"konnect\")\n  (uuid \"aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee\")\n  (paper \"A4\")\n  (lib_symbols\n    (symbol \"Device:R\"\n      (property \"Reference\" \"R\" (at 0 0 0))\n      (property \"Value\" \"10k\" (at 0 0 0))\n      (property \"Datasheet\" \"~\" (at 0 0 0))\n      (property \"Description\" \"Resistor\" (at 0 0 0))\n    )\n  )\n)\n",
        )
        .unwrap();

    let result = handle_add_schematic_component(
        &json!({
            "schematic": path.display().to_string(),
            "lib_id": "Device:R",
            "reference": "R1",
            "x": 100.0,
            "y": 50.0,
            "fields": {
                "LCSC": "C12345",
                "Tolerance": "1%",
                "Power": "0.1W"
            }
        }),
        &test_ctx(),
    )
    .await
    .unwrap();
    assert!(!result.is_error, "{result:?}");

    let sch = cse::Schematic::load(&path).unwrap();
    let field = |name: &str| {
        sch.symbols
            .iter()
            .find(|s| s.reference() == Some("R1"))
            .and_then(|s| s.properties.iter().find(|p| p.name == name))
            .map(|p| p.value.as_str())
    };
    assert_eq!(field("LCSC"), Some("C12345"));
    assert_eq!(field("Tolerance"), Some("1%"));
    assert_eq!(field("Power"), Some("0.1W"));
}

#[tokio::test]
async fn unrotated_symbol_takes_the_librarys_field_anchors() {
    let (reference, value) = place_rotated_resistor(0.0).await;
    // Same numbers eeschema writes for this library symbol at (100, 50).
    assert!(
        reference.contains("(at 103.632 50.8 90)"),
        "Reference belongs beside the body, rotated: {reference}"
    );
    assert!(
        value.contains("(at 101.6 50.8 90)"),
        "Value belongs on the body's axis, rotated: {value}"
    );
}

#[tokio::test]
async fn rotated_symbol_carries_its_fields_around_with_it() {
    let (reference, value) = place_rotated_resistor(90.0).await;
    // The anchor rotates with the body: 2.032mm to the right of the
    // origin becomes 2.032mm above it. The stored angle stays at the
    // library's 90° — KiCad adds the symbol's rotation when it draws, so
    // this renders horizontally above the now-horizontal body.
    assert!(
        reference.contains("(at 101.6 48.768 90)"),
        "Reference must follow the rotated body: {reference}"
    );
    assert!(
        value.contains("(at 101.6 50.8 90)"),
        "Value must follow the rotated body: {value}"
    );
}

/// The repair path for sheets written before fields followed the library
/// (#101): an instance whose fields sit at the old fixed offset is put
/// back on its anchors, and a second run reports nothing left to move.
#[tokio::test]
async fn reset_field_positions_puts_stale_fields_back_on_their_anchors() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("stale-fields.kicad_sch");
    // A sheet as the old code wrote it: Reference at y-3.81 and Value at
    // y+3.81, while the library anchors them beside the body at 90.
    std::fs::write(
            &path,
            "(kicad_sch\n  (version 20250610)\n  (generator \"konnect\")\n  (uuid \"aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee\")\n  (paper \"A4\")\n  (lib_symbols\n    (symbol \"Device:R\"\n      (property \"Reference\" \"R\" (at 2.032 0 90))\n      (property \"Value\" \"R\" (at 0 0 90))\n    )\n  )\n  (symbol\n    (lib_id \"Device:R\")\n    (at 101.6 50.8 0)\n    (unit 1)\n    (uuid \"bbbbbbbb-bbbb-cccc-dddd-eeeeeeeeeeee\")\n    (property \"Reference\" \"R1\" (at 101.6 46.99 0))\n    (property \"Value\" \"10k\" (at 101.6 54.61 0))\n  )\n)\n",
        )
        .unwrap();

    let args = json!({ "schematic": path.display().to_string() });
    let dry = handle_reset_schematic_field_positions(
        &json!({
            "schematic": path.display().to_string(), "dry_run": true
        }),
        &test_ctx(),
    )
    .await
    .unwrap();
    let crate::mcp::protocol::ToolContent::Text { text } = &dry.content[0] else {
        panic!("expected text")
    };
    let body: serde_json::Value = serde_json::from_str(text).unwrap();
    assert_eq!(body["moved"], json!(["R1.Reference", "R1.Value"]));
    assert!(
        std::fs::read_to_string(&path).unwrap().contains("46.99"),
        "dry_run must not write"
    );

    let done = handle_reset_schematic_field_positions(&args, &test_ctx())
        .await
        .unwrap();
    assert!(!done.is_error, "{done:?}");

    let sch = cse::Schematic::load(&path).unwrap();
    let sym = sch.symbols.by_reference("R1").expect("R1");
    let field = |name: &str| {
        cse::sexp::writer::write(
            &sym.properties
                .iter()
                .find(|p| p.name == name)
                .unwrap()
                .to_sexp(),
        )
    };
    assert!(
        field("Reference").contains("(at 103.632 50.8 90)"),
        "{}",
        field("Reference")
    );
    assert!(
        field("Value").contains("(at 101.6 50.8 90)"),
        "{}",
        field("Value")
    );

    // Idempotent: nothing left to move on a second pass.
    let again = handle_reset_schematic_field_positions(&args, &test_ctx())
        .await
        .unwrap();
    let crate::mcp::protocol::ToolContent::Text { text } = &again.content[0] else {
        panic!("expected text")
    };
    let body: serde_json::Value = serde_json::from_str(text).unwrap();
    assert_eq!(body["moved"], json!([]));
    assert_eq!(body["unchanged"], json!(["R1.Reference", "R1.Value"]));
}

/// A reference that is not in the sheet is reported rather than silently
/// doing nothing.
#[tokio::test]
async fn reset_field_positions_reports_an_unknown_reference() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("one.kicad_sch");
    std::fs::write(
            &path,
            "(kicad_sch\n  (version 20250610)\n  (generator \"konnect\")\n  (uuid \"aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee\")\n  (paper \"A4\")\n  (lib_symbols\n    (symbol \"Device:R\"\n      (property \"Reference\" \"R\" (at 2.032 0 90))\n      (property \"Value\" \"R\" (at 0 0 90))\n    )\n  )\n  (symbol\n    (lib_id \"Device:R\")\n    (at 101.6 50.8 0)\n    (unit 1)\n    (uuid \"bbbbbbbb-bbbb-cccc-dddd-eeeeeeeeeeee\")\n    (property \"Reference\" \"R1\" (at 101.6 46.99 0))\n    (property \"Value\" \"10k\" (at 101.6 54.61 0))\n  )\n)\n",
        )
        .unwrap();

    let result = handle_reset_schematic_field_positions(
        &json!({
            "schematic": path.display().to_string(), "references": ["R9"]
        }),
        &test_ctx(),
    )
    .await
    .unwrap();
    let crate::mcp::protocol::ToolContent::Text { text } = &result.content[0] else {
        panic!("expected text")
    };
    let body: serde_json::Value = serde_json::from_str(text).unwrap();
    assert_eq!(body["not_found"], json!(["R9"]));
    assert_eq!(body["moved"], json!([]));
}

/// `not_found` is built from a HashSet, whose iteration order varies run
/// to run — several unknown references would come back in a different
/// order each call unless it is sorted.
#[tokio::test]
async fn reset_field_positions_reports_unknown_references_in_a_stable_order() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("stable.kicad_sch");
    std::fs::write(
            &path,
            "(kicad_sch\n  (version 20250610)\n  (generator \"konnect\")\n  (uuid \"aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee\")\n  (paper \"A4\")\n  (lib_symbols\n    (symbol \"Device:R\"\n      (property \"Reference\" \"R\" (at 2.032 0 90))\n      (property \"Value\" \"R\" (at 0 0 90))\n    )\n  )\n  (symbol\n    (lib_id \"Device:R\")\n    (at 101.6 50.8 0)\n    (unit 1)\n    (uuid \"bbbbbbbb-bbbb-cccc-dddd-eeeeeeeeeeee\")\n    (property \"Reference\" \"R1\" (at 101.6 46.99 0))\n    (property \"Value\" \"10k\" (at 101.6 54.61 0))\n  )\n)\n",
        )
        .unwrap();

    // Repeated because a HashSet of this size reorders between runs; an
    // unsorted list passes once and then does not.
    for _ in 0..8 {
        let result = handle_reset_schematic_field_positions(
            &json!({
                "schematic": path.display().to_string(),
                "references": ["R9", "R2", "U7", "C3"]
            }),
            &test_ctx(),
        )
        .await
        .unwrap();
        let crate::mcp::protocol::ToolContent::Text { text } = &result.content[0] else {
            panic!("expected text")
        };
        let body: serde_json::Value = serde_json::from_str(text).unwrap();
        assert_eq!(body["not_found"], json!(["C3", "R2", "R9", "U7"]));
    }
}

/// A field the library anchors but the placed symbol does not carry is
/// reported, not skipped in silence — an unreported skip reads as "reset".
#[tokio::test]
async fn reset_field_positions_reports_a_field_the_symbol_does_not_have() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("no-value.kicad_sch");
    // The library anchors Reference and Value; the instance has only
    // Reference.
    std::fs::write(
            &path,
            "(kicad_sch\n  (version 20250610)\n  (generator \"konnect\")\n  (uuid \"aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee\")\n  (paper \"A4\")\n  (lib_symbols\n    (symbol \"Device:R\"\n      (property \"Reference\" \"R\" (at 2.032 0 90))\n      (property \"Value\" \"R\" (at 0 0 90))\n    )\n  )\n  (symbol\n    (lib_id \"Device:R\")\n    (at 101.6 50.8 0)\n    (unit 1)\n    (uuid \"bbbbbbbb-bbbb-cccc-dddd-eeeeeeeeeeee\")\n    (property \"Reference\" \"R1\" (at 101.6 46.99 0))\n  )\n)\n",
        )
        .unwrap();

    let result = handle_reset_schematic_field_positions(
        &json!({ "schematic": path.display().to_string() }),
        &test_ctx(),
    )
    .await
    .unwrap();
    let crate::mcp::protocol::ToolContent::Text { text } = &result.content[0] else {
        panic!("expected text")
    };
    let body: serde_json::Value = serde_json::from_str(text).unwrap();
    assert_eq!(body["moved"], json!(["R1.Reference"]));
    assert_eq!(
        body["no_property"],
        json!(["R1.Value"]),
        "the skipped field must be accounted for: {body}"
    );
}

#[tokio::test]
async fn add_schematic_component_hides_power_reference() {
    // Pre-seed lib_symbols so ensure_lib_symbol succeeds without KiCad.
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("power-via-add.kicad_sch");
    std::fs::write(
            &path,
            "(kicad_sch\n  (version 20250610)\n  (generator \"konnect\")\n  (uuid \"aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee\")\n  (paper \"A4\")\n  (lib_symbols\n    (symbol \"power:GND\"\n      (property \"Reference\" \"#PWR\" (at 0 0 0) (hide yes))\n      (property \"Value\" \"GND\" (at 0 0 0))\n    )\n  )\n)\n",
        )
        .unwrap();

    let result = handle_add_schematic_component(
        &json!({
            "schematic": path.display().to_string(),
            "lib_id": "power:GND",
            "x": 50.0,
            "y": 60.0,
            "reference": "#PWR010",
            "value": "GND"
        }),
        &test_ctx(),
    )
    .await
    .unwrap();
    assert!(!result.is_error, "{result:?}");

    let sch = cse::Schematic::load(&path).unwrap();
    let sym = sch
        .symbols
        .iter()
        .find(|s| s.reference() == Some("#PWR010"))
        .expect("power instance");
    let ref_sexp = cse::sexp::writer::write(
        &sym.properties
            .iter()
            .find(|p| p.name == "Reference")
            .unwrap()
            .to_sexp(),
    );
    let hide_at = ref_sexp.find("(hide yes)").expect("property-level hide");
    let effects_at = ref_sexp.find("(effects").expect("effects");
    assert!(
        hide_at < effects_at,
        "power: via add_schematic_component must hide Reference like add_power_symbol: {ref_sexp}"
    );
    let val_sexp = cse::sexp::writer::write(
        &sym.properties
            .iter()
            .find(|p| p.name == "Value")
            .unwrap()
            .to_sexp(),
    );
    assert!(
        !val_sexp.contains("hide"),
        "Value stays visible: {val_sexp}"
    );
}

/// Field visibility comes from the library definition, not from the `power:`
/// prefix (#853): a logo whose library hides Reference and Value is placed
/// hidden, a non-power `PWR_FLAG` keeps its library-hidden Reference hidden,
/// and a library that shows Value keeps it shown.
#[tokio::test]
async fn add_schematic_component_takes_field_visibility_from_library() {
    let (symdir, _env) = stub_symbol_dir();
    let path = symdir.path().join("visibility-from-library.kicad_sch");
    std::fs::write(
        &path,
        "(kicad_sch\n  (version 20250610)\n  (generator \"konnect\")\n  (uuid \"aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee\")\n  (paper \"A4\")\n  (lib_symbols\n    (symbol \"Graphic:Logo_Open_Hardware_Small\"\n      (property \"Reference\" \"#SYM\" (at 0 0 0) (hide yes))\n      (property \"Value\" \"Logo_Open_Hardware_Small\" (at 0 0 0) (hide yes))\n    )\n    (symbol \"MyPower:PWR_FLAG\"\n      (property \"Reference\" \"#FLG\" (at 0 0 0) (hide yes))\n      (property \"Value\" \"PWR_FLAG\" (at 0 0 0))\n    )\n  )\n)\n",
    )
    .unwrap();

    for (lib_id, reference) in [
        ("Graphic:Logo_Open_Hardware_Small", "#SYM1"),
        ("MyPower:PWR_FLAG", "#FLG02"),
        ("Device:WITH_LEGACY_HIDDEN_FIELD", "U3"),
    ] {
        let result = handle_add_schematic_component(
            &json!({
                "schematic": path.display().to_string(),
                "lib_id": lib_id,
                "x": 50.0,
                "y": 60.0,
                "reference": reference
            }),
            &test_ctx(),
        )
        .await
        .unwrap();
        assert!(!result.is_error, "{result:?}");
    }

    let sch = cse::Schematic::load(&path).unwrap();
    let field_sexp = |sym: &cse::Symbol, name: &str| {
        cse::sexp::writer::write(
            &sym.properties
                .iter()
                .find(|p| p.name == name)
                .unwrap()
                .to_sexp(),
        )
    };
    let logo = sch
        .symbols
        .iter()
        .find(|s| s.reference() == Some("#SYM1"))
        .expect("logo instance");
    assert!(
        field_sexp(logo, "Reference").contains("(hide yes)"),
        "the library hides the logo Reference, so the placed Reference must be hidden: {}",
        field_sexp(logo, "Reference")
    );
    assert!(
        field_sexp(logo, "Value").contains("(hide yes)"),
        "the library hides the logo Value, so the placed Value must be hidden: {}",
        field_sexp(logo, "Value")
    );

    let flag = sch
        .symbols
        .iter()
        .find(|s| s.reference() == Some("#FLG02"))
        .expect("PWR_FLAG instance");
    assert!(
        field_sexp(flag, "Reference").contains("(hide yes)"),
        "the library hides the PWR_FLAG Reference, so the placed Reference must be hidden: {}",
        field_sexp(flag, "Reference")
    );
    assert!(
        !field_sexp(flag, "Value").contains("hide"),
        "the library shows the PWR_FLAG Value, so it must stay shown: {}",
        field_sexp(flag, "Value")
    );

    let legacy = sch
        .symbols
        .iter()
        .find(|s| s.reference() == Some("U3"))
        .expect("legacy-hidden-field instance");
    assert!(
        field_sexp(legacy, "Value").contains("(hide yes)"),
        "the KiCad 6/7 library hides Value with a bare hide inside effects: {}",
        field_sexp(legacy, "Value")
    );
}

/// A schematic keeps its own copy of every symbol, so editing the library
/// leaves the sheet drawing the old shape — what KiCad reports as
/// "doesn't match copy in library".
#[tokio::test]
async fn update_symbols_from_library_refreshes_a_stale_embedded_copy() {
    // In the stub project dir, so its sym-lib-table shadows the global
    // `Device` entry — off a developer's KiCad install, that entry resolves
    // and the edit below then lands on a library nothing reads.
    let (symdir, _env) = stub_symbol_dir();
    let path = symdir.path().join("stale.kicad_sch");
    let ctx = test_ctx();
    handle_create_schematic(&json!({ "path": path.display().to_string() }), &ctx)
        .await
        .unwrap();
    let placed = handle_add_schematic_component(
        &json!({ "schematic": path.display().to_string(), "lib_id": "Device:R",
                     "reference": "R1", "x": 100.0, "y": 100.0 }),
        &ctx,
    )
    .await
    .unwrap();
    assert!(!placed.is_error, "{placed:?}");
    assert!(!std::fs::read_to_string(&path).unwrap().contains("WIDENED"));

    // Edit the library out from under the schematic.
    let lib = symdir
        .path()
        .join("Device.kicad_symdir")
        .join("R.kicad_sym");
    let edited = std::fs::read_to_string(&lib).unwrap().replace(
        "(property \"Value\" \"R\"",
        "(property \"Value\" \"WIDENED\"",
    );
    std::fs::write(&lib, edited).unwrap();

    // A dry run reports the stale copy without touching the file.
    let dry = handle_update_symbols_from_library(
        &json!({ "schematic": path.display().to_string(), "dry_run": true }),
        &ctx,
    )
    .await
    .unwrap();
    let crate::mcp::protocol::ToolContent::Text { text } = &dry.content[0] else {
        panic!("expected text")
    };
    let body: serde_json::Value = serde_json::from_str(text).unwrap();
    assert_eq!(body["updated"], json!(["Device:R"]));
    assert!(
        !std::fs::read_to_string(&path).unwrap().contains("WIDENED"),
        "dry_run must not write"
    );

    let done = handle_update_symbols_from_library(
        &json!({ "schematic": path.display().to_string() }),
        &ctx,
    )
    .await
    .unwrap();
    assert!(!done.is_error, "{done:?}");
    let after = std::fs::read_to_string(&path).unwrap();
    assert!(after.contains("WIDENED"), "{after}");
    assert!(copperpour_sexp::parse_sexp(&after).is_ok(), "{after}");

    // Idempotent: a second run finds nothing to do.
    let again = handle_update_symbols_from_library(
        &json!({ "schematic": path.display().to_string() }),
        &ctx,
    )
    .await
    .unwrap();
    let crate::mcp::protocol::ToolContent::Text { text } = &again.content[0] else {
        panic!("expected text")
    };
    let body: serde_json::Value = serde_json::from_str(text).unwrap();
    assert_eq!(body["updated_count"], json!(0));
    assert_eq!(body["unchanged"], json!(["Device:R"]));
}

#[tokio::test]
async fn update_symbols_from_library_rejects_an_unknown_reference() {
    let (_symdir, _env) = stub_symbol_dir();
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("empty.kicad_sch");
    let ctx = test_ctx();
    handle_create_schematic(&json!({ "path": path.display().to_string() }), &ctx)
        .await
        .unwrap();
    let result = handle_update_symbols_from_library(
        &json!({ "schematic": path.display().to_string(), "references": ["U9"] }),
        &ctx,
    )
    .await
    .unwrap();
    assert!(result.is_error, "{result:?}");
}

/// Wires and labels attach at pin coordinates, so a library edit that
/// moved a pin would silently orphan them. The update is refused and
/// reported instead, unless the caller opts in with allow_pin_moves
/// (grafted from #177 by @JYPochez).
#[tokio::test]
async fn update_symbols_from_library_refuses_a_moved_pin_unless_allowed() {
    // In the stub project dir — see the stale-copy test above.
    let (symdir, _env) = stub_symbol_dir();
    let path = symdir.path().join("guarded.kicad_sch");
    let ctx = test_ctx();
    handle_create_schematic(&json!({ "path": path.display().to_string() }), &ctx)
        .await
        .unwrap();
    let placed = handle_add_schematic_component(
        &json!({ "schematic": path.display().to_string(), "lib_id": "Device:R",
                     "reference": "R1", "x": 100.0, "y": 100.0 }),
        &ctx,
    )
    .await
    .unwrap();
    assert!(!placed.is_error, "{placed:?}");

    // Move pin 2 in the library: (at 0 -3.81 90) → (at 0 -5.08 90).
    let lib = symdir
        .path()
        .join("Device.kicad_symdir")
        .join("R.kicad_sym");
    let edited = std::fs::read_to_string(&lib)
        .unwrap()
        .replace("(at 0 -3.81 90)", "(at 0 -5.08 90)");
    std::fs::write(&lib, edited).unwrap();

    let refused = handle_update_symbols_from_library(
        &json!({ "schematic": path.display().to_string() }),
        &ctx,
    )
    .await
    .unwrap();
    let crate::mcp::protocol::ToolContent::Text { text } = &refused.content[0] else {
        panic!("expected text")
    };
    let body: serde_json::Value = serde_json::from_str(text).unwrap();
    assert_eq!(body["updated_count"], json!(0), "{body}");
    assert_eq!(body["pins_moved"][0]["lib_id"], json!("Device:R"), "{body}");
    let detail = body["pins_moved"][0]["pins"][0].as_str().unwrap();
    assert!(detail.contains("pin 2"), "{detail}");
    assert!(
        detail.contains("-3.81") && detail.contains("-5.08"),
        "{detail}"
    );
    assert!(
        !std::fs::read_to_string(&path).unwrap().contains("-5.08"),
        "a refused update must not touch the schematic"
    );

    // The explicit opt-in updates it.
    let forced = handle_update_symbols_from_library(
        &json!({ "schematic": path.display().to_string(), "allow_pin_moves": true }),
        &ctx,
    )
    .await
    .unwrap();
    let crate::mcp::protocol::ToolContent::Text { text } = &forced.content[0] else {
        panic!("expected text")
    };
    let body: serde_json::Value = serde_json::from_str(text).unwrap();
    assert_eq!(body["updated"], json!(["Device:R"]), "{body}");
    assert_eq!(body["pins_moved"], json!([]), "{body}");
    let after = std::fs::read_to_string(&path).unwrap();
    assert!(after.contains("(at 0 -5.08 90)"), "{after}");
    assert!(copperpour_sexp::parse_sexp(&after).is_ok(), "{after}");
}

/// #203: annotating the same key twice must update the one property in
/// place, not append a sibling — eeschema shows both and edits the wrong
/// one, and a malformed duplicate survives save/reload.
#[tokio::test]
async fn add_component_annotation_updates_an_existing_key_in_place() {
    let (_symdir, _env) = stub_symbol_dir();
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("annot.kicad_sch");
    let ctx = test_ctx();
    handle_create_schematic(&json!({ "path": path.display().to_string() }), &ctx)
        .await
        .unwrap();
    handle_add_schematic_component(
        &json!({ "schematic": path.display().to_string(), "lib_id": "Device:R",
                     "reference": "R1", "x": 100.0, "y": 100.0 }),
        &ctx,
    )
    .await
    .unwrap();

    let first = handle_add_component_annotation(
        &json!({ "schematic": path.display().to_string(), "reference": "R1",
                     "key": "MPN", "value": "RC0402" }),
        &ctx,
    )
    .await
    .unwrap();
    assert!(!first.is_error, "{first:?}");
    let second = handle_add_component_annotation(
        &json!({ "schematic": path.display().to_string(), "reference": "R1",
                     "key": "MPN", "value": "RC0603" }),
        &ctx,
    )
    .await
    .unwrap();
    assert!(!second.is_error, "{second:?}");

    let after = std::fs::read_to_string(&path).unwrap();
    assert_eq!(
        after.matches("(property \"MPN\"").count(),
        1,
        "one MPN property, updated in place:
{after}"
    );
    assert!(after.contains("RC0603"), "{after}");
    assert!(
        !after.contains("RC0402"),
        "old value must be gone:
{after}"
    );
    assert!(copperpour_sexp::parse_sexp(&after).is_ok());
}

/// The old path hardcoded (at 0 0 0) — the annotation rendered at the
/// sheet origin, far from its symbol. The shared property writer anchors
/// each property on its own placed unit.
#[tokio::test]
async fn add_component_annotation_anchors_at_the_symbol_not_the_origin() {
    let (_symdir, _env) = stub_symbol_dir();
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("anchor.kicad_sch");
    let ctx = test_ctx();
    handle_create_schematic(&json!({ "path": path.display().to_string() }), &ctx)
        .await
        .unwrap();
    handle_add_schematic_component(
        &json!({ "schematic": path.display().to_string(), "lib_id": "Device:R",
                     "reference": "R1", "x": 100.0, "y": 100.0 }),
        &ctx,
    )
    .await
    .unwrap();
    handle_add_component_annotation(
        &json!({ "schematic": path.display().to_string(), "reference": "R1",
                     "key": "MPN", "value": "RC0402" }),
        &ctx,
    )
    .await
    .unwrap();

    let after = std::fs::read_to_string(&path).unwrap();
    let prop_at = after.find("(property \"MPN\"").unwrap();
    let prop_block = &after[prop_at..prop_at + 120];
    assert!(
        !prop_block.contains("(at 0 0 0)"),
        "annotation must anchor near its symbol, not the origin:
{prop_block}"
    );
    assert!(prop_block.contains("(at 100"), "{prop_block}");
}

/// Reference/Value/Footprint/Datasheet have dedicated parameters with
/// their own side effects (#157's instances rewrite); annotating them
/// would bypass those.
#[tokio::test]
async fn add_component_annotation_refuses_reserved_keys() {
    let (_symdir, _env) = stub_symbol_dir();
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("reserved.kicad_sch");
    let ctx = test_ctx();
    handle_create_schematic(&json!({ "path": path.display().to_string() }), &ctx)
        .await
        .unwrap();
    handle_add_schematic_component(
        &json!({ "schematic": path.display().to_string(), "lib_id": "Device:R",
                     "reference": "R1", "x": 100.0, "y": 100.0 }),
        &ctx,
    )
    .await
    .unwrap();
    let result = handle_add_component_annotation(
        &json!({ "schematic": path.display().to_string(), "reference": "R1",
                     "key": "Reference", "value": "R9" }),
        &ctx,
    )
    .await
    .unwrap();
    assert!(result.is_error, "{result:?}");
    let crate::mcp::protocol::ToolContent::Text { text } = &result.content[0] else {
        panic!("expected text")
    };
    assert!(text.contains("edit_schematic_component"), "{text}");
}

/// A removed pin is as dangerous as a moved one — whatever attached to it
/// dangles. Same guard, different message.
#[tokio::test]
async fn update_symbols_from_library_refuses_a_removed_pin() {
    // In the stub project dir — see the stale-copy test above.
    let (symdir, _env) = stub_symbol_dir();
    let path = symdir.path().join("shrunk.kicad_sch");
    let ctx = test_ctx();
    handle_create_schematic(&json!({ "path": path.display().to_string() }), &ctx)
        .await
        .unwrap();
    handle_add_schematic_component(
        &json!({ "schematic": path.display().to_string(), "lib_id": "Device:R",
                     "reference": "R1", "x": 100.0, "y": 100.0 }),
        &ctx,
    )
    .await
    .unwrap();

    // Delete pin 2 from the library definition entirely.
    let lib = symdir
        .path()
        .join("Device.kicad_symdir")
        .join("R.kicad_sym");
    let content = std::fs::read_to_string(&lib).unwrap();
    let start = content.find("(pin passive line (at 0 -3.81 90)").unwrap();
    // Cut up to the unit subsymbol's closer, "\n\t\t)" — the pin's own
    // closer is "\n\t\t\t)", which this pattern cannot match early.
    let end = start + content[start..].find("\n\t\t)").unwrap();
    let mut edited = content;
    edited.replace_range(start..end, "");
    std::fs::write(&lib, edited).unwrap();

    let refused = handle_update_symbols_from_library(
        &json!({ "schematic": path.display().to_string() }),
        &ctx,
    )
    .await
    .unwrap();
    let crate::mcp::protocol::ToolContent::Text { text } = &refused.content[0] else {
        panic!("expected text")
    };
    let body: serde_json::Value = serde_json::from_str(text).unwrap();
    assert_eq!(body["updated_count"], json!(0), "{body}");
    let detail = body["pins_moved"][0]["pins"][0].as_str().unwrap();
    assert!(
        detail.contains("pin 2") && detail.contains("removed"),
        "{detail}"
    );
}

fn update_body(result: &CallToolResult) -> serde_json::Value {
    assert!(!result.is_error, "{result:?}");
    let crate::mcp::protocol::ToolContent::Text { text } = &result.content[0] else {
        panic!("expected text")
    };
    serde_json::from_str(text).unwrap()
}

/// Names of the definitions embedded in the sheet's `lib_symbols`.
fn embedded_names(sheet: &str) -> Vec<String> {
    let tree = copperpour_sexp::parse_sexp(sheet).unwrap();
    tree.find("lib_symbols")
        .map(|ls| {
            ls.find_all("symbol")
                .iter()
                .filter_map(|s| s.get(1).and_then(|n| n.as_str()).map(String::from))
                .collect()
        })
        .unwrap_or_default()
}

/// #46: repointing a placed symbol at another library symbol leaves the
/// sheet without a definition for it. The update embeds it, alongside a
/// refresh of a stale copy in the same write.
#[tokio::test]
async fn update_symbols_from_library_embeds_a_symbol_the_sheet_lacks() {
    // In the stub project dir — see the stale-copy test above.
    let (symdir, _env) = stub_symbol_dir();
    let path = symdir.path().join("repointed.kicad_sch");
    let ctx = test_ctx();
    handle_create_schematic(&json!({ "path": path.display().to_string() }), &ctx)
        .await
        .unwrap();
    for (lib_id, reference, x) in [
        ("Device:R", "R1", 100.0),
        ("Device:WITH_DEFAULTS", "U1", 150.0),
    ] {
        let placed = handle_add_schematic_component(
            &json!({ "schematic": path.display().to_string(), "lib_id": lib_id,
                         "reference": reference, "x": x, "y": 100.0 }),
            &ctx,
        )
        .await
        .unwrap();
        assert!(!placed.is_error, "{placed:?}");
    }
    // Repoint U1, as a hand edit would; nothing embeds the new symbol.
    let repoint = |from: &str, to: &str| {
        let sheet = std::fs::read_to_string(&path).unwrap().replace(
            &format!("(lib_id \"{from}\")"),
            &format!("(lib_id \"{to}\")"),
        );
        assert!(!embedded_names(&sheet).contains(&to.to_string()));
        std::fs::write(&path, sheet).unwrap();
    };
    let update = |mut args: serde_json::Value| {
        args["schematic"] = json!(path.display().to_string());
        let ctx = &ctx;
        async move {
            update_body(
                &handle_update_symbols_from_library(&args, ctx)
                    .await
                    .unwrap(),
            )
        }
    };
    repoint("Device:WITH_DEFAULTS", "Device:C_Polarized");

    let before = std::fs::read_to_string(&path).unwrap();
    let dry = update(json!({ "dry_run": true })).await;
    assert_eq!(dry["added"], json!(["Device:C_Polarized"]), "{dry}");
    assert_eq!(dry["errors"], json!([]), "{dry}");
    assert_eq!(
        std::fs::read_to_string(&path).unwrap(),
        before,
        "dry_run must not write"
    );

    // Only an addition, so nothing else is there to cause the write.
    let done = update(json!({})).await;
    assert_eq!(done["added"], json!(["Device:C_Polarized"]), "{done}");
    assert_eq!(done["added_count"], json!(1), "{done}");
    assert_eq!(done["updated"], json!([]), "{done}");
    assert!(done["hint"].as_str().unwrap().contains("added"), "{done}");
    let after = std::fs::read_to_string(&path).unwrap();
    // U1 now resolves, the way KiCad looks it up, to a definition with pins.
    let tree = copperpour_sexp::parse_sexp(&after).unwrap();
    let u1 = extract_symbol_instances(&tree)
        .into_iter()
        .find(|i| i.reference == "U1")
        .unwrap();
    let lib_syms = tree.find("lib_symbols").unwrap().find_all("symbol");
    let embedded = copperpour_sexp::schematic::find_lib_symbol(&lib_syms, &u1)
        .expect("U1's embedded definition");
    assert_eq!(embedded.find_all("symbol")[0].find_all("pin").len(), 2);

    // Idempotent, through the references filter too.
    let again = update(json!({ "references": ["U1"] })).await;
    assert_eq!(again["added"], json!([]), "{again}");
    assert_eq!(again["unchanged"], json!(["Device:C_Polarized"]), "{again}");
    assert_eq!(std::fs::read_to_string(&path).unwrap(), after);

    // A refresh and an addition in one call land in the same write.
    let lib = symdir
        .path()
        .join("Device.kicad_symdir")
        .join("R.kicad_sym");
    let edited = std::fs::read_to_string(&lib).unwrap().replace(
        "(property \"Value\" \"R\"",
        "(property \"Value\" \"WIDENED\"",
    );
    std::fs::write(&lib, edited).unwrap();
    repoint("Device:C_Polarized", "Device:WITH_CUSTOM_FIELDS");
    let both = update(json!({})).await;
    assert_eq!(both["updated"], json!(["Device:R"]), "{both}");
    assert_eq!(
        both["added"],
        json!(["Device:WITH_CUSTOM_FIELDS"]),
        "{both}"
    );
    assert_eq!(both["errors"], json!([]), "{both}");
    let after = std::fs::read_to_string(&path).unwrap();
    let tree = copperpour_sexp::parse_sexp(&after).unwrap();
    let resistor = tree
        .find("lib_symbols")
        .unwrap()
        .find_all("symbol")
        .into_iter()
        .find(|s| s.get(1).and_then(|n| n.as_str()) == Some("Device:R"))
        .unwrap();
    let value = resistor
        .find_all("property")
        .into_iter()
        .find(|p| p.get(1).and_then(|n| n.as_str()) == Some("Value"))
        .and_then(|p| p.get(2).and_then(|n| n.as_str()));
    assert_eq!(value, Some("WIDENED"), "{after}");
    let names = embedded_names(&after);
    assert_eq!(
        names
            .iter()
            .filter(|n| *n == "Device:WITH_CUSTOM_FIELDS")
            .count(),
        1,
        "{names:?}"
    );
}

/// A missing definition that no library resolves stays an error, and the
/// sheet is left as it was.
#[tokio::test]
async fn update_symbols_from_library_reports_an_unresolvable_missing_symbol() {
    let (symdir, _env) = stub_symbol_dir();
    let path = symdir.path().join("dangling.kicad_sch");
    let ctx = test_ctx();
    handle_create_schematic(&json!({ "path": path.display().to_string() }), &ctx)
        .await
        .unwrap();
    let placed = handle_add_schematic_component(
        &json!({ "schematic": path.display().to_string(), "lib_id": "Device:R",
                     "reference": "R1", "x": 100.0, "y": 100.0 }),
        &ctx,
    )
    .await
    .unwrap();
    assert!(!placed.is_error, "{placed:?}");
    let sheet = std::fs::read_to_string(&path)
        .unwrap()
        .replace("(lib_id \"Device:R\")", "(lib_id \"Device:NO_SUCH_PART\")");
    std::fs::write(&path, &sheet).unwrap();

    let body = update_body(
        &handle_update_symbols_from_library(
            &json!({ "schematic": path.display().to_string() }),
            &ctx,
        )
        .await
        .unwrap(),
    );
    assert_eq!(body["added"], json!([]), "{body}");
    let error = body["errors"][0].as_str().unwrap();
    assert!(
        error.contains("Device:NO_SUCH_PART")
            && error.contains("nothing was embedded")
            && error.contains("not found in library"),
        "{error}"
    );
    assert_eq!(std::fs::read_to_string(&path).unwrap(), sheet);
}

/// An instance with `(lib_name …)` draws from that sheet-local copy, not
/// from its lib_id, so it is not missing a definition: embedding its
/// lib_id would add a copy nothing draws.
#[tokio::test]
async fn update_symbols_from_library_leaves_a_lib_name_instance_alone() {
    let (symdir, _env) = stub_symbol_dir();
    let path = symdir.path().join("local_copy.kicad_sch");
    let ctx = test_ctx();
    handle_create_schematic(&json!({ "path": path.display().to_string() }), &ctx)
        .await
        .unwrap();
    let placed = handle_add_schematic_component(
        &json!({ "schematic": path.display().to_string(), "lib_id": "Device:R",
                     "reference": "R1", "x": 100.0, "y": 100.0 }),
        &ctx,
    )
    .await
    .unwrap();
    assert!(!placed.is_error, "{placed:?}");
    // Rename the embedded copy to a sheet-local `R_1` and point the
    // instance at it, the way eeschema stores a locally edited symbol.
    let original = std::fs::read_to_string(&path).unwrap();
    let mut sheet = original.replacen("\"Device:R\"", "\"R_1\"", 1);
    assert!(
        embedded_names(&sheet).contains(&"R_1".to_string()),
        "the first \"Device:R\" is the embedded copy's name:\n{original}"
    );
    sheet = sheet.replace(
        "(lib_id \"Device:R\")",
        "(lib_name \"R_1\")\n\t\t(lib_id \"Device:R\")",
    );
    std::fs::write(&path, &sheet).unwrap();

    let body = update_body(
        &handle_update_symbols_from_library(
            &json!({ "schematic": path.display().to_string() }),
            &ctx,
        )
        .await
        .unwrap(),
    );
    assert_eq!(body["added"], json!([]), "{body}");
    assert_eq!(
        body["errors"],
        json!([
            "'Device:R' is drawn only from sheet-local copies named by lib_name, \
                    which this tool does not refresh"
        ]),
        "{body}"
    );
    assert_eq!(std::fs::read_to_string(&path).unwrap(), sheet);
}

/// Like replace_component, embedding refuses a definition that lacks a
/// unit the sheet places: KiCad cannot draw or netlist that unit. The
/// copy serves unselected instances too, so their units count.
#[tokio::test]
async fn update_symbols_from_library_refuses_a_symbol_missing_a_placed_unit() {
    let (symdir, _env) = stub_symbol_dir();
    let path = symdir.path().join("units.kicad_sch");
    let ctx = test_ctx();
    handle_create_schematic(&json!({ "path": path.display().to_string() }), &ctx)
        .await
        .unwrap();
    for (reference, unit, x) in [("U1", 1, 100.0), ("U2", 3, 150.0)] {
        let placed = handle_add_schematic_component(
            &json!({ "schematic": path.display().to_string(), "lib_id": "Device:OPAMP_DUAL",
                         "reference": reference, "unit": unit, "x": x, "y": 100.0 }),
            &ctx,
        )
        .await
        .unwrap();
        assert!(!placed.is_error, "{placed:?}");
    }
    let sheet = std::fs::read_to_string(&path)
        .unwrap()
        .replace("(lib_id \"Device:OPAMP_DUAL\")", "(lib_id \"Device:R\")");
    std::fs::write(&path, &sheet).unwrap();

    // Only U1, whose unit 1 the one-unit resistor has.
    let body = update_body(
        &handle_update_symbols_from_library(
            &json!({ "schematic": path.display().to_string(), "references": ["U1"] }),
            &ctx,
        )
        .await
        .unwrap(),
    );
    assert_eq!(body["added"], json!([]), "{body}");
    let error = body["errors"][0].as_str().unwrap();
    assert!(
        error.contains("Device:R") && error.contains("placed unit 3"),
        "{error}"
    );
    assert_eq!(std::fs::read_to_string(&path).unwrap(), sheet);
}

/// A sheet without a lib_symbols section gets one where KiCad writes it,
/// right after the header items, and nothing else moves.
#[tokio::test]
async fn update_symbols_from_library_creates_a_missing_lib_symbols_section() {
    let (dir, _env) = stub_symbol_dir();
    let path = dir.path().join("no_section.kicad_sch");
    let sheet = "(kicad_sch\n\t(version 20250114)\n\t(generator \"eeschema\")\n\t(uuid \"11111111-2222-3333-[REDACTED]\")\n\t(paper \"A4\")\n\t(title_block\n\t\t(title \"Bare\")\n\t)\n\t(symbol\n\t\t(lib_id \"Device:R\")\n\t\t(at 100 100 0)\n\t\t(unit 1)\n\t\t(uuid \"aaaaaaaa-bbbb-cccc-dddd-000000000001\")\n\t\t(property \"Reference\" \"R1\" (at 100 100 0))\n\t)\n)\n";
    std::fs::write(&path, sheet).unwrap();

    let body = update_body(
        &handle_update_symbols_from_library(
            &json!({ "schematic": path.display().to_string() }),
            &test_ctx(),
        )
        .await
        .unwrap(),
    );
    assert_eq!(body["added"], json!(["Device:R"]), "{body}");

    let after = std::fs::read_to_string(&path).unwrap();
    let items: Vec<String> = copperpour_sexp::writer::find_direct_child_blocks(&after, "kicad_sch")
        .into_iter()
        .map(|(start, end)| after[start..end].to_string())
        .collect();
    let heads: Vec<&str> = items
        .iter()
        .map(|item| item[1..].split([' ', '\n', '\t']).next().unwrap())
        .collect();
    assert_eq!(
        heads,
        [
            "version",
            "generator",
            "uuid",
            "paper",
            "title_block",
            "lib_symbols",
            "symbol"
        ],
        "{after}"
    );
    assert_eq!(embedded_names(&after), ["Device:R"]);
    let before: Vec<String> = copperpour_sexp::writer::find_direct_child_blocks(sheet, "kicad_sch")
        .into_iter()
        .map(|(start, end)| sheet[start..end].to_string())
        .collect();
    let without_section: Vec<String> = items
        .into_iter()
        .filter(|item| !item.starts_with("(lib_symbols"))
        .collect();
    assert_eq!(without_section, before);
}

/// A sheet with no lib_symbols section and no header items to put one
/// after: the symbol resolves but cannot be placed, which is reported as
/// that, not as a lookup failure, and nothing is written.
#[tokio::test]
async fn update_symbols_from_library_reports_a_definition_it_cannot_place() {
    let (dir, _env) = stub_symbol_dir();
    let path = dir.path().join("headerless.kicad_sch");
    let sheet = "(kicad_sch\n\t(symbol\n\t\t(lib_id \"Device:R\")\n\t\t(at 100 100 0)\n\t\t(unit 1)\n\t\t(property \"Reference\" \"R1\" (at 100 100 0))\n\t)\n)\n";
    std::fs::write(&path, sheet).unwrap();

    let body = update_body(
        &handle_update_symbols_from_library(
            &json!({ "schematic": path.display().to_string() }),
            &test_ctx(),
        )
        .await
        .unwrap(),
    );
    assert_eq!(body["added"], json!([]), "{body}");
    let error = body["errors"][0].as_str().unwrap();
    assert!(error.contains("could not be placed"), "{error}");
    assert_eq!(std::fs::read_to_string(&path).unwrap(), sheet);
}

/// #46's workflow on a KiCad-saved sheet, through the served tool: two
/// parts repointed at another registered library, one of them selected.
/// Only the selected one's definition is embedded, and nothing else on
/// the sheet moves — not even a title that quotes "(lib_symbols".
#[tokio::test]
async fn served_update_symbols_from_library_embeds_a_repointed_symbol_on_a_kicad_sheet() {
    let (dir, _env) = stub_symbol_dir();
    // Outside KICAD10_SYMBOL_DIR's own listing, so only the project's
    // table entry can resolve it.
    let passives = dir.path().join("libs").join("Passives.kicad_symdir");
    std::fs::create_dir_all(&passives).unwrap();
    for name in ["CAP_ALT", "RES_ALT"] {
        std::fs::write(
                passives.join(format!("{name}.kicad_sym")),
                format!(
                    "(kicad_symbol_lib\n\t(version 20241209)\n\t(generator \"test\")\n\t(symbol \"{name}\"\n\t\t(property \"Reference\" \"C\" (at 0 0 0))\n\t\t(property \"Value\" \"{name}\" (at 0 0 0))\n\t\t(symbol \"{name}_0_1\"\n\t\t\t(pin passive line (at 0 6.35 270) (length 2.54)\n\t\t\t\t(name \"~\" (effects (font (size 1.27 1.27))))\n\t\t\t\t(number \"1\" (effects (font (size 1.27 1.27))))\n\t\t\t)\n\t\t\t(pin passive line (at 0 -6.35 90) (length 2.54)\n\t\t\t\t(name \"~\" (effects (font (size 1.27 1.27))))\n\t\t\t\t(number \"2\" (effects (font (size 1.27 1.27))))\n\t\t\t)\n\t\t)\n\t)\n)\n"
                ),
            )
            .unwrap();
    }
    let table = dir.path().join("sym-lib-table");
    let registered = std::fs::read_to_string(&table).unwrap().replace(
            "\n)\n",
            "\n  (lib (name \"Passives\") (type \"KiCad\") (uri \"${KIPRJMOD}/libs/Passives.kicad_symdir\") (options \"\") (descr \"\"))\n)\n",
        );
    std::fs::write(&table, registered).unwrap();

    let sheet = include_str!("../../../tests/fixtures/dnp_kicad10.kicad_sch")
        .replacen(
            "\t(paper \"A4\")\n",
            "\t(paper \"A4\")\n\t(title_block\n\t\t(title \"Notes (lib_symbols)\")\n\t)\n",
            1,
        )
        .replace("(lib_id \"Device:C\")", "(lib_id \"Passives:CAP_ALT\")")
        .replace("(lib_id \"Device:R\")", "(lib_id \"Passives:RES_ALT\")");
    assert!(sheet.contains("Notes (lib_symbols)") && sheet.contains("Passives:RES_ALT"));
    let path = dir.path().join("repointed.kicad_sch");
    std::fs::write(&path, &sheet).unwrap();

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
    let message = json!({"jsonrpc": "2.0", "id": 46, "method": "tools/call",
            "params": {"name": "update_symbols_from_library", "arguments": {
                "schematic": path.display().to_string(), "references": ["C1"] }}});
    let result = handler
        .handle_message(message)
        .await
        .unwrap()
        .result
        .unwrap();
    assert_ne!(result["isError"], true, "{result}");
    let body: serde_json::Value =
        serde_json::from_str(result["content"][0]["text"].as_str().unwrap()).unwrap();
    assert_eq!(body["added"], json!(["Passives:CAP_ALT"]), "{body}");
    assert_eq!(body["errors"], json!([]), "{body}");

    let after = std::fs::read_to_string(&path).unwrap();
    let tree = copperpour_sexp::parse_sexp(&after).unwrap();
    let c1 = extract_symbol_instances(&tree)
        .into_iter()
        .find(|i| i.reference == "C1")
        .unwrap();
    let lib_syms = tree.find("lib_symbols").unwrap().find_all("symbol");
    let embedded = copperpour_sexp::schematic::find_lib_symbol(&lib_syms, &c1)
        .expect("C1's embedded definition");
    let pins = embedded.find_all("symbol")[0].find_all("pin");
    let ys: Vec<f64> = pins
        .iter()
        .map(|pin| pin.find("at").unwrap().get_f64(2).unwrap())
        .collect();
    assert_eq!(ys, vec![6.35, -6.35]);
    assert!(!embedded_names(&after).contains(&"Passives:RES_ALT".to_string()));

    // Every other item on the sheet, and every copy already embedded, is
    // byte for byte what it was.
    let items = |text: &str, parent: &str| -> Vec<String> {
        copperpour_sexp::writer::find_direct_child_blocks(text, parent)
            .into_iter()
            .map(|(start, end)| text[start..end].to_string())
            .collect()
    };
    let outside = |text: &str| -> Vec<String> {
        items(text, "kicad_sch")
            .into_iter()
            .filter(|item| !item.starts_with("(lib_symbols"))
            .collect()
    };
    assert_eq!(outside(&after), outside(&sheet));
    let kept = items(&sheet, "lib_symbols");
    let now = items(&after, "lib_symbols");
    assert_eq!(now.len(), kept.len() + 1);
    assert_eq!(now[..kept.len()], kept[..]);
}
