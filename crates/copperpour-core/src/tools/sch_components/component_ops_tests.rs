/// `edit_schematic_component` had two independent defects, both of which
/// reported success: `fields` was declared in the schema and never read
/// (#158), and `new_reference` rewrote only the rendered property, leaving the
/// instances path — which is where KiCad reads the designator for the netlist
/// — on the old value (#157).
mod edit_component_tests {
    use super::super::*;
    use crate::router::ToolRouter;
    use crate::tools::ServerConfig;
    use serde_json::json;
    use std::io::Write;
    use std::sync::Arc;

    /// One R1, with an instances path, as eeschema writes it.
    const SCH: &str = "(kicad_sch\n\t(version 20250610)\n\t(generator \"eeschema\")\n\t(uuid \"root\")\n\t(lib_symbols\n\t\t(symbol \"Device:R\"\n\t\t\t(property \"Reference\" \"R\" (at 0 0 0))\n\t\t)\n\t)\n\t(symbol\n\t\t(lib_id \"Device:R\")\n\t\t(at 50 60 0)\n\t\t(unit 1)\n\t\t(uuid \"sym-1\")\n\t\t(property \"Reference\" \"R1\"\n\t\t\t(at 52 58 0)\n\t\t)\n\t\t(property \"Value\" \"10k\"\n\t\t\t(at 52 62 0)\n\t\t)\n\t\t(instances\n\t\t\t(project \"proj\"\n\t\t\t\t(path \"/root\"\n\t\t\t\t\t(reference \"R1\") (unit 1)\n\t\t\t\t)\n\t\t\t)\n\t\t)\n\t)\n\t(sheet_instances\n\t\t(path \"/\" (page \"1\"))\n\t)\n)\n";

    async fn edit(args: serde_json::Value) -> (String, String) {
        let mut f = tempfile::NamedTempFile::with_suffix(".kicad_sch").unwrap();
        f.write_all(SCH.as_bytes()).unwrap();
        f.flush().unwrap();

        let mut args = args;
        args["schematic"] = json!(f.path().to_str().unwrap());

        let def = tools()
            .into_iter()
            .find(|t| t.name == "edit_schematic_component")
            .unwrap();
        let ctx = Arc::new(ToolContext::new(
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
        ));
        let res = (def.handler)(&args, ctx).await.unwrap();
        let reply = match res.content.first() {
            Some(crate::mcp::protocol::ToolContent::Text { text }) => text.clone(),
            other => panic!("expected text, got {other:?}"),
        };
        (std::fs::read_to_string(f.path()).unwrap(), reply)
    }

    /// #157: the rename must reach the instances path, not just the property.
    #[tokio::test]
    async fn renaming_a_reference_rewrites_the_instances_path() {
        let (out, _) = edit(json!({ "reference": "R1", "new_reference": "R7" })).await;
        assert!(
            out.contains("(property \"Reference\" \"R7\""),
            "property renamed:\n{out}"
        );
        assert!(
            out.contains("(reference \"R7\")"),
            "instances path must carry the new designator, or the netlist \
             ignores the rename:\n{out}"
        );
        assert!(
            !out.contains("(reference \"R1\")"),
            "no instances entry may keep the old designator:\n{out}"
        );
    }

    /// #158: a custom field that does not exist yet must be created.
    #[tokio::test]
    async fn a_new_custom_field_is_written_into_the_symbol() {
        let (out, reply) = edit(json!({
            "reference": "R1",
            "fields": { "MPN": "RC0402FR-0710KL" }
        }))
        .await;
        assert!(
            out.contains("(property \"MPN\" \"RC0402FR-0710KL\""),
            "custom field must land in the file:\n{out}"
        );
        assert!(
            out.contains("(hide yes)"),
            "a custom field is data, not sheet artwork:\n{out}"
        );
        assert!(reply.contains("MPN"), "the reply must report it: {reply}");
        // Anchored on the symbol, not defaulted to the sheet origin (#95).
        assert!(
            !out.contains("(property \"MPN\" \"RC0402FR-0710KL\"\n\t\t\t(at 0 0 0)"),
            "must not land at the sheet origin:\n{out}"
        );
    }

    /// #158: an existing custom field is updated rather than duplicated.
    #[tokio::test]
    async fn an_existing_custom_field_is_updated_not_duplicated() {
        let (out, _) = edit(json!({ "reference": "R1", "fields": { "MPN": "first" } })).await;
        assert_eq!(out.matches("(property \"MPN\"").count(), 1);

        // Value is a first-class parameter, so it must be updated in place.
        let (out2, _) = edit(json!({ "reference": "R1", "value": "22k" })).await;
        assert_eq!(out2.matches("(property \"Value\"").count(), 1, "{out2}");
        assert!(out2.contains("(property \"Value\" \"22k\""), "{out2}");
    }

    /// The defect that made #158 invisible: with `fields` unread, both
    /// `changed` and `errors` came back empty, so the no-op guard never fired
    /// and the call reported success having done nothing.
    #[tokio::test]
    async fn a_fields_only_call_no_longer_reports_an_empty_success() {
        let (_, reply) = edit(json!({
            "reference": "R1",
            "fields": { "MPN": "RC0402FR-0710KL" }
        }))
        .await;
        assert!(
            !reply.contains("\"changes\":[]"),
            "a fields-only call must not report an empty change set: {reply}"
        );
    }

    /// Reserved names belong to their own parameters — routing Reference
    /// through `fields` would skip the instances rewrite and silently
    /// reintroduce #157.
    #[tokio::test]
    async fn reserved_names_are_refused_inside_fields() {
        let (out, reply) = edit(json!({
            "reference": "R1",
            "fields": { "Reference": "R9" }
        }))
        .await;
        assert!(
            out.contains("(property \"Reference\" \"R1\""),
            "the designator must be untouched:\n{out}"
        );
        assert!(
            reply.contains("Reference"),
            "the refusal is reported: {reply}"
        );
    }
}

mod schematic_view_tests {
    use super::super::*;
    use std::path::{Path, PathBuf};

    #[test]
    fn the_view_slot_is_stable_for_one_schematic() {
        let sheet = Path::new("/projects/alpha/power.kicad_sch");
        assert_eq!(schematic_view_dir(sheet), schematic_view_dir(sheet));
    }

    /// The old handler used a fresh uuid per call, which is why nothing could
    /// be returned without leaking a directory per call. Deriving the slot from
    /// the path bounds it to one per schematic — but it must be the whole path,
    /// or two projects with a `power.kicad_sch` would overwrite each other.
    #[test]
    fn two_sheets_sharing_a_stem_get_different_slots() {
        let alpha = schematic_view_dir(Path::new("/projects/alpha/power.kicad_sch"));
        let beta = schematic_view_dir(Path::new("/projects/beta/power.kicad_sch"));
        assert_ne!(alpha, beta);
    }

    #[test]
    fn the_view_slot_lives_under_the_system_temp_dir() {
        let dir = schematic_view_dir(Path::new("/projects/alpha/power.kicad_sch"));
        assert!(
            dir.starts_with(std::env::temp_dir().join("copperpour-schematic-views")),
            "views must not be written next to the caller's project: {}",
            dir.display()
        );
    }

    /// The reported defect, end to end: the tool used to render the SVG, read
    /// its length, delete it, and report "The SVG file has been generated".
    /// Needs a real kicad-cli, so it is ignored like the other live tests.
    #[tokio::test]
    #[ignore = "needs a real kicad-cli on PATH"]
    async fn the_rendered_svg_survives_the_call() {
        let tmp = tempfile::tempdir().unwrap();
        let sheet = tmp.path().join("view.kicad_sch");
        std::fs::write(
            &sheet,
            "(kicad_sch\n\t(version 20260101)\n\t(generator \"eeschema\")\n\t(uuid \"view-0001\")\n\t(paper \"A4\")\n\t(lib_symbols)\n\t(sheet_instances\n\t\t(path \"/\" (page \"1\"))\n\t)\n)\n",
        )
        .unwrap();

        let cfg = crate::tools::ServerConfig {
            kicad_cli: std::env::var("KICAD_CLI").unwrap_or_else(|_| "kicad-cli".to_string()),
            kicad_binary: String::new(),
            ipc_address: String::new(),
            project_dir: None,
            jlcpcb_db_path: None,
            auto_load_toolsets: false,
            eager_toolsets: false,
        };
        let ctx = ToolContext::new(cfg, std::sync::Arc::new(crate::router::ToolRouter::new()));
        let args = json!({ "schematic": sheet.display().to_string() });

        let first = handle_get_schematic_view(&args, &ctx).await.unwrap();
        assert!(!first.is_error, "{:?}", first.content);
        let body: serde_json::Value = match &first.content[0] {
            crate::mcp::protocol::ToolContent::Text { text } => {
                serde_json::from_str(text).expect("the result is JSON, not prose")
            }
            _ => panic!("expected text content"),
        };

        let svg = PathBuf::from(body["svg"].as_str().expect("a path to the SVG"));
        assert!(
            svg.exists(),
            "the file the tool names must still be there: {}",
            svg.display()
        );
        assert_eq!(
            std::fs::metadata(&svg).unwrap().len(),
            body["bytes"].as_u64().unwrap(),
            "the reported size is the file's size"
        );
        assert_eq!(body["format"], "svg");

        // The invisible text layer this SVG is also useful for.
        let content = std::fs::read_to_string(&svg).unwrap();
        assert!(
            content.contains("opacity=\"0\""),
            "kicad-cli writes a machine-readable text layer"
        );

        // A second view reuses the slot instead of leaving a directory behind.
        let second = handle_get_schematic_view(&args, &ctx).await.unwrap();
        let body2: serde_json::Value = match &second.content[0] {
            crate::mcp::protocol::ToolContent::Text { text } => serde_json::from_str(text).unwrap(),
            _ => panic!("expected text content"),
        };
        assert_eq!(body2["svg"], body["svg"]);
    }

    /// `render_schematic_svg` used to take `output.parent()` on the slot
    /// directory `handle_get_schematic_view` passed in, stripping the hash
    /// component and writing every sheet into the shared parent
    /// `copperpour-schematic-views/<stem>.svg` (#25). Two sheets sharing a
    /// stem must land inside their own slots and both survive.
    #[tokio::test]
    async fn two_same_stem_sheets_render_into_distinct_view_slots() {
        let dir = tempfile::tempdir().unwrap();
        let cli = crate::tools::cli::test_support::write_script(
            dir.path(),
            "fake-kicad-cli",
            "#!/bin/sh\nout=\"\"\nprev=\"\"\nfor a in \"$@\"; do\n  if [ \"$prev\" = \"--output\" ]; then out=\"$a\"; fi\n  prev=\"$a\"\ndone\nfor last; do :; done\nstem=$(basename \"$last\" .kicad_sch)\nprintf 'VIEW %s' \"$last\" > \"$out/$stem.svg\"\nexit 0\n",
            "@echo off\r\nsetlocal enabledelayedexpansion\r\nset prev=\r\nset last=\r\n:loop\r\nif \"%~1\"==\"\" goto done\r\nif \"!prev!\"==\"--output\" set out=%~1\r\nset prev=%~1\r\nset last=%~1\r\nshift\r\ngoto loop\r\n:done\r\nfor %%F in (\"!last!\") do set stem=%%~nF\r\necho VIEW !last!> \"!out!\\!stem!.svg\"\r\nexit /b 0\r\n",
        );
        let alpha_dir = dir.path().join("alpha");
        let beta_dir = dir.path().join("beta");
        std::fs::create_dir_all(&alpha_dir).unwrap();
        std::fs::create_dir_all(&beta_dir).unwrap();
        let alpha = alpha_dir.join("power.kicad_sch");
        let beta = beta_dir.join("power.kicad_sch");
        let fixture = include_str!("../../../tests/fixtures/single_pin_nets.kicad_sch");
        std::fs::write(&alpha, fixture).unwrap();
        std::fs::write(&beta, fixture).unwrap();

        let handler = crate::mcp::handler::McpHandler::new(crate::tools::ServerConfig {
            kicad_cli: cli.display().to_string(),
            auto_load_toolsets: true,
            eager_toolsets: true,
            ..Default::default()
        })
        .await
        .expect("handler builds");

        let mut bodies = Vec::new();
        for sheet in [&alpha, &beta] {
            let response = handler
                .handle_message(json!({
                    "jsonrpc": "2.0",
                    "id": 1,
                    "method": "tools/call",
                    "params": {
                        "name": "get_schematic_view",
                        "arguments": { "schematic": sheet.display().to_string() }
                    }
                }))
                .await
                .expect("request returns a response");
            let result = response.result.expect("successful JSON-RPC response");
            let text = result["content"][0]["text"]
                .as_str()
                .expect("tool returns JSON text");
            bodies.push(serde_json::from_str::<serde_json::Value>(text).unwrap());
        }

        let alpha_svg = schematic_view_dir(&alpha).join("power.svg");
        let beta_svg = schematic_view_dir(&beta).join("power.svg");
        assert_eq!(bodies[0]["svg"], alpha_svg.display().to_string());
        assert_eq!(bodies[1]["svg"], beta_svg.display().to_string());
        assert!(
            std::fs::read_to_string(&alpha_svg)
                .unwrap()
                .contains(&alpha.display().to_string()),
            "alpha slot must keep alpha's render"
        );
        assert!(
            std::fs::read_to_string(&beta_svg)
                .unwrap()
                .contains(&beta.display().to_string()),
            "beta slot must keep beta's render"
        );
        let _ = std::fs::remove_dir_all(schematic_view_dir(&alpha));
        let _ = std::fs::remove_dir_all(schematic_view_dir(&beta));
    }
}

pub(super) mod component_delete_connectivity_tests {
    use super::super::*;
    use crate::mcp::{error::extract_error_kind, protocol::ToolContent};
    use crate::tools::ServerConfig;
    use std::sync::Arc;

    const CONNECTIVITY: &str = include_str!("../../../tests/fixtures/junction_reconcile.kicad_sch");

    fn context() -> ToolContext {
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
            Arc::new(crate::router::ToolRouter::new()),
        )
    }

    fn fixture(content: &str) -> (tempfile::TempDir, std::path::PathBuf) {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("delete.kicad_sch");
        std::fs::write(&path, content).unwrap();
        (directory, path)
    }

    fn body(result: &CallToolResult) -> serde_json::Value {
        assert!(!result.is_error, "{result:?}");
        let ToolContent::Text { text } = &result.content[0] else {
            panic!("expected text result");
        };
        serde_json::from_str(text).unwrap()
    }

    pub(in crate::tools::sch_components) fn has_junction(content: &str, x: f64, y: f64) -> bool {
        let tree = parse_sexp(content).unwrap();
        copperpour_sexp::schematic::extract_junctions(&tree)
            .iter()
            .any(|&(jx, jy)| copperpour_sexp::geometry::points_coincident(x, y, jx, jy, 0.01))
    }

    #[tokio::test]
    async fn deleting_a_pin_only_dot_prunes_it_but_preserves_an_unrelated_wire_t() {
        let (_directory, path) = fixture(CONNECTIVITY);
        let result = handle_delete_schematic_component(
            &json!({ "schematic": path, "reference": "R1" }),
            &context(),
        )
        .await
        .unwrap();
        let response = body(&result);

        assert_eq!(response["deleted_units"], 1);
        assert_eq!(response["junctions_pruned_count"], 1);
        let committed = std::fs::read_to_string(&path).unwrap();
        assert!(!has_junction(&committed, 120.65, 139.7));
        assert!(
            has_junction(&committed, 120.65, 170.18),
            "the two-wire T remains justified independently of R1"
        );
    }

    #[tokio::test]
    async fn attached_no_connect_is_removed_and_unrelated_marker_survives() {
        let unrelated = "\t(no_connect\n\t\t(at 250 250)\n\t\t(uuid \"unrelated-marker\")\n\t)\n";
        let closing = CONNECTIVITY.rfind("\n)").unwrap();
        let original = format!(
            "{}{unrelated}{}",
            &CONNECTIVITY[..closing + 1],
            &CONNECTIVITY[closing + 1..]
        );
        assert!(original.contains("unrelated-marker"));
        let (_directory, path) = fixture(&original);
        let result = handle_delete_schematic_component(
            &json!({ "schematic": path, "reference": "R3" }),
            &context(),
        )
        .await
        .unwrap();
        let response = body(&result);

        assert_eq!(response["removed_no_connects_count"], 1);
        assert_eq!(
            response["removed_no_connect_uuids"][0],
            "3f9dbc19-858e-4bf8-b937-b169159de4c8"
        );
        let committed = std::fs::read_to_string(&path).unwrap();
        assert!(!committed.contains("3f9dbc19-858e-4bf8-b937-b169159de4c8"));
        assert!(committed.contains("unrelated-marker"));
    }

    #[tokio::test]
    async fn attached_no_connect_survives_when_a_remaining_pin_shares_the_point() {
        let original =
            CONNECTIVITY.replace("\t\t(at 120.65 135.89 0)\n", "\t\t(at 190.5 196.85 0)\n");
        assert_ne!(original, CONNECTIVITY);
        let (_directory, path) = fixture(&original);
        let result = handle_delete_schematic_component(
            &json!({ "schematic": path, "reference": "R3" }),
            &context(),
        )
        .await
        .unwrap();
        let response = body(&result);

        assert_eq!(response["removed_no_connects_count"], 0);
        let committed = std::fs::read_to_string(&path).unwrap();
        assert!(committed.contains("3f9dbc19-858e-4bf8-b937-b169159de4c8"));
    }

    #[tokio::test]
    async fn missing_reference_is_structured_stale_and_does_not_write() {
        let (_directory, path) = fixture(CONNECTIVITY);
        let result = handle_delete_schematic_component(
            &json!({ "schematic": path, "reference": "R404" }),
            &context(),
        )
        .await
        .unwrap();

        assert_eq!(extract_error_kind(&result).as_deref(), Some("stale_target"));
        assert_eq!(std::fs::read_to_string(path).unwrap(), CONNECTIVITY);
    }

    #[tokio::test]
    async fn unresolved_pin_geometry_is_stale_and_does_not_write() {
        let original = "(kicad_sch\n  (version 20260306)\n  (uuid \"root\")\n  (lib_symbols)\n  (symbol\n    (lib_id \"Missing:Part\")\n    (at 10 10 0)\n    (unit 1)\n    (uuid \"missing-lib\")\n    (property \"Reference\" \"U1\")\n  )\n)\n";
        let (_directory, path) = fixture(original);
        let result = handle_delete_schematic_component(
            &json!({ "schematic": path, "reference": "U1" }),
            &context(),
        )
        .await
        .unwrap();

        assert_eq!(extract_error_kind(&result).as_deref(), Some("stale_target"));
        assert_eq!(std::fs::read_to_string(path).unwrap(), original);
    }

    #[tokio::test]
    async fn attached_marker_without_uuid_is_stale_and_does_not_write() {
        let original =
            CONNECTIVITY.replace("\t\t(uuid \"3f9dbc19-858e-4bf8-b937-b169159de4c8\")\n", "");
        assert_ne!(original, CONNECTIVITY);
        let (_directory, path) = fixture(&original);
        let result = handle_delete_schematic_component(
            &json!({ "schematic": path, "reference": "R3" }),
            &context(),
        )
        .await
        .unwrap();

        assert_eq!(extract_error_kind(&result).as_deref(), Some("stale_target"));
        assert_eq!(std::fs::read_to_string(path).unwrap(), original);
    }

    #[tokio::test]
    async fn duplicate_reference_and_unit_is_ambiguous_and_does_not_write() {
        let original = CONNECTIVITY.replacen(
            "(property \"Reference\" \"R3\"",
            "(property \"Reference\" \"R1\"",
            1,
        );
        let (_directory, path) = fixture(&original);
        let result = handle_delete_schematic_component(
            &json!({ "schematic": path, "reference": "R1" }),
            &context(),
        )
        .await
        .unwrap();

        assert_eq!(
            extract_error_kind(&result).as_deref(),
            Some("ambiguous_target")
        );
        assert_eq!(std::fs::read_to_string(path).unwrap(), original);
    }

    #[test]
    fn stale_revision_refuses_the_prepared_delete_without_overwriting() {
        let (_directory, path) = fixture(CONNECTIVITY);
        let plan = plan_component_deletion(&path, CONNECTIVITY, "R1").unwrap();
        let newer = CONNECTIVITY.replace("(paper \"A4\")", "(paper \"A3\")");
        assert_ne!(newer, CONNECTIVITY);
        std::fs::write(&path, &newer).unwrap();

        let error = commit_command(&path, &plan.command).unwrap_err();
        let refusal = component_delete_commit_refusal(&path, &error).unwrap();
        assert_eq!(
            extract_error_kind(&refusal).as_deref(),
            Some("stale_target")
        );
        assert_eq!(std::fs::read_to_string(path).unwrap(), newer);
    }

    #[tokio::test]
    async fn kicad_lock_is_structured_stale_and_preserves_the_file() {
        let (_directory, path) = fixture(CONNECTIVITY);
        let lock = path.with_file_name(format!(
            "~{}.lck",
            path.file_name().unwrap().to_string_lossy()
        ));
        std::fs::write(&lock, "locked").unwrap();
        let result = handle_delete_schematic_component(
            &json!({ "schematic": path, "reference": "R1" }),
            &context(),
        )
        .await
        .unwrap();

        assert_eq!(extract_error_kind(&result).as_deref(), Some("stale_target"));
        assert_eq!(std::fs::read_to_string(path).unwrap(), CONNECTIVITY);
    }
}

/// A placement under a numbered designator that another placement of the
/// project holds is refused before anything is written, unless it is another
/// unit of that multi-unit part, as eeschema's annotation check reports it
/// (`eeschema/sch_reference_list.cpp` 681-740 at 10.0.6). Every call is served
/// through `tools/call` on KiCad-saved fixtures: `ecc83_multiunit.kicad_sch`,
/// whose `R1` is a 1.5K, and the `annotate_nested_hierarchy` project (see its
/// README), whose `bank.kicad_sch` holds `R10` and whose `cell.kicad_sch` holds
/// `R13`. The units of a multi-unit part are in
/// `only_another_unit_of_the_part_with_its_value_joins_a_designator`, which
/// needs a library that resolves their count.
mod designator_in_use_tests {
    use super::super::*;
    use crate::tools::ServerConfig;
    use std::path::Path;

    const ECC83: &str = include_str!("../../../tests/fixtures/ecc83_multiunit.kicad_sch");
    const NESTED_PRO: &str =
        include_str!("../../../tests/fixtures/annotate_nested_hierarchy/nested_pages.kicad_pro");
    const NESTED_ROOT: &str =
        include_str!("../../../tests/fixtures/annotate_nested_hierarchy/nested_pages.kicad_sch");
    const NESTED_BANK: &str =
        include_str!("../../../tests/fixtures/annotate_nested_hierarchy/bank.kicad_sch");
    const NESTED_CELL: &str =
        include_str!("../../../tests/fixtures/annotate_nested_hierarchy/cell.kicad_sch");

    /// Lay fixture files out in a fresh directory, as KiCad saved them.
    fn lay_out(files: &[(&str, &str)]) -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        for (name, content) in files {
            std::fs::write(dir.path().join(name), content).unwrap();
        }
        dir
    }

    fn ecc83() -> (tempfile::TempDir, std::path::PathBuf) {
        let dir = lay_out(&[("ecc83-pp.kicad_sch", ECC83)]);
        let path = dir.path().join("ecc83-pp.kicad_sch");
        (dir, path)
    }

    fn nested() -> tempfile::TempDir {
        lay_out(&[
            ("nested_pages.kicad_pro", NESTED_PRO),
            ("nested_pages.kicad_sch", NESTED_ROOT),
            ("bank.kicad_sch", NESTED_BANK),
            ("cell.kicad_sch", NESTED_CELL),
        ])
    }

    /// One `tools/call` through the served dispatch: the JSON body, with
    /// `is_error` from the result.
    async fn serve(tool: &str, path: &Path, mut arguments: serde_json::Value) -> serde_json::Value {
        let handler = crate::mcp::handler::McpHandler::new(ServerConfig {
            eager_toolsets: true,
            ..Default::default()
        })
        .await
        .expect("handler builds");
        arguments["schematic"] = json!(path.display().to_string());
        let response = handler
            .handle_message(json!({
                "jsonrpc": "2.0",
                "id": 1,
                "method": "tools/call",
                "params": { "name": tool, "arguments": arguments }
            }))
            .await
            .expect("tools/call receives a response");
        let result = response.result.expect("successful JSON-RPC response");
        let mut body: serde_json::Value =
            serde_json::from_str(result["content"][0]["text"].as_str().unwrap()).unwrap();
        body["is_error"] = json!(result["isError"] == json!(true));
        body
    }

    async fn place(
        path: &Path,
        lib_id: &str,
        reference: &str,
        unit: u32,
        x: f64,
    ) -> serde_json::Value {
        serve(
            "add_schematic_component",
            path,
            json!({"lib_id": lib_id, "reference": reference, "unit": unit, "x": x, "y": 25.4}),
        )
        .await
    }

    fn refused_on(body: &serde_json::Value, field: &str) -> String {
        assert_eq!(body["is_error"], json!(true), "{body}");
        assert_eq!(body["error"]["kind"], "invalid_argument", "{body}");
        assert_eq!(body["error"]["field"], field, "{body}");
        body["error"]["reason"].as_str().unwrap().to_string()
    }

    #[tokio::test]
    async fn a_designator_on_the_sheet_is_refused_and_nothing_is_written() {
        let (_dir, path) = ecc83();
        let before = std::fs::read(&path).unwrap();
        let reason = refused_on(
            &place(&path, "ecc83-pp:R", "R1", 1, 101.6).await,
            "reference",
        );
        assert!(
            reason.contains("'R1' is already placed: ecc83-pp:R unit 1 value '1.5K'"),
            "{reason}"
        );
        assert!(
            reason.contains("Omit reference to place it as 'R?'"),
            "{reason}"
        );
        assert_eq!(std::fs::read(&path).unwrap(), before);
    }

    /// `R?` is what `annotate_schematic` numbers; it never meets the check.
    #[tokio::test]
    async fn an_unannotated_designator_is_never_refused() {
        let (_dir, path) = ecc83();
        for x in [101.6, 127.0] {
            let placed = serve(
                "add_schematic_component",
                &path,
                json!({"lib_id": "ecc83-pp:R", "x": x, "y": 25.4}),
            )
            .await;
            assert_eq!(placed["is_error"], json!(false), "{placed}");
        }
        let sch = cse::Schematic::load(&path).unwrap();
        let unannotated = sch.symbols.iter().filter(|s| s.reference() == Some("R?"));
        assert_eq!(unannotated.count(), 2);
    }

    #[tokio::test]
    async fn a_designator_held_on_another_sheet_is_refused_and_names_it() {
        let dir = nested();
        let root = dir.path().join("nested_pages.kicad_sch");
        let before = std::fs::read(&root).unwrap();
        for (reference, holder) in [("R10", "bank.kicad_sch"), ("R13", "cell.kicad_sch")] {
            let reason = refused_on(
                &place(&root, "Device:R", reference, 1, 25.4).await,
                "reference",
            );
            assert!(reason.contains(holder), "{reason}");
        }
        assert_eq!(std::fs::read(&root).unwrap(), before);
        let placed = place(&root, "Device:R", "R17", 1, 25.4).await;
        assert_eq!(placed["is_error"], json!(false), "{placed}");
        assert!(placed.get("other_sheets_unreadable").is_none(), "{placed}");
    }

    /// `bank.kicad_sch` is placed twice, so a placement on it carries its
    /// designator in both instance records; it holds the designator once.
    #[tokio::test]
    async fn a_placement_on_a_sheet_placed_twice_holds_its_designator_once() {
        let dir = nested();
        let bank = dir.path().join("bank.kicad_sch");
        let placed = place(&bank, "Device:R", "R17", 1, 25.4).await;
        assert_eq!(placed["is_error"], json!(false), "{placed}");
        let reason = refused_on(&place(&bank, "Device:R", "R17", 1, 50.8).await, "reference");
        assert_eq!(reason.matches("bank.kicad_sch").count(), 1, "{reason}");
    }

    /// A sheet file that cannot be read is named, its designators unseen; an
    /// unannotated placement reads no other sheet.
    #[tokio::test]
    async fn an_unreadable_sheet_is_named_in_the_reply() {
        let dir = nested();
        let root = dir.path().join("nested_pages.kicad_sch");
        std::fs::remove_file(dir.path().join("cell.kicad_sch")).unwrap();
        let placed = place(&root, "Device:R", "R13", 1, 25.4).await;
        assert_eq!(placed["is_error"], json!(false), "{placed}");
        let unreadable = placed["other_sheets_unreadable"].as_array().unwrap();
        assert_eq!(unreadable.len(), 1, "{placed}");
        assert!(
            unreadable[0].as_str().unwrap().ends_with("cell.kicad_sch"),
            "{placed}"
        );
        let unannotated = serve(
            "add_schematic_component",
            &root,
            json!({"lib_id": "Device:R", "x": 50.8, "y": 25.4}),
        )
        .await;
        assert_eq!(unannotated["is_error"], json!(false), "{unannotated}");
        assert!(
            unannotated.get("other_sheets_unreadable").is_none(),
            "{unannotated}"
        );
    }

    /// An earlier item of the same batch holds its designator too.
    #[tokio::test]
    async fn a_batch_refuses_only_its_items_under_a_designator_in_use() {
        let (_dir, path) = ecc83();
        let body = serve(
            "batch_place_components",
            &path,
            json!({"components": [
                {"lib_id": "ecc83-pp:R", "reference": "R1", "x": 101.6, "y": 25.4},
                {"lib_id": "ecc83-pp:R", "reference": "R20", "x": 127.0, "y": 25.4},
                {"lib_id": "ecc83-pp:R", "reference": "R20", "x": 152.4, "y": 25.4}
            ]}),
        )
        .await;
        assert_eq!(body["placed_count"], 1, "{body}");
        let failures: Vec<(u64, &str)> = body["failures"]
            .as_array()
            .unwrap()
            .iter()
            .map(|f| (f["index"].as_u64().unwrap(), f["kind"].as_str().unwrap()))
            .collect();
        assert_eq!(
            failures,
            [(0, "invalid_argument"), (2, "invalid_argument")],
            "{body}"
        );
        let sch = cse::Schematic::load(&path).unwrap();
        for (reference, count) in [("R1", 1), ("R20", 1)] {
            let placed = sch
                .symbols
                .iter()
                .filter(|s| s.reference() == Some(reference));
            assert_eq!(placed.count(), count, "{reference}");
        }
    }
}

mod private_field_tests {
    use super::super::*;
    use crate::mcp::error::extract_error_kind;
    use crate::mcp::protocol::ToolContent;
    use crate::tools::ServerConfig;
    use std::sync::Arc;

    const SHEET: &str = include_str!("../../../tests/fixtures/component_fields_kicad10.kicad_sch");

    fn context() -> ToolContext {
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
            Arc::new(crate::router::ToolRouter::new()),
        )
    }

    /// The fixture with R1's MPN and LCSC fields marked private, the way
    /// KiCad writes them: a bare `private` token before the name.
    fn fixture() -> (tempfile::TempDir, std::path::PathBuf) {
        let source = SHEET
            .replace(
                "(property \"MPN\" \"MPN-679-R1\"",
                "(property private \"MPN\" \"MPN-679-R1\"",
            )
            .replace(
                "(property \"LCSC\" \"C679001\"",
                "(property private \"LCSC\" \"C679001\"",
            );
        assert_eq!(source.matches("(property private ").count(), 2);
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("private.kicad_sch");
        std::fs::write(&path, source).unwrap();
        (directory, path)
    }

    fn body(result: &CallToolResult) -> serde_json::Value {
        assert!(!result.is_error, "{result:?}");
        let ToolContent::Text { text } = &result.content[0] else {
            panic!("expected text result");
        };
        serde_json::from_str(text).unwrap()
    }

    #[tokio::test]
    async fn get_reports_private_fields_under_their_names() {
        let (_directory, path) = fixture();
        let result = handle_get_schematic_component(
            &json!({ "schematic": path, "reference": "R1" }),
            &context(),
        )
        .await
        .unwrap();
        let properties = &body(&result)["properties"];
        assert_eq!(properties["MPN"], "MPN-679-R1", "{properties}");
        assert_eq!(properties["LCSC"], "C679001", "{properties}");
        assert!(properties.get("private").is_none(), "{properties}");
    }

    #[tokio::test]
    async fn list_reports_private_fields_under_their_names() {
        let (_directory, path) = fixture();
        let result = handle_list_schematic_components(&json!({ "schematic": path }), &context())
            .await
            .unwrap();
        let response = body(&result);
        let r1 = response["components"]
            .as_array()
            .unwrap()
            .iter()
            .find(|component| component["reference"] == "R1")
            .expect("R1 is listed");
        assert_eq!(r1["properties"]["MPN"], "MPN-679-R1", "{r1}");
        assert!(r1["properties"].get("private").is_none(), "{r1}");
    }

    #[tokio::test]
    async fn moving_another_part_keeps_private_fields_private() {
        let (_directory, path) = fixture();
        let result = handle_move_schematic_component(
            &json!({ "schematic": path, "reference": "U1", "x": 50.8, "y": 50.8 }),
            &context(),
        )
        .await
        .unwrap();
        assert!(!result.is_error, "{result:?}");
        let committed = std::fs::read_to_string(&path).unwrap();
        assert!(
            committed.contains("(property private \"MPN\" \"MPN-679-R1\""),
            "MPN lost its flag or value"
        );
        assert!(
            committed.contains("(property private \"LCSC\" \"C679001\""),
            "LCSC lost its flag or value"
        );
        assert!(!committed.contains("(property \"private\""));
    }

    #[tokio::test]
    async fn two_private_fields_do_not_make_a_part_ambiguous() {
        let (_directory, path) = fixture();
        let result = handle_move_schematic_component(
            &json!({ "schematic": path, "reference": "R1", "x": 60.96, "y": 60.96 }),
            &context(),
        )
        .await
        .unwrap();
        assert!(
            !result.is_error,
            "{result:?} (kind {:?})",
            extract_error_kind(&result)
        );
        let committed = std::fs::read_to_string(&path).unwrap();
        assert!(committed.contains("(property private \"MPN\" \"MPN-679-R1\""));
    }

    /// Reproduces #571: placing a symbol with KiCad `private` properties
    /// (`Device:Crystal_GND24` has `KLC_S3.3` and `KLC_S4.1`) must write them
    /// as `(property private "NAME" "VALUE" …)`, not `(property "private" "NAME" …)`.
    #[tokio::test]
    async fn placing_symbol_with_private_properties_preserves_names_and_flags() {
        let dir = tempfile::tempdir().unwrap();
        let sym_path = dir.path().join("Device.kicad_sym");
        std::fs::write(
            &sym_path,
            "(kicad_symbol_lib\n\
             \t(version 20251024)\n\
             \t(generator \"kicad_symbol_editor\")\n\
             \t(generator_version \"10.0\")\n\
             \t(symbol \"Crystal_GND24\"\n\
             \t\t(property \"Reference\" \"Y\" (at 3.175 5.08 0) (effects (font (size 1.27 1.27))))\n\
             \t\t(property \"Value\" \"Crystal_GND24\" (at 3.175 3.175 0) (effects (font (size 1.27 1.27))))\n\
             \t\t(property \"Footprint\" \"\" (at 0 0 0) (hide yes) (effects (font (size 1.27 1.27))))\n\
             \t\t(property \"Datasheet\" \"\" (at 0 0 0) (hide yes) (effects (font (size 1.27 1.27))))\n\
             \t\t(property \"Description\" \"Four pin crystal, GND on pins 2 and 4\" (at 0 0 0) (hide yes) (effects (font (size 1.27 1.27))))\n\
             \t\t(property private \"KLC_S3.3\" \"The rectangle is not a symbol body but a graphical element\" (at 0 -12.7 0) (show_name yes) (hide yes) (effects (font (size 1.27 1.27))))\n\
             \t\t(property private \"KLC_S4.1\" \"Some pins are on 50mil grid to make the symbol small\" (at 0 -15.24 0) (show_name yes) (hide yes) (effects (font (size 1.27 1.27))))\n\
             \t\t(property \"ki_keywords\" \"quartz\" (at 0 0 0) (hide yes) (effects (font (size 1.27 1.27))))\n\
             \t\t(symbol \"Crystal_GND24_0_1\"\n\
             \t\t\t(polyline (pts (xy -2.54 2.286) (xy 2.54 2.286)) (stroke (width 0) (type default)) (fill (type none)))\n\
             \t\t)\n\
             \t)\n\
             )\n",
        )
        .unwrap();
        std::fs::write(
            dir.path().join("sym-lib-table"),
            format!(
                "(sym_lib_table (version 7) (lib (name \"Device\") (type \"KiCad\") (uri {}) (options \"\") (descr \"\")))",
                crate::tools::library::quote_lib_table_string(&sym_path.display().to_string())
            ),
        )
        .unwrap();
        let sch_path = dir.path().join("test.kicad_sch");
        std::fs::write(
            &sch_path,
            "(kicad_sch (version 20250114) (generator \"eeschema\") (generator_version \"10.0\") (uuid \"00000000-0000-0000-0000-000000000001\") (paper \"A4\") (lib_symbols) (sheet_instances (path \"/\" (page \"1\"))))\n",
        )
        .unwrap();

        let ctx = context();
        let result = handle_add_schematic_component(
            &json!({
                "schematic": sch_path,
                "lib_id": "Device:Crystal_GND24",
                "x": 100.33,
                "y": 100.33,
                "reference": "Y1"
            }),
            &ctx,
        )
        .await
        .unwrap();
        assert!(!result.is_error, "{result:?}");

        let committed = std::fs::read_to_string(&sch_path).unwrap();
        // One in the embedded lib_symbols copy, one on the placed instance.
        assert_eq!(
            committed
                .matches("(property private \"KLC_S3.3\" \"The rectangle is not a symbol body but a graphical element\"")
                .count(),
            2,
            "KLC_S3.3 missing or lost private flag on the instance: {committed}"
        );
        // One in the embedded lib_symbols copy, one on the placed instance.
        assert_eq!(
            committed
                .matches("(property private \"KLC_S4.1\" \"Some pins are on 50mil grid to make the symbol small\"")
                .count(),
            2,
            "KLC_S4.1 missing or lost private flag on the instance: {committed}"
        );
        assert!(
            !committed.contains("(property \"private\""),
            "wrote bogus \"private\" field: {committed}"
        );

        let edit = handle_edit_schematic_component(
            &json!({
                "schematic": sch_path,
                "reference": "Y1",
                "value": "16MHz"
            }),
            &ctx,
        )
        .await
        .unwrap();
        assert!(!edit.is_error, "edit after placement failed: {edit:?}");
    }

    #[tokio::test]
    async fn batch_place_and_served_dispatch_preserve_private_properties_and_literal_private_name()
    {
        let dir = tempfile::tempdir().unwrap();
        let sym_path = dir.path().join("Device.kicad_sym");
        std::fs::write(
            &sym_path,
            "(kicad_symbol_lib\n\
             \t(version 20251024)\n\
             \t(generator \"kicad_symbol_editor\")\n\
             \t(generator_version \"10.0\")\n\
             \t(symbol \"Crystal_GND24\"\n\
             \t\t(property \"Reference\" \"Y\" (at 3.175 5.08 0) (effects (font (size 1.27 1.27))))\n\
             \t\t(property \"Value\" \"Crystal_GND24\" (at 3.175 3.175 0) (effects (font (size 1.27 1.27))))\n\
             \t\t(property \"Footprint\" \"\" (at 0 0 0) (hide yes) (effects (font (size 1.27 1.27))))\n\
             \t\t(property \"Datasheet\" \"\" (at 0 0 0) (hide yes) (effects (font (size 1.27 1.27))))\n\
             \t\t(property \"Description\" \"Four pin crystal\" (at 0 0 0) (hide yes) (effects (font (size 1.27 1.27))))\n\
             \t\t(property private \"KLC_S3.3\" \"The rectangle is not a symbol body\" (at 0 -12.7 0) (show_name yes) (hide yes) (effects (font (size 1.27 1.27))))\n\
             \t\t(property private \"KLC_S4.1\" \"Some pins are on 50mil grid\" (at 0 -15.24 0) (show_name yes) (hide yes) (effects (font (size 1.27 1.27))))\n\
             \t\t(property \"private\" \"literal-value\" (at 0 0 0) (hide yes) (effects (font (size 1.27 1.27))))\n\
             \t\t(symbol \"Crystal_GND24_0_1\"\n\
             \t\t\t(polyline (pts (xy -2.54 2.286) (xy 2.54 2.286)) (stroke (width 0) (type default)) (fill (type none)))\n\
             \t\t)\n\
             \t)\n\
             \t(symbol \"R\"\n\
             \t\t(property \"Reference\" \"R\" (at 2.032 0 90) (effects (font (size 1.27 1.27))))\n\
             \t\t(property \"Value\" \"R\" (at 0 0 90) (effects (font (size 1.27 1.27))))\n\
             \t\t(property \"Footprint\" \"\" (at 0 0 0) (hide yes) (effects (font (size 1.27 1.27))))\n\
             \t\t(property \"Datasheet\" \"~\" (at 0 0 0) (hide yes) (effects (font (size 1.27 1.27))))\n\
             \t\t(property \"Description\" \"Resistor\" (at 0 0 0) (hide yes) (effects (font (size 1.27 1.27))))\n\
             \t\t(symbol \"R_0_1\"\n\
             \t\t\t(rectangle (start -1.016 -2.54) (end 1.016 2.54) (stroke (width 0.254) (type default)) (fill (type none)))\n\
             \t\t)\n\
             \t)\n\
             )\n",
        )
        .unwrap();
        std::fs::write(
            dir.path().join("sym-lib-table"),
            format!(
                "(sym_lib_table (version 7) (lib (name \"Device\") (type \"KiCad\") (uri {}) (options \"\") (descr \"\")))",
                crate::tools::library::quote_lib_table_string(&sym_path.display().to_string())
            ),
        )
        .unwrap();
        let sch_path = dir.path().join("test.kicad_sch");
        std::fs::write(
            &sch_path,
            "(kicad_sch (version 20250114) (generator \"eeschema\") (generator_version \"10.0\") (uuid \"00000000-0000-0000-0000-000000000001\") (paper \"A4\") (lib_symbols) (sheet_instances (path \"/\" (page \"1\"))))\n",
        )
        .unwrap();

        // Served dispatch via McpHandler::handle_message for add_schematic_component.
        let handler = crate::mcp::handler::McpHandler::new(ServerConfig {
            project_dir: Some(dir.path().to_path_buf()),
            eager_toolsets: true,
            ..ServerConfig::default()
        })
        .await
        .unwrap();
        let served_resp = handler
            .handle_message(json!({
                "jsonrpc": "2.0",
                "id": 1,
                "method": "tools/call",
                "params": {
                    "name": "add_schematic_component",
                    "arguments": {
                        "schematic": sch_path,
                        "lib_id": "Device:Crystal_GND24",
                        "x": 100.33,
                        "y": 100.33,
                        "reference": "Y1"
                    }
                }
            }))
            .await
            .unwrap();
        let served_result = served_resp.result.as_ref().unwrap();
        let is_err = served_result["isError"].as_bool().unwrap_or(false);
        assert!(
            !is_err,
            "served add_schematic_component failed: {served_resp:?}"
        );

        // Sibling route: batch_place_components with Crystal_GND24 and R.
        let batch_resp = handler
            .handle_message(json!({
                "jsonrpc": "2.0",
                "id": 2,
                "method": "tools/call",
                "params": {
                    "name": "batch_place_components",
                    "arguments": {
                        "schematic": sch_path,
                        "components": [
                            { "lib_id": "Device:Crystal_GND24", "x": 120.65, "y": 100.33, "reference": "Y2" },
                            { "lib_id": "Device:R", "x": 140.97, "y": 100.33, "reference": "R1", "value": "10k" }
                        ]
                    }
                }
            }))
            .await
            .unwrap();
        let batch_result = batch_resp.result.as_ref().unwrap();
        let batch_err = batch_result["isError"].as_bool().unwrap_or(false);
        assert!(
            !batch_err,
            "served batch_place_components failed: {batch_resp:?}"
        );

        let committed = std::fs::read_to_string(&sch_path).unwrap();
        assert_eq!(
            committed
                .matches("(property private \"KLC_S3.3\" \"The rectangle is not a symbol body\"")
                .count(),
            3,
            "expected 1 lib_symbols + 2 placed instances for KLC_S3.3: {committed}"
        );
        assert_eq!(
            committed
                .matches("(property private \"KLC_S4.1\" \"Some pins are on 50mil grid\"")
                .count(),
            3,
            "expected 1 lib_symbols + 2 placed instances for KLC_S4.1: {committed}"
        );
        assert_eq!(
            committed
                .matches("(property \"private\" \"literal-value\"")
                .count(),
            3,
            "quoted \"private\" field name must stay non-private: {committed}"
        );
    }
}

/// A write KiCad's editor lock refused wrote nothing, so it is a `conflict`
/// refusal that names the lock, never `mutation_outcome_uncertain` (#427).
mod editor_lock_refusal_tests {
    use crate::tools::ServerConfig;
    use serde_json::{json, Value};

    const PLACED: &str = include_str!("../../../tests/fixtures/derived_lib_name.kicad_sch");
    const UNANNOTATED: &str = include_str!("../../../tests/fixtures/annotate_duplicates.kicad_sch");

    #[tokio::test]
    async fn served_placement_and_annotation_refuse_a_locked_schematic() {
        let handler = crate::mcp::handler::McpHandler::new(ServerConfig {
            eager_toolsets: true,
            ..ServerConfig::default()
        })
        .await
        .unwrap();
        let cases = [
            (
                "add_schematic_component",
                ("derived_lib_name", PLACED),
                json!({ "lib_id": "Device:R", "x": 190.5, "y": 101.6, "reference": "R9" }),
                1,
            ),
            (
                "batch_place_components",
                ("derived_lib_name", PLACED),
                json!({ "components": [
                    { "lib_id": "Device:R", "x": 190.5, "y": 101.6, "reference": "R9" },
                    { "lib_id": "Device:R", "x": 203.2, "y": 101.6, "reference": "R10" }
                ] }),
                2,
            ),
            (
                "annotate_schematic",
                ("adj", UNANNOTATED),
                json!({}),
                // Two `C?` to number, and the duplicate `R1`s left unresolved.
                3,
            ),
        ];
        for (tool, (project, source), mut arguments, requested) in cases {
            // Named after the fixture's project, which its instances record.
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join(format!("{project}.kicad_sch"));
            std::fs::write(&path, source).unwrap();
            std::fs::write(dir.path().join(format!("{project}.kicad_pro")), "{}\n").unwrap();
            let lock = crate::test_support::resolved_dir(dir.path())
                .join(format!("~{project}.kicad_sch.lck"));
            std::fs::write(&lock, r#"{"hostname":"x","username":"y"}"#).unwrap();
            arguments["schematic"] = json!(path.display().to_string());

            let response = handler
                .handle_message(json!({
                    "jsonrpc": "2.0", "id": 427, "method": "tools/call",
                    "params": { "name": tool, "arguments": arguments }
                }))
                .await
                .unwrap()
                .result
                .unwrap();
            assert_eq!(response["isError"], true, "{tool}: {response}");
            let body: Value =
                serde_json::from_str(response["content"][0]["text"].as_str().unwrap()).unwrap();
            assert_eq!(body["error"]["kind"], "conflict", "{tool}: {body}");
            assert_eq!(
                body["error"]["paths"],
                json!([path.display().to_string()]),
                "{tool}: {body}"
            );
            let message = body["message"].as_str().unwrap();
            assert!(message.contains("wrote nothing"), "{tool}: {body}");
            assert!(
                message.contains(&lock.display().to_string()),
                "{tool}: {body}"
            );
            let outcome = &body["outcome"];
            assert_eq!(outcome["status"], "failed", "{tool}: {body}");
            assert_eq!(outcome["completed"], 0, "{tool}: {body}");
            assert_eq!(outcome["failed"], requested, "{tool}: {body}");
            assert_eq!(outcome["retry"]["safe"], true, "{tool}: {body}");
            assert_eq!(std::fs::read_to_string(&path).unwrap(), source, "{tool}");
            assert!(lock.exists(), "{tool} must leave KiCad's lock alone");
        }
    }
}

mod root_editor_lock_tests {
    use crate::mcp::handler::McpHandler;
    use crate::tools::ServerConfig;
    use serde_json::{json, Value};

    async fn call(handler: &McpHandler, name: &str, arguments: Value) -> Value {
        let response = handler
            .handle_message(json!({
                "jsonrpc": "2.0", "id": 428, "method": "tools/call",
                "params": { "name": name, "arguments": arguments }
            }))
            .await
            .unwrap()
            .result
            .unwrap();
        serde_json::from_str(response["content"][0]["text"].as_str().unwrap()).unwrap()
    }

    /// Tools that report an `outcome` envelope with their refusal.
    const OUTCOME_TOOLS: [&str; 3] = [
        "add_schematic_component",
        "batch_place_components",
        "annotate_schematic",
    ];

    async fn root_lock_case(tool: &str, mut arguments: Value) {
        let handler = McpHandler::new(ServerConfig {
            eager_toolsets: true,
            ..ServerConfig::default()
        })
        .await
        .unwrap();
        let directory = tempfile::tempdir().unwrap();
        let fixtures = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures");
        let names = [
            "nested_pages.kicad_pro",
            "nested_pages.kicad_sch",
            "bank.kicad_sch",
            "cell.kicad_sch",
        ];
        for name in names {
            std::fs::copy(
                fixtures.join("annotate_nested_hierarchy").join(name),
                directory.path().join(name),
            )
            .unwrap();
        }
        let child = directory.path().join("bank.kicad_sch");
        // Use the existing KiCad-authored library, independent of installed libraries.
        let library = fixtures.join("reference_prefix_kicad10/Device.kicad_sym");
        std::fs::write(directory.path().join("sym-lib-table"), format!(
            "(sym_lib_table (version 7) (lib (name \"Device\") (type \"KiCad\") (uri {}) (options \"\") (descr \"\")))",
            crate::tools::library::quote_lib_table_string(&library.display().to_string())
        )).unwrap();
        // An unannotated part for annotation, and R99 for the tools that edit one.
        // `bank` is placed twice, so annotation would also see R99 as a duplicate.
        let parts: &[_] = if tool == "annotate_schematic" {
            &[(200.66, None)]
        } else {
            &[(200.66, None), (226.06, Some("R99"))]
        };
        for &(x, reference) in parts {
            let mut part = json!({ "schematic": child, "lib_id": "Device:R", "x": x, "y": 101.6 });
            if let Some(reference) = reference {
                part["reference"] = json!(reference);
            }
            let prepared = call(&handler, "add_schematic_component", part).await;
            assert_eq!(prepared["outcome"]["status"], "complete", "{prepared}");
        }
        let before: Vec<_> = names
            .iter()
            .map(|name| std::fs::read(directory.path().join(name)).unwrap())
            .collect();
        let root = directory.path().join("nested_pages.kicad_sch");
        let lock =
            crate::test_support::resolved_dir(directory.path()).join("~nested_pages.kicad_sch.lck");
        std::fs::write(&lock, r#"{"hostname":"x","username":"y"}"#).unwrap();
        arguments["schematic"] = json!(child);
        let refused = call(&handler, tool, arguments.clone()).await;
        if tool == "delete_schematic_component" {
            // Its document binding sees the root lock before the writer does.
            assert_eq!(refused["error"]["kind"], "stale_target", "{refused}");
            assert_eq!(refused["error"]["target"], json!(child), "{refused}");
        } else {
            assert_eq!(refused["error"]["kind"], "conflict", "{tool}: {refused}");
            assert_eq!(refused["error"]["paths"], json!([root]), "{refused}");
            let message = refused["message"].as_str().unwrap();
            assert!(message.contains(&root.display().to_string()), "{refused}");
        }
        if OUTCOME_TOOLS.contains(&tool) {
            let message = refused["message"].as_str().unwrap();
            assert!(message.contains(&lock.display().to_string()), "{refused}");
            assert_eq!(refused["outcome"]["status"], "failed", "{refused}");
            assert_eq!(refused["outcome"]["completed"], 0, "{refused}");
            assert_eq!(refused["outcome"]["retry"]["safe"], true, "{refused}");
        }
        for (name, expected) in names.iter().zip(&before) {
            assert_eq!(
                &std::fs::read(directory.path().join(name)).unwrap(),
                expected,
                "{tool}: {name}"
            );
        }
        assert!(lock.exists());
        if tool == "annotate_schematic" {
            let mut dry = arguments.clone();
            dry["dry_run"] = json!(true);
            let planned = call(&handler, tool, dry).await;
            assert_eq!(planned["outcome"]["status"], "complete", "{planned}");
            assert_eq!(std::fs::read(&child).unwrap(), before[2]);
        }
        // The same request succeeds once Eeschema releases its root lock.
        std::fs::remove_file(lock).unwrap();
        let applied = call(&handler, tool, arguments).await;
        assert!(applied.get("error").is_none(), "{tool}: {applied}");
        assert_ne!(std::fs::read(&child).unwrap(), before[2], "{tool}");
        for index in [0, 1, 3] {
            assert_eq!(
                std::fs::read(directory.path().join(names[index])).unwrap(),
                before[index],
                "{tool}"
            );
        }
    }

    #[tokio::test]
    async fn root_lock_refuses_single_child_placement() {
        root_lock_case(
            "add_schematic_component",
            json!({"lib_id":"Device:R", "x":251.46,"y":101.6,"reference":"R98"}),
        )
        .await;
    }
    #[tokio::test]
    async fn root_lock_refuses_batch_child_placement() {
        root_lock_case(
            "batch_place_components",
            json!({"components":[{"lib_id":"Device:R", "x":251.46,"y":101.6,"reference":"R98"}]}),
        )
        .await;
    }
    #[tokio::test]
    async fn root_lock_refuses_child_template() {
        root_lock_case("apply_template", json!({"template_id":"i2c_pullups"})).await;
    }
    #[tokio::test]
    async fn root_lock_refuses_child_file_annotation() {
        root_lock_case("annotate_schematic", json!({"schematic_scope":"file"})).await;
    }

    /// Every other sheet writer reaches the same check in the shared writer.
    #[tokio::test]
    async fn root_lock_refuses_other_child_writes() {
        let cases = [
            (
                "add_wire",
                json!({"x1": 100.33, "y1": 50.8, "x2": 120.65, "y2": 50.8}),
            ),
            ("add_junction", json!({"x": 100.33, "y": 50.8})),
            ("add_no_connect", json!({"x": 100.33, "y": 50.8})),
            (
                "add_schematic_net_label",
                json!({"net": "SIG", "x": 100.33, "y": 50.8}),
            ),
            (
                "move_schematic_component",
                json!({"reference": "R99", "x": 251.46, "y": 101.6}),
            ),
            (
                "edit_schematic_component",
                json!({"reference": "R99", "value": "10k"}),
            ),
            ("delete_schematic_component", json!({"reference": "R99"})),
            (
                "batch_edit_schematic_components",
                json!({"edits":[{"reference":"R99", "field_placements":{"Value":{"x":238.76,"y":104.14}}}]}),
            ),
        ];
        for (tool, arguments) in cases {
            root_lock_case(tool, arguments).await;
        }
    }
}

mod structured_error_kind_tests {
    use crate::mcp::handler::McpHandler;
    use crate::tools::ServerConfig;
    use copperpour_sexp::paths::display_path;
    use serde_json::{json, Value};
    use std::path::{Path, PathBuf};

    async fn call_raw(handler: &McpHandler, name: &str, arguments: Value) -> (bool, Value) {
        let response = handler
            .handle_message(json!({
                "jsonrpc": "2.0",
                "id": 741,
                "method": "tools/call",
                "params": { "name": name, "arguments": arguments }
            }))
            .await
            .unwrap()
            .result
            .unwrap();
        let is_error = response["isError"].as_bool().unwrap_or(false);
        let body: Value =
            serde_json::from_str(response["content"][0]["text"].as_str().unwrap()).unwrap();
        (is_error, body)
    }

    async fn test_handler() -> McpHandler {
        McpHandler::new(ServerConfig {
            eager_toolsets: true,
            ..ServerConfig::default()
        })
        .await
        .unwrap()
    }

    fn ecc83_multiunit_source() -> String {
        std::fs::read_to_string(
            Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/ecc83_multiunit.kicad_sch"),
        )
        .unwrap()
    }

    fn setup_workspace_with_device_lib() -> (tempfile::TempDir, PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let fixtures = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures");
        let library = fixtures.join("reference_prefix_kicad10/Device.kicad_sym");
        std::fs::write(
            dir.path().join("sym-lib-table"),
            format!(
                "(sym_lib_table (version 7) (lib (name \"Device\") (type \"KiCad\") (uri {}) (options \"\") (descr \"\")))",
                crate::tools::library::quote_lib_table_string(&library.display().to_string())
            ),
        )
        .unwrap();
        let sch = dir.path().join("ecc83-pp.kicad_sch");
        std::fs::write(&sch, ecc83_multiunit_source()).unwrap();
        (dir, sch)
    }

    /// Replace the first `ecc83-pp:ECC83` definition in `lib_symbols` with an
    /// unflattened `(extends ...)` stub so `get_schematic_pin_locations` hits
    /// its stub refusal on a variant derived from the KiCad-resaved fixture.
    fn with_ecc83_extends_stub(source: &str) -> String {
        let needle = "\t\t(symbol \"ecc83-pp:ECC83\"";
        let start = source
            .find(needle)
            .expect("ecc83_multiunit.kicad_sch defines ecc83-pp:ECC83");
        let (rel_start, rel_end) =
            copperpour_sexp::writer::find_block_with_leading_whitespace(source, start + 2)
                .expect("ecc83-pp:ECC83 lib_symbol block is balanced");
        let mut out = String::with_capacity(source.len());
        out.push_str(&source[..rel_start]);
        out.push_str("\n\t\t(symbol \"ecc83-pp:ECC83\" (extends \"ecc83-pp:Base\"))");
        out.push_str(&source[rel_end..]);
        out
    }

    /// Inject a private `"Note"` into U1 unit 1 and a public `"Note"` into U1
    /// unit 2, leaving U1 unit 3 without `"Note"` so adding `"Note"` to missing
    /// units faces an undecidable private-vs-public choice on a valid 3-unit sheet.
    fn with_mixed_note_visibility(source: &str) -> String {
        let u1_unit1_anchor = "(property \"Reference\" \"U1\"\n\t\t\t(at 163.83 55.88 0)";
        let u1_unit2_anchor = "(property \"Reference\" \"U1\"\n\t\t\t(at 161.29 99.06 0)";
        assert!(source.contains(u1_unit1_anchor));
        assert!(source.contains(u1_unit2_anchor));
        source
            .replacen(
                u1_unit1_anchor,
                "(property private \"Note\" \"a\" (at 160.02 64.77 0) (effects (font (size 1.27 1.27)) (hide yes)))\n\t\t(property \"Reference\" \"U1\"\n\t\t\t(at 163.83 55.88 0)",
                1,
            )
            .replacen(
                u1_unit2_anchor,
                "(property \"Note\" \"b\" (at 157.48 107.95 0) (effects (font (size 1.27 1.27)) (hide yes)))\n\t\t(property \"Reference\" \"U1\"\n\t\t\t(at 161.29 99.06 0)",
                1,
            )
    }

    /// Inject a structurally malformed `(property "Note")` (missing quoted value)
    /// into U1 unit 1 while keeping the surrounding S-expression parseable.
    fn with_malformed_note_property(source: &str) -> String {
        let u1_unit1_anchor = "(property \"Reference\" \"U1\"\n\t\t\t(at 163.83 55.88 0)";
        assert!(source.contains(u1_unit1_anchor));
        source.replacen(
            u1_unit1_anchor,
            "(property \"Note\")\n\t\t(property \"Reference\" \"U1\"\n\t\t\t(at 163.83 55.88 0)",
            1,
        )
    }

    #[tokio::test]
    async fn served_placement_and_batch_invalid_unit_return_invalid_argument() {
        let handler = test_handler().await;
        let (_dir, sch) = setup_workspace_with_device_lib();
        let before = std::fs::read_to_string(&sch).unwrap();

        let (is_error, body) = call_raw(
            &handler,
            "add_schematic_component",
            json!({
                "schematic": sch.to_str().unwrap(),
                "lib_id": "Device:R",
                "x": 50.8,
                "y": 50.8,
                "unit": 2
            }),
        )
        .await;
        assert!(is_error, "{body}");
        assert_eq!(body["error"]["kind"], "invalid_argument", "{body}");
        assert_eq!(body["error"]["field"], "unit", "{body}");
        assert!(
            body["message"]
                .as_str()
                .unwrap()
                .contains("Invalid unit 2 for 'Device:R'"),
            "{body}"
        );
        assert_eq!(std::fs::read_to_string(&sch).unwrap(), before);

        let (is_error, body) = call_raw(
            &handler,
            "batch_place_components",
            json!({
                "schematic": sch.to_str().unwrap(),
                "components": [
                    { "lib_id": "Device:R", "x": 50.8, "y": 50.8, "unit": 2 }
                ]
            }),
        )
        .await;
        assert!(is_error, "{body}");
        assert_eq!(body["failures"][0]["kind"], "invalid_argument", "{body}");
        assert!(
            body["failures"][0]["message"]
                .as_str()
                .unwrap()
                .contains("Invalid unit 2 for 'Device:R'"),
            "{body}"
        );
        assert_eq!(std::fs::read_to_string(&sch).unwrap(), before);
    }

    #[tokio::test]
    async fn served_move_connected_returns_unsupported_capability() {
        let handler = test_handler().await;
        let (is_error, body) = call_raw(&handler, "move_connected", json!({})).await;
        assert!(is_error, "{body}");
        assert_eq!(body["error"]["kind"], "unsupported_capability", "{body}");
        assert_eq!(body["error"]["capability"], "move_connected", "{body}");
        assert!(
            body["message"]
                .as_str()
                .unwrap()
                .contains("move_connected is not implemented"),
            "{body}"
        );
    }

    #[tokio::test]
    async fn served_query_refusals_return_stale_target_and_malformed_file() {
        let handler = test_handler().await;
        let (_dir, sch) = setup_workspace_with_device_lib();
        let baseline = ecc83_multiunit_source();

        for tool in ["get_schematic_component", "get_schematic_pin_locations"] {
            let (is_error, body) = call_raw(
                &handler,
                tool,
                json!({ "schematic": sch.to_str().unwrap(), "reference": "R999" }),
            )
            .await;
            assert!(is_error, "{tool}: {body}");
            assert_eq!(body["error"]["kind"], "stale_target", "{tool}: {body}");
            assert_eq!(body["error"]["target"], "component R999", "{tool}: {body}");
            assert!(
                body["message"].as_str().unwrap().contains("R999"),
                "{tool}: {body}"
            );
        }

        // Missing embedded definition in lib_symbols -> malformed_file.
        let missing_def =
            baseline.replace("(symbol \"ecc83-pp:ECC83\"", "(symbol \"ecc83-pp:Other\"");
        std::fs::write(&sch, missing_def).unwrap();
        let (is_error, body) = call_raw(
            &handler,
            "get_schematic_pin_locations",
            json!({ "schematic": sch.to_str().unwrap(), "reference": "U1" }),
        )
        .await;
        assert!(is_error, "{body}");
        assert_eq!(body["error"]["kind"], "malformed_file", "{body}");
        assert_eq!(body["error"]["path"], display_path(&sch), "{body}");
        assert!(
            body["message"]
                .as_str()
                .unwrap()
                .contains("has no embedded definition"),
            "{body}"
        );

        // Unflattened (extends ...) stub derived from the resaved fixture -> malformed_file.
        std::fs::write(&sch, with_ecc83_extends_stub(&baseline)).unwrap();
        let (is_error, body) = call_raw(
            &handler,
            "get_schematic_pin_locations",
            json!({ "schematic": sch.to_str().unwrap(), "reference": "U1" }),
        )
        .await;
        assert!(is_error, "{body}");
        assert_eq!(body["error"]["kind"], "malformed_file", "{body}");
        assert_eq!(body["error"]["path"], display_path(&sch), "{body}");
        assert!(
            body["message"]
                .as_str()
                .unwrap()
                .contains("(extends \"ecc83-pp:Base\") stub"),
            "{body}"
        );
    }

    #[tokio::test]
    async fn served_library_ops_refusals_return_structured_error_kinds() {
        let handler = test_handler().await;
        let (_dir, sch) = setup_workspace_with_device_lib();
        let baseline = ecc83_multiunit_source();

        // Place R98 for single-unit checks.
        let (is_error, placed) = call_raw(
            &handler,
            "add_schematic_component",
            json!({
                "schematic": sch.to_str().unwrap(),
                "lib_id": "Device:R",
                "x": 50.8,
                "y": 50.8,
                "reference": "R98"
            }),
        )
        .await;
        assert!(!is_error, "{placed}");
        let single_unit_before = std::fs::read_to_string(&sch).unwrap();

        // add_component_annotation with built-in field -> invalid_argument.
        let (is_error, body) = call_raw(
            &handler,
            "add_component_annotation",
            json!({
                "schematic": sch.to_str().unwrap(),
                "reference": "R98",
                "key": "Value",
                "value": "10k"
            }),
        )
        .await;
        assert!(is_error, "{body}");
        assert_eq!(body["error"]["kind"], "invalid_argument", "{body}");
        assert_eq!(body["error"]["field"], "key", "{body}");
        assert!(
            body["message"]
                .as_str()
                .unwrap()
                .contains("'Value' is a built-in field"),
            "{body}"
        );
        assert_eq!(std::fs::read_to_string(&sch).unwrap(), single_unit_before);

        // group_components with empty references -> invalid_argument.
        let (is_error, body) = call_raw(
            &handler,
            "group_components",
            json!({
                "schematic": sch.to_str().unwrap(),
                "group_name": "power",
                "references": []
            }),
        )
        .await;
        assert!(is_error, "{body}");
        assert_eq!(body["error"]["kind"], "invalid_argument", "{body}");
        assert_eq!(body["error"]["field"], "references", "{body}");
        assert_eq!(body["message"], "No references provided", "{body}");

        // update_symbols_from_library with missing reference -> stale_target.
        let (is_error, body) = call_raw(
            &handler,
            "update_symbols_from_library",
            json!({
                "schematic": sch.to_str().unwrap(),
                "references": ["R999"]
            }),
        )
        .await;
        assert!(is_error, "{body}");
        assert_eq!(body["error"]["kind"], "stale_target", "{body}");
        assert_eq!(body["error"]["target"], "component R999", "{body}");

        // replace_component missing reference -> stale_target.
        let (is_error, body) = call_raw(
            &handler,
            "replace_component",
            json!({
                "schematic": sch.to_str().unwrap(),
                "reference": "R999",
                "new_lib_id": "Device:R"
            }),
        )
        .await;
        assert!(is_error, "{body}");
        assert_eq!(body["error"]["kind"], "stale_target", "{body}");
        assert_eq!(body["error"]["target"], "component R999", "{body}");

        // replace_component out-of-range unit -> invalid_argument.
        let (is_error, body) = call_raw(
            &handler,
            "replace_component",
            json!({
                "schematic": sch.to_str().unwrap(),
                "reference": "R98",
                "new_lib_id": "Device:R",
                "unit": 2
            }),
        )
        .await;
        assert!(is_error, "{body}");
        assert_eq!(body["error"]["kind"], "invalid_argument", "{body}");
        assert_eq!(body["error"]["field"], "unit", "{body}");
        assert_eq!(std::fs::read_to_string(&sch).unwrap(), single_unit_before);

        // Multi-unit schematic checks on the resaved 3-unit U1 (ecc83-pp:ECC83).
        std::fs::write(&sch, &baseline).unwrap();

        // replace_component with unit override on multi-unit placement -> invalid_argument.
        let (is_error, body) = call_raw(
            &handler,
            "replace_component",
            json!({
                "schematic": sch.to_str().unwrap(),
                "reference": "U1",
                "new_lib_id": "Device:R",
                "unit": 1
            }),
        )
        .await;
        assert!(is_error, "{body}");
        assert_eq!(body["error"]["kind"], "invalid_argument", "{body}");
        assert_eq!(body["error"]["field"], "unit", "{body}");
        assert_eq!(std::fs::read_to_string(&sch).unwrap(), baseline);

        // replace_component replacing 3-unit U1 with 1-unit Device:R -> plan_blocked.
        let (is_error, body) = call_raw(
            &handler,
            "replace_component",
            json!({
                "schematic": sch.to_str().unwrap(),
                "reference": "U1",
                "new_lib_id": "Device:R"
            }),
        )
        .await;
        assert!(is_error, "{body}");
        assert_eq!(body["error"]["kind"], "plan_blocked", "{body}");
        assert_eq!(body["error"]["operation"], "replace_component", "{body}");
        assert_eq!(std::fs::read_to_string(&sch).unwrap(), baseline);

        // replace_component with inconsistent lib_ids across units -> malformed_file.
        let inconsistent = baseline.replacen(
            "(lib_id \"ecc83-pp:ECC83\")",
            "(lib_id \"ecc83-pp:Other\")",
            1,
        );
        std::fs::write(&sch, &inconsistent).unwrap();
        let (is_error, body) = call_raw(
            &handler,
            "replace_component",
            json!({
                "schematic": sch.to_str().unwrap(),
                "reference": "U1",
                "new_lib_id": "ecc83-pp:ECC83"
            }),
        )
        .await;
        assert!(is_error, "{body}");
        assert_eq!(body["error"]["kind"], "malformed_file", "{body}");
        assert_eq!(body["error"]["path"], display_path(&sch), "{body}");
        assert!(
            body["message"]
                .as_str()
                .unwrap()
                .contains("inconsistent library ids"),
            "{body}"
        );

        // add_component_annotation when units have mixed private/public copies -> plan_blocked
        // (valid KiCad file, undecidable visibility choice for the missing unit 3).
        let mixed = with_mixed_note_visibility(&baseline);
        std::fs::write(&sch, &mixed).unwrap();
        let (is_error, body) = call_raw(
            &handler,
            "add_component_annotation",
            json!({
                "schematic": sch.to_str().unwrap(),
                "reference": "U1",
                "key": "Note",
                "value": "c"
            }),
        )
        .await;
        assert!(is_error, "{body}");
        assert_eq!(body["error"]["kind"], "plan_blocked", "{body}");
        assert_eq!(
            body["error"]["operation"], "add_component_annotation",
            "{body}"
        );
        assert!(
            body["message"]
                .as_str()
                .unwrap()
                .contains("both private and public copies"),
            "{body}"
        );
        assert_eq!(std::fs::read_to_string(&sch).unwrap(), mixed);

        // add_component_annotation on a structurally malformed property block -> malformed_file.
        let malformed = with_malformed_note_property(&baseline);
        std::fs::write(&sch, &malformed).unwrap();
        let (is_error, body) = call_raw(
            &handler,
            "add_component_annotation",
            json!({
                "schematic": sch.to_str().unwrap(),
                "reference": "U1",
                "key": "Other",
                "value": "v"
            }),
        )
        .await;
        assert!(is_error, "{body}");
        assert_eq!(body["error"]["kind"], "malformed_file", "{body}");
        assert_eq!(body["error"]["path"], display_path(&sch), "{body}");
        assert_eq!(std::fs::read_to_string(&sch).unwrap(), malformed);
    }

    #[tokio::test]
    async fn served_edit_component_refusals_preserve_distinct_error_kinds() {
        let handler = test_handler().await;
        let (_dir, sch) = setup_workspace_with_device_lib();
        let baseline = ecc83_multiunit_source();
        std::fs::write(&sch, &baseline).unwrap();

        // Reserved field in `fields` -> invalid_argument (field: "fields").
        let (is_error, body) = call_raw(
            &handler,
            "edit_schematic_component",
            json!({
                "schematic": sch.to_str().unwrap(),
                "reference": "U1",
                "fields": { "Value": "12AX7" }
            }),
        )
        .await;
        assert!(is_error, "{body}");
        assert_eq!(body["error"]["kind"], "invalid_argument", "{body}");
        assert_eq!(body["error"]["field"], "fields", "{body}");
        assert!(
            body["message"]
                .as_str()
                .unwrap()
                .contains("No fields were updated on 'U1'"),
            "{body}"
        );

        // Non-string value in `fields` -> invalid_argument (field: "fields").
        let (is_error, body) = call_raw(
            &handler,
            "edit_schematic_component",
            json!({
                "schematic": sch.to_str().unwrap(),
                "reference": "U1",
                "fields": { "Note": 42 }
            }),
        )
        .await;
        assert!(is_error, "{body}");
        assert_eq!(body["error"]["kind"], "invalid_argument", "{body}");
        assert_eq!(body["error"]["field"], "fields", "{body}");

        // Invalid `field_placements` entry -> invalid_argument (field: "field_placements").
        let (is_error, body) = call_raw(
            &handler,
            "edit_schematic_component",
            json!({
                "schematic": sch.to_str().unwrap(),
                "reference": "U1",
                "field_placements": { "Value": {} }
            }),
        )
        .await;
        assert!(is_error, "{body}");
        assert_eq!(body["error"]["kind"], "invalid_argument", "{body}");
        assert_eq!(body["error"]["field"], "field_placements", "{body}");

        // Multi-unit field position without `unit` -> invalid_argument (field: "unit").
        let (is_error, body) = call_raw(
            &handler,
            "edit_schematic_component",
            json!({
                "schematic": sch.to_str().unwrap(),
                "reference": "U1",
                "field_placements": { "Value": { "x": 60.0 } }
            }),
        )
        .await;
        assert!(is_error, "{body}");
        assert_eq!(body["error"]["kind"], "invalid_argument", "{body}");
        assert_eq!(body["error"]["field"], "unit", "{body}");

        // Missing target field in `field_placements` -> stale_target.
        let (is_error, body) = call_raw(
            &handler,
            "edit_schematic_component",
            json!({
                "schematic": sch.to_str().unwrap(),
                "reference": "U1",
                "field_placements": { "DoesNotExist": { "hide": true } }
            }),
        )
        .await;
        assert!(is_error, "{body}");
        assert_eq!(body["error"]["kind"], "stale_target", "{body}");
        assert_eq!(body["error"]["target"], "component U1", "{body}");
        assert_eq!(std::fs::read_to_string(&sch).unwrap(), baseline);

        // Mixed private/public custom property across units -> plan_blocked.
        let mixed = with_mixed_note_visibility(&baseline);
        std::fs::write(&sch, &mixed).unwrap();
        let (is_error, body) = call_raw(
            &handler,
            "edit_schematic_component",
            json!({
                "schematic": sch.to_str().unwrap(),
                "reference": "U1",
                "fields": { "Note": "c" }
            }),
        )
        .await;
        assert!(is_error, "{body}");
        assert_eq!(body["error"]["kind"], "plan_blocked", "{body}");
        assert_eq!(
            body["error"]["operation"], "edit_schematic_component",
            "{body}"
        );
        assert_eq!(std::fs::read_to_string(&sch).unwrap(), mixed);

        // `remove_fields` on a malformed property block -> malformed_file (plan_property_removal).
        let malformed = with_malformed_note_property(&baseline);
        std::fs::write(&sch, &malformed).unwrap();
        let (is_error, body) = call_raw(
            &handler,
            "edit_schematic_component",
            json!({
                "schematic": sch.to_str().unwrap(),
                "reference": "U1",
                "remove_fields": ["Note"]
            }),
        )
        .await;
        assert!(is_error, "{body}");
        assert_eq!(body["error"]["kind"], "malformed_file", "{body}");
        assert_eq!(body["error"]["path"], display_path(&sch), "{body}");
        assert!(
            body["message"]
                .as_str()
                .unwrap()
                .contains("No fields were updated on 'U1': Note:"),
            "{body}"
        );
        assert_eq!(std::fs::read_to_string(&sch).unwrap(), malformed);

        // Missing built-in Value property on unit 2 -> malformed_file.
        let unit2_value = "\t\t(property \"Value\" \"ECC83\"\n\t\t\t(at 162.56 116.84 0)\n\t\t\t(effects\n\t\t\t\t(font\n\t\t\t\t\t(size 1.27 1.27)\n\t\t\t\t)\n\t\t\t)\n\t\t)\n";
        assert!(baseline.contains(unit2_value));
        let missing_value = baseline.replacen(unit2_value, "", 1);
        std::fs::write(&sch, &missing_value).unwrap();
        let (is_error, body) = call_raw(
            &handler,
            "edit_schematic_component",
            json!({
                "schematic": sch.to_str().unwrap(),
                "reference": "U1",
                "value": "12AX7"
            }),
        )
        .await;
        assert!(is_error, "{body}");
        assert_eq!(body["error"]["kind"], "malformed_file", "{body}");
        assert_eq!(body["error"]["path"], display_path(&sch), "{body}");
        assert!(
            body["message"]
                .as_str()
                .unwrap()
                .contains("No fields were updated on 'U1': Value:"),
            "{body}"
        );
        assert_eq!(std::fs::read_to_string(&sch).unwrap(), missing_value);

        // Combined malformed built-in field + invalid custom field: malformed_file takes precedence.
        let (is_error, body) = call_raw(
            &handler,
            "edit_schematic_component",
            json!({
                "schematic": sch.to_str().unwrap(),
                "reference": "U1",
                "value": "12AX7",
                "fields": { "Note": 42 }
            }),
        )
        .await;
        assert!(is_error, "{body}");
        assert_eq!(body["error"]["kind"], "malformed_file", "{body}");
        assert_eq!(body["error"]["path"], display_path(&sch), "{body}");
        assert_eq!(std::fs::read_to_string(&sch).unwrap(), missing_value);
    }
}
