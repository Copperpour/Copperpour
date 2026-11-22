mod move_connected_tests {
    use super::super::*;

    #[tokio::test]
    async fn move_connected_refuses_instead_of_faking_success() {
        let result = handle_move_connected(
            &serde_json::json!({
                "schematic": "unused.kicad_sch",
                "reference": "R1", "x": 10.0, "y": 10.0
            }),
            &crate::tools::ToolContext::new(
                crate::tools::ServerConfig {
                    kicad_cli: String::new(),
                    kicad_binary: String::new(),
                    ipc_address: String::new(),
                    project_dir: None,
                    jlcpcb_db_path: None,
                    auto_load_toolsets: false,
                    eager_toolsets: false,
                },
                std::sync::Arc::new(crate::router::ToolRouter::new()),
            ),
        )
        .await
        .unwrap();

        assert!(result.is_error);
        let crate::mcp::protocol::ToolContent::Text { text } = &result.content[0] else {
            panic!("expected text");
        };
        assert!(
            text.contains("move_schematic_component"),
            "must name the working alternative: {text}"
        );
    }
}

mod no_connect_carry_tests {
    use super::super::*;
    use crate::mcp::{error::extract_error_kind, protocol::ToolContent};
    use crate::tools::{sch_wiring::NoConnectCarry, ServerConfig};
    use std::sync::Arc;

    use super::super::component_ops_tests::component_delete_connectivity_tests::has_junction;

    const CARRY: &str = include_str!("../../../tests/fixtures/no_connect_carry_kicad10.kicad_sch");

    /// The marker KiCad wrote onto `R2` pin 1, and the one it wrote on nothing.
    const R2_MARKER: &str = "1a251b9a-30be-43fa-bf5f-04706ed82788";
    const DANGLING_MARKER: &str = "493fbe64-0015-4848-87bd-797fff8aa3fd";
    const STACKED_MARKER: &str = "3627efa6-e463-4121-ac35-b9056319db36";

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

    fn fixture() -> (tempfile::TempDir, std::path::PathBuf) {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("carry.kicad_sch");
        std::fs::write(&path, CARRY).unwrap();
        (directory, path)
    }

    /// The tool's JSON body, whether it succeeded or refused — the caller
    /// asserts which it expected.
    fn body(result: &CallToolResult) -> serde_json::Value {
        let ToolContent::Text { text } = &result.content[0] else {
            panic!("expected text result");
        };
        serde_json::from_str(text).unwrap()
    }

    /// Where the file now has the marker with this UUID.
    pub(super) fn marker_at(content: &str, uuid: &str) -> (f64, f64) {
        let tree = parse_sexp(content).expect("the written sheet parses");
        tree.find_all("no_connect")
            .into_iter()
            .find(|node| node.find_str("uuid") == Some(uuid))
            .and_then(copperpour_sexp::schematic::parse_at)
            .map(|(x, y, _)| (x, y))
            .unwrap_or_else(|| panic!("no-connect {uuid} is missing from the written sheet"))
    }

    #[tokio::test]
    async fn a_move_carries_the_marker_and_leaves_the_protected_pin_off_the_wire() {
        let (_directory, path) = fixture();
        let result = handle_move_schematic_component(
            &json!({ "schematic": path, "reference": "R2", "x": 154.94, "y": 163.83 }),
            &context(),
        )
        .await
        .unwrap();
        assert!(!result.is_error, "{result:?}");
        let response = body(&result);

        assert_eq!(response["no_connects_moved_count"], 1, "{response}");
        let moved = &response["no_connects_moved"][0];
        assert_eq!(moved["uuid"], R2_MARKER, "{response}");
        assert_eq!(
            moved["from"],
            json!({ "x": 158.75, "y": 156.21 }),
            "{response}"
        );
        assert_eq!(
            moved["to"],
            json!({ "x": 154.94, "y": 160.02 }),
            "{response}"
        );
        // The pin lands mid-span on the NETB wire. KiCad reaches it only
        // through a junction dot, so the carried marker must stop the dot
        // being written — otherwise the pin joins /NETB (#626).
        assert_eq!(response["junctions_added_count"], 0, "{response}");

        let committed = std::fs::read_to_string(&path).unwrap();
        assert_eq!(marker_at(&committed, R2_MARKER), (154.94, 160.02));
        assert!(!has_junction(&committed, 154.94, 160.02));
    }

    #[tokio::test]
    async fn a_region_move_carries_the_marker_in_the_same_atomic_write() {
        let (_directory, path) = fixture();
        let result = handle_move_region(
            &json!({
                "schematic": path,
                "x1": 158.0, "y1": 159.0,
                "x2": 159.0, "y2": 161.0,
                "dx": -3.81, "dy": 3.81
            }),
            &context(),
        )
        .await
        .unwrap();
        assert!(!result.is_error, "{result:?}");
        let response = body(&result);

        assert_eq!(response["moved"], json!(["R2"]), "{response}");
        assert_eq!(response["no_connects_moved_count"], 1, "{response}");
        assert_eq!(
            response["no_connects_moved"][0]["uuid"], R2_MARKER,
            "{response}"
        );
        assert_eq!(
            response["no_connects_moved"][0]["to"],
            json!({ "x": 154.94, "y": 160.02 }),
            "{response}"
        );
        assert_eq!(response["junctions_added_count"], 0, "{response}");
        let committed = std::fs::read_to_string(&path).unwrap();
        assert_eq!(marker_at(&committed, R2_MARKER), (154.94, 160.02));
        assert!(!has_junction(&committed, 154.94, 160.02));
    }

    #[tokio::test]
    async fn a_turn_carries_the_marker_the_same_way_a_move_does() {
        let (_directory, path) = fixture();
        let result = handle_rotate_schematic_component(
            &json!({ "schematic": path, "reference": "R2", "rotation": 90 }),
            &context(),
        )
        .await
        .unwrap();
        assert!(!result.is_error, "{result:?}");
        let response = body(&result);

        assert_eq!(response["no_connects_moved_count"], 1, "{response}");
        let moved = &response["no_connects_moved"][0];
        assert_eq!(moved["uuid"], R2_MARKER, "{response}");
        assert_eq!(
            moved["to"],
            json!({ "x": 154.94, "y": 160.02 }),
            "{response}"
        );
        assert_eq!(response["junctions_added_count"], 0, "{response}");
        assert_eq!(response["junctions_pruned_count"], 0, "{response}");

        let committed = std::fs::read_to_string(&path).unwrap();
        assert_eq!(marker_at(&committed, R2_MARKER), (154.94, 160.02));
        assert!(!has_junction(&committed, 154.94, 160.02));
    }

    #[tokio::test]
    async fn a_placement_change_carrying_nothing_leaves_every_marker_alone() {
        let (_directory, path) = fixture();
        let result = handle_move_schematic_component(
            &json!({ "schematic": path, "reference": "R4", "x": 139.7, "y": 175.26 }),
            &context(),
        )
        .await
        .unwrap();
        assert!(!result.is_error, "{result:?}");
        let response = body(&result);

        assert_eq!(response["no_connects_moved_count"], 0, "{response}");
        assert_eq!(response["no_connects_moved"], json!([]), "{response}");
        let committed = std::fs::read_to_string(&path).unwrap();
        assert_eq!(marker_at(&committed, R2_MARKER), (158.75, 156.21));
        assert_eq!(marker_at(&committed, DANGLING_MARKER), (180.34, 120.65));
        assert_eq!(marker_at(&committed, STACKED_MARKER), (114.3, 105.41));
    }

    #[tokio::test]
    async fn a_marker_under_two_pins_refuses_the_move_and_writes_nothing() {
        for (reference, x, y) in [("R3", 101.6, 101.6), ("R5", 101.6, 109.22)] {
            let (_directory, path) = fixture();
            let result = handle_move_schematic_component(
                &json!({ "schematic": path, "reference": reference, "x": x, "y": y }),
                &context(),
            )
            .await
            .unwrap();
            assert!(result.is_error, "{result:?}");
            let response = body(&result);

            assert_eq!(
                extract_error_kind(&result),
                Some("ambiguous_target".to_string()),
                "{response}"
            );
            assert_eq!(
                response["error"]["target"],
                format!("no-connect {STACKED_MARKER} at (114.3, 105.41)"),
                "{response}"
            );
            let candidates = response["error"]["candidates"]
                .as_array()
                .expect("candidates are listed")
                .iter()
                .map(|value| value.as_str().unwrap().to_owned())
                .collect::<Vec<_>>();
            assert_eq!(candidates.len(), 2, "{response}");
            assert!(
                candidates
                    .iter()
                    .any(|c| c.contains("R3") && c.contains("pin 2")),
                "{response}"
            );
            assert!(
                candidates
                    .iter()
                    .any(|c| c.contains("R5") && c.contains("pin 1")),
                "{response}"
            );

            assert_eq!(
                std::fs::read_to_string(&path).unwrap(),
                CARRY,
                "refusing {reference} must leave the file byte-identical"
            );
        }
    }

    #[tokio::test]
    async fn an_ambiguous_region_carry_refuses_before_writing() {
        let (_directory, path) = fixture();
        let before = std::fs::read(&path).unwrap();
        let result = handle_move_region(
            &json!({
                "schematic": path,
                "x1": 113.0, "y1": 100.0,
                "x2": 115.0, "y2": 103.0,
                "dx": -12.7, "dy": 0.0
            }),
            &context(),
        )
        .await
        .unwrap();

        assert!(result.is_error, "{result:?}");
        assert_eq!(
            extract_error_kind(&result),
            Some("ambiguous_target".to_string()),
            "{:?}",
            body(&result)
        );
        assert_eq!(std::fs::read(&path).unwrap(), before);
    }

    #[test]
    fn a_marker_the_written_file_does_not_show_makes_the_outcome_uncertain() {
        // The report is read back from the file, never from the plan: a marker
        // the write did not land is the difference between "your pin is still
        // protected" and "your pin may be on a net".
        let (_directory, path) = fixture();
        let landed = crate::tools::sch_wiring::NoConnectCarry {
            uuid: R2_MARKER.to_owned(),
            from: (158.75, 156.21),
            to: (158.75, 156.21),
        };
        let reported = observed_no_connect_moves(
            &path,
            "move_schematic_component",
            std::slice::from_ref(&landed),
        )
        .unwrap()
        .expect("the fixture does have that marker at that point");
        assert_eq!(reported[0]["to"], json!({ "x": 158.75, "y": 156.21 }));

        let elsewhere = NoConnectCarry {
            to: (1.0, 1.0),
            ..landed
        };
        let result = observed_no_connect_moves(&path, "move_schematic_component", &[elsewhere])
            .unwrap()
            .expect_err("the marker is not at (1, 1), so the outcome is not proven");
        assert_eq!(
            extract_error_kind(&result),
            Some("mutation_outcome_uncertain".to_string()),
            "{result:?}"
        );
    }

    #[tokio::test]
    async fn a_marker_with_an_empty_uuid_refuses_on_the_typed_path_too() {
        // The typed model reads a missing UUID as "" and writes it back as
        // (uuid ""), so a marker that the S-expression path refuses must not
        // slip through here keyed on the empty string — two of them would
        // share that key and the carry would follow the wrong one.
        let (_directory, path) = fixture();
        let anonymous = CARRY.replace(&format!("\t\t(uuid \"{R2_MARKER}\")\n"), "");
        assert!(!anonymous.contains(R2_MARKER));
        std::fs::write(&path, &anonymous).unwrap();

        let result = handle_move_schematic_component(
            &json!({ "schematic": path, "reference": "R2", "x": 154.94, "y": 163.83 }),
            &context(),
        )
        .await
        .unwrap();

        assert!(result.is_error, "{result:?}");
        assert_eq!(
            extract_error_kind(&result),
            Some("stale_target".to_string()),
            "{:?}",
            body(&result)
        );
        assert_eq!(std::fs::read_to_string(&path).unwrap(), anonymous);
    }

    #[tokio::test]
    async fn a_carry_that_would_stack_two_markers_refuses_and_writes_nothing() {
        // R2 pin 1 lands exactly on the marker that is dangling on purpose.
        // Two markers on one point belong to neither pin, so this refuses
        // rather than writing a sheet no caller can read back.
        let (_directory, path) = fixture();
        let result = handle_move_schematic_component(
            &json!({ "schematic": path, "reference": "R2", "x": 180.34, "y": 124.46 }),
            &context(),
        )
        .await
        .unwrap();
        assert!(result.is_error, "{result:?}");
        let response = body(&result);

        assert_eq!(
            extract_error_kind(&result),
            Some("ambiguous_target".to_string()),
            "{response}"
        );
        let candidates = response["error"]["candidates"].to_string();
        assert!(candidates.contains(R2_MARKER), "{response}");
        assert!(candidates.contains(DANGLING_MARKER), "{response}");
        assert_eq!(std::fs::read_to_string(&path).unwrap(), CARRY);
    }
}

/// A field's `(at …)` is an absolute sheet coordinate, not an offset from the
/// body, so a turn owes each field the same rotation it gives the symbol
/// (#612).
///
/// Every case runs on `rotate_fields_kicad10.kicad_sch`, which is KiCad
/// 10.0.6's own serialization — see its README for provenance and for the
/// `kicad-cli sch export svg` anchors these expectations come from.
///
/// The sheet is built in twins: a symbol placed unrotated, and the same
/// symbol placed at 90° outright `TWIN_OFFSET_MM` below it. Turning the first
/// must land its fields exactly on the second's, less that offset — an
/// expectation KiCad wrote into the fixture, not one this crate computed.
mod rotate_field_text_tests {
    use super::super::*;
    use crate::mcp::protocol::ToolContent;
    use crate::tools::ServerConfig;
    use std::sync::Arc;

    const SHEET: &str = include_str!("../../../tests/fixtures/rotate_fields_kicad10.kicad_sch");

    /// How far below its unrotated twin each pre-rotated symbol sits.
    const TWIN_OFFSET_MM: f64 = 25.4;

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

    fn fixture() -> (tempfile::TempDir, std::path::PathBuf) {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("rotate-fields.kicad_sch");
        std::fs::write(&path, SHEET).unwrap();
        (directory, path)
    }

    fn body(result: &CallToolResult) -> serde_json::Value {
        assert!(!result.is_error, "{result:?}");
        let ToolContent::Text { text } = &result.content[0] else {
            panic!("expected text result");
        };
        serde_json::from_str(text).unwrap()
    }

    async fn rotate(path: &std::path::Path, reference: &str, rotation: f64) -> CallToolResult {
        handle_rotate_schematic_component(
            &json!({ "schematic": path, "reference": reference, "rotation": rotation }),
            &context(),
        )
        .await
        .unwrap()
    }

    /// Sheet position and stored angle of a symbol's Reference and Value text.
    /// The angle comes back because the turn must leave it alone: KiCad adds
    /// the symbol's rotation when it draws, so turning the stored angle here
    /// would draw the text at twice the angle.
    pub(super) fn field_positions(source: &str, reference: &str) -> [(f64, f64, f64); 2] {
        let schematic =
            cse::Schematic::from_source(std::path::Path::new("sheet"), source.to_string())
                .expect("fixture parses");
        let symbol = schematic
            .symbols
            .iter()
            .find(|symbol| symbol.reference() == Some(reference))
            .expect("placed symbol");
        ["Reference", "Value"].map(|name| {
            let at = symbol
                .properties
                .iter()
                .find(|property| property.name == name)
                .expect("field")
                .sub_nodes
                .iter()
                .find_map(cse::types::At::from_sexp)
                .expect("field position");
            (at.x, at.y, at.rotation.unwrap_or(0.0))
        })
    }

    /// The fields of `twin`, lifted by the offset that separates the two
    /// placements — where `reference`'s fields have to land once it turns.
    fn twin_target(reference_twin: &str) -> [(f64, f64, f64); 2] {
        field_positions(SHEET, reference_twin).map(|(x, y, angle)| (x, y - TWIN_OFFSET_MM, angle))
    }

    /// KiCad writes these coordinates to four decimals. Compare on that scale,
    /// so the last bit of the twin subtraction does not decide a geometric
    /// claim — 50.8 and 50.800000000000004 are the same point on a sheet.
    pub(super) fn to_nanometres(fields: [(f64, f64, f64); 2]) -> [(i64, i64, i64); 2] {
        fields.map(|(x, y, angle)| {
            (
                (x * 1_000_000.0).round() as i64,
                (y * 1_000_000.0).round() as i64,
                (angle * 1_000_000.0).round() as i64,
            )
        })
    }

    /// Turning a placed symbol used to leave its field text where the body had
    /// been. A `Device:LED` turned to 90° kept its Reference 2.54mm above the
    /// origin — the middle of the now-vertical body, under the wire into its
    /// anode.
    ///
    /// D2 is the same LED, placed at 90° outright, so the fixture carries the
    /// answer. The literal is stated too, so the two agreeing on a wrong point
    /// would still fail.
    #[tokio::test]
    async fn turning_a_symbol_carries_its_field_text_around() {
        let (_directory, path) = fixture();

        assert!(!rotate(&path, "D1", 90.0).await.is_error);

        let turned = field_positions(&std::fs::read_to_string(&path).unwrap(), "D1");
        // 2.54mm above the origin becomes 2.54mm to its left, clearing the
        // vertical body instead of lying along it.
        assert_eq!(turned, [(99.06, 50.8, 0.0), (104.14, 50.8, 0.0)]);
        assert_eq!(to_nanometres(turned), to_nanometres(twin_target("D2")));
    }

    /// A placement rotates before it mirrors, and a reflection reverses the
    /// sense of any rotation conjugated by it, so a mirrored body turns its
    /// fields the other way. The case needs an anchor off both axes — a
    /// `Device:LED`'s anchors are symmetric about the origin, so the reversal
    /// only swaps the two fields and lands on the same pair of points.
    #[tokio::test]
    async fn turning_a_mirrored_symbol_turns_its_fields_the_other_way() {
        let (_directory, path) = fixture();

        assert!(!rotate(&path, "U1", 90.0).await.is_error);

        let turned = field_positions(&std::fs::read_to_string(&path).unwrap(), "U1");
        assert_eq!(turned[0], (133.985, 45.72, 0.0));
        assert_eq!(to_nanometres(turned), to_nanometres(twin_target("U2")));
    }

    /// The same part, same turn, no mirror: the Reference lands on the other
    /// side of the body. This is the control that makes the case above bite —
    /// ignore the reflection and the mirrored symbol lands here instead.
    #[tokio::test]
    async fn turning_an_unmirrored_symbol_turns_its_fields_the_plain_way() {
        let (_directory, path) = fixture();

        assert!(!rotate(&path, "U3", 90.0).await.is_error);

        let turned = field_positions(&std::fs::read_to_string(&path).unwrap(), "U3");
        assert_eq!(turned[0], (172.085, 55.88, 0.0));
        assert_eq!(to_nanometres(turned), to_nanometres(twin_target("U4")));
        assert_ne!(
            turned[0].1 - 50.8,
            twin_target("U2")[0].1 - 50.8,
            "the mirrored and unmirrored turns must not agree, or neither case proves the parity"
        );
    }

    /// The turn carries a field the caller positioned by hand as readily as
    /// one still on its library anchor: the offset turns, it is not reset.
    /// `reset_schematic_field_positions` is the tool that discards a manual
    /// offset, and it must stay the only one. D3's Reference was dragged to
    /// 10mm right and 10mm up of its origin before the resave.
    #[tokio::test]
    async fn turning_a_symbol_carries_a_hand_placed_field_offset_round() {
        let (_directory, path) = fixture();
        assert_eq!(field_positions(SHEET, "D3")[0], (111.6, 91.6, 0.0));

        assert!(!rotate(&path, "D3", 90.0).await.is_error);

        // (10, -10) from the origin, turned a quarter: (-10, -10).
        assert_eq!(
            field_positions(&std::fs::read_to_string(&path).unwrap(), "D3")[0],
            (91.6, 91.6, 0.0)
        );
    }

    /// A full turn must land back where it started, with no drift accumulated
    /// through the trigonometry — byte for byte, so a field angle nudged by a
    /// quarter of a degree would show up too.
    #[tokio::test]
    async fn turning_a_symbol_the_whole_way_round_restores_the_sheet() {
        let (_directory, path) = fixture();
        // The first write reserializes KiCad's formatting into Copperpour's, so
        // the baseline is the sheet after a no-op turn, not the fixture bytes.
        assert!(!rotate(&path, "D1", 0.0).await.is_error);
        let baseline = std::fs::read_to_string(&path).unwrap();

        for rotation in [90.0, 180.0, 270.0, 0.0] {
            assert!(!rotate(&path, "D1", rotation).await.is_error);
        }

        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            baseline,
            "a full turn must land back where it started, byte for byte"
        );
    }

    /// One turn now does three things in the same mutation: it carries the
    /// field text (#612), it reconciles the junctions the pins leave and reach
    /// (#615), and it moves the no-connect flag with the pin it protects
    /// (#626). R1 stands on the NETJ wire with a marker on pin 2; turned, pin
    /// 1 lands mid-span and pin 2 swings clear of the wire's end.
    ///
    /// `kicad-cli sch export netlist` is the oracle for the connectivity half:
    /// `/NETJ` carries `R2.1` before the turn and `R2.1, R1.1` after it.
    #[tokio::test]
    async fn one_turn_carries_fields_junctions_and_no_connects_together() {
        let (_directory, path) = fixture();
        // R1's Reference sits 2.032mm right of the origin at a stored 90°.
        assert_eq!(field_positions(SHEET, "R1")[0], (103.632, 139.7, 90.0));

        let response = body(&rotate(&path, "R1", 90.0).await);

        assert_eq!(response["junctions_added_count"], 1);
        assert_eq!(response["junctions_pruned_count"], 0);
        assert_eq!(response["no_connects_moved_count"], 1);
        // 2.032mm to the right becomes 2.032mm above, and the stored angle is
        // untouched — the field carry did not skip a handler that reconciles.
        assert_eq!(
            field_positions(&std::fs::read_to_string(&path).unwrap(), "R1")[0],
            (101.6, 137.668, 90.0)
        );
    }
}

/// Reflecting a placed symbol owes its field text the same reflection (#613):
/// a field's `(at …)` is an absolute sheet coordinate, so writing the token
/// alone leaves Reference and Value on the unreflected side of the body.
///
/// No tool mirrors a placed symbol yet (#450), so these cases drive
/// `Symbol::set_mirror` directly, on `mirror_fields_kicad10.kicad_sch` —
/// KiCad 10.0.6's own serialization, built in twins like
/// `rotate_fields_kicad10`: a symbol, and the same symbol placed with the
/// target reflection `TWIN_OFFSET_MM` below it. See its README for provenance
/// and the `kicad-cli sch export svg` anchors.
mod mirror_field_text_tests {
    use super::super::*;
    use super::rotate_field_text_tests::{field_positions, to_nanometres};

    const SHEET: &str = include_str!("../../../tests/fixtures/mirror_fields_kicad10.kicad_sch");
    /// Drawn in KiCad, not built by Copperpour: its mirrored connectors carry
    /// fields on the reflected library anchors, as eeschema wrote them.
    const DEMO: &str =
        include_str!("../../../tests/fixtures/project_ownership/complex_hierarchy.kicad_sch");

    /// How far below each symbol its reflected twin sits.
    const TWIN_OFFSET_MM: f64 = 25.4;

    /// Reflect `reference` to `mirror` and read its fields back from the
    /// written sheet.
    fn mirrored(source: &str, reference: &str, mirror: Option<&str>) -> [(f64, f64, f64); 2] {
        let mut schematic =
            cse::Schematic::from_source(std::path::Path::new("sheet"), source.to_string())
                .expect("fixture parses");
        schematic
            .symbols
            .iter_mut()
            .find(|symbol| symbol.reference() == Some(reference))
            .expect("placed symbol")
            .set_mirror(mirror);
        field_positions(&schematic.to_source(), reference)
    }

    /// `twin`'s fields, lifted by the offset between the two placements.
    fn twin(reference: &str) -> [(i64, i64, i64); 2] {
        to_nanometres(
            field_positions(SHEET, reference).map(|(x, y, angle)| (x, y - TWIN_OFFSET_MM, angle)),
        )
    }

    /// A `Device:LED` anchors Reference above the origin and Value below;
    /// `(mirror x)` negates screen-Y, so the two trade sides — landing on D2,
    /// the same LED placed reflected. The literal is stated too, so the
    /// fixture and the code agreeing on a wrong point would still fail.
    #[test]
    fn mirroring_x_reflects_field_text_across_the_body() {
        let reflected = mirrored(SHEET, "D1", Some("x"));
        assert_eq!(reflected, [(101.6, 53.34, 0.0), (101.6, 48.26, 0.0)]);
        assert_eq!(to_nanometres(reflected), twin("D2"));
    }

    /// `(mirror y)` negates screen-X. U1's Reference sits off both axes, so
    /// only the X offset may change.
    #[test]
    fn mirroring_y_reflects_field_text_left_to_right() {
        let reflected = mirrored(SHEET, "U1", Some("y"));
        assert_eq!(reflected[0], (144.78, 45.085, 0.0));
        assert_eq!(to_nanometres(reflected), twin("U2"));
    }

    /// A placement rotates first and mirrors second, so on a turned body
    /// `(mirror x)` still negates *screen*-Y: U3 at 90° keeps its Reference's
    /// X offset and flips its Y. U4 was placed by the same transform, so this
    /// shows reflecting agrees with placing reflected; the order itself is
    /// pinned by the KiCad-drawn L2 case below.
    #[test]
    fn mirroring_a_turned_symbol_reflects_in_sheet_axes() {
        let reflected = mirrored(SHEET, "U3", Some("x"));
        assert_eq!(reflected[0], (172.085, 45.72, 0.0));
        assert_eq!(to_nanometres(reflected), twin("U4"));
    }

    /// Swapping one axis for the other is two reflections — both offsets flip.
    #[test]
    fn swapping_the_mirror_axis_flips_both_offsets() {
        let reflected = mirrored(SHEET, "U5", Some("y"));
        assert_eq!(reflected[0], (220.98, 45.085, 0.0));
        assert_eq!(to_nanometres(reflected), twin("U6"));
    }

    /// Clearing the mirror is a reflection too: U7 comes back to U8's fields.
    #[test]
    fn clearing_the_mirror_reflects_field_text_back() {
        let reflected = mirrored(SHEET, "U7", None);
        assert_eq!(reflected[0], (134.62, 95.885, 0.0));
        assert_eq!(to_nanometres(reflected), twin("U8"));
    }

    /// KiCad drew these, not Copperpour. P102 is a `CONN_2` placed `(mirror y)`
    /// whose library anchors put Reference at local x = −1.27 and Value at
    /// +1.27; eeschema wrote them reflected, at 43.18 and 40.64 about the
    /// origin at 41.91. Clearing the mirror must put each on its library side.
    #[test]
    fn clearing_a_kicad_drawn_mirror_restores_the_library_anchors() {
        assert_eq!(
            field_positions(DEMO, "P102"),
            [(43.18, 66.04, 90.0), (40.64, 66.04, 90.0)]
        );

        assert_eq!(
            mirrored(DEMO, "P102", None),
            [(40.64, 66.04, 90.0), (43.18, 66.04, 90.0)]
        );
    }

    /// The twins above take their answer from Copperpour's own placement
    /// transform, so they cannot tell whether that transform mirrors before or
    /// after it turns. KiCad can. L2 is an `INDUCTOR` at 270° with
    /// `(mirror x)`, whose library anchors its Value at (2.54, 0); eeschema
    /// wrote it 2.54mm *above* the origin, where rotate-then-mirror puts it
    /// and mirror-then-rotate does not. Unreflected, the turn alone puts it
    /// 2.54mm below. A reflection taken in the symbol's own frame would move X
    /// instead and leave it above.
    ///
    /// The sheet is KiCad's `video` demo, read from the local install because
    /// its CC BY-SA licence keeps it out of the repo. The test skips when no
    /// install is found, or when the installed sheet no longer places L2 so.
    #[test]
    fn clearing_a_kicad_drawn_mirror_on_a_turned_symbol_reflects_in_sheet_axes() {
        let Some(video) = installed_demo("video/modul.kicad_sch") else {
            eprintln!("SKIP: no KiCad demos found (set KICAD_DEMOS to enable)");
            return;
        };
        let schematic = cse::Schematic::from_source(std::path::Path::new("sheet"), video.clone())
            .expect("demo parses");
        let Some(l2) = schematic
            .symbols
            .iter()
            .find(|symbol| symbol.reference() == Some("L2"))
            .filter(|l2| l2.at.rotation == Some(270.0) && l2.mirror.as_deref() == Some("x"))
        else {
            eprintln!("SKIP: video/modul.kicad_sch no longer places L2 at 270° with (mirror x)");
            return;
        };
        // Offsets from L2's origin, so a demo that moves L2 still checks.
        let (x, y) = (l2.at.x, l2.at.y);
        let value_offset = |fields: [(f64, f64, f64); 2]| {
            to_nanometres(fields.map(|(fx, fy, angle)| (fx - x, fy - y, angle)))[1]
        };
        let expected = |dy: f64| to_nanometres([(0.0, dy, 90.0); 2])[1];

        assert_eq!(value_offset(field_positions(&video, "L2")), expected(-2.54));

        assert_eq!(value_offset(mirrored(&video, "L2", None)), expected(2.54));
    }

    /// A KiCad demo sheet under `KICAD_DEMOS`, else under the standard install
    /// paths — the lookup `tests/conformance_test.rs` uses.
    fn installed_demo(sheet: &str) -> Option<String> {
        let override_dir = std::env::var_os("KICAD_DEMOS").map(std::path::PathBuf::from);
        let candidates: &[&str] = if cfg!(target_os = "windows") {
            &[
                r"C:\KiCad\10.0\share\kicad\demos",
                r"C:\Program Files\KiCad\10.0\share\kicad\demos",
            ]
        } else if cfg!(target_os = "macos") {
            &["/Applications/KiCad/KiCad.app/Contents/SharedSupport/demos"]
        } else {
            &["/usr/share/kicad/demos", "/usr/local/share/kicad/demos"]
        };
        override_dir
            .into_iter()
            .chain(candidates.iter().map(std::path::PathBuf::from))
            .find(|dir| dir.exists())
            .and_then(|dir| std::fs::read_to_string(dir.join(sheet)).ok())
    }

    /// A field the caller moved by hand keeps its offset, reflected with the
    /// body rather than reset. D3's Reference was dragged 10mm right of and
    /// 10mm above its origin before the resave.
    #[test]
    fn mirroring_carries_a_hand_placed_field_offset_across() {
        assert_eq!(field_positions(SHEET, "D3")[0], (111.6, 91.6, 0.0));

        assert_eq!(mirrored(SHEET, "D3", Some("x"))[0], (111.6, 111.6, 0.0));
    }

    /// Reflecting and reflecting back restores the sheet byte for byte.
    #[test]
    fn mirroring_there_and_back_restores_the_sheet() {
        let parse = || {
            cse::Schematic::from_source(std::path::Path::new("sheet"), SHEET.to_string())
                .expect("fixture parses")
        };
        let baseline = parse().to_source();
        let mut schematic = parse();
        for symbol in &mut schematic.symbols {
            let mirror = symbol.mirror.clone();
            for step in [Some("x"), Some("y"), None] {
                symbol.set_mirror(step);
            }
            symbol.set_mirror(mirror.as_deref());
        }
        assert_eq!(schematic.to_source(), baseline);
    }
}

/// A turn relocates pin endpoints exactly as a move does, so it owes the sheet
/// the same junction reconciliation (#615).
///
/// Every case runs on `rotate_junctions_kicad10.kicad_sch`, which is KiCad
/// 10.0.6's own serialization — see its README for provenance and for the
/// netlist KiCad answers with, which is where these expectations come from.
mod rotate_junction_reconciliation_tests {
    use super::super::*;
    use crate::mcp::{error::extract_error_kind, protocol::ToolContent};
    use crate::tools::ServerConfig;
    use std::sync::Arc;

    const SHEET: &str = include_str!("../../../tests/fixtures/rotate_junctions_kicad10.kicad_sch");

    /// The dot `move_schematic_component` left under R1 pin 1, mid-span on the
    /// NETA wire.
    const STRANDED_DOT: (f64, f64) = (127.0, 97.79);
    /// Where R2 pin 1 lands once R2 is turned to 90°: mid-span on the NETB wire.
    const LANDING_POINT: (f64, f64) = (154.94, 160.02);
    /// The wire T in the corner of the sheet, which no turn here goes near.
    const UNRELATED_DOT: (f64, f64) = (190.5, 88.9);

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
        let path = directory.path().join("rotate.kicad_sch");
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

    fn has_junction(content: &str, (x, y): (f64, f64)) -> bool {
        let tree = parse_sexp(content).unwrap();
        copperpour_sexp::schematic::extract_junctions(&tree)
            .iter()
            .any(|&(jx, jy)| copperpour_sexp::geometry::points_coincident(x, y, jx, jy, 0.01))
    }

    async fn rotate(path: &std::path::Path, reference: &str, rotation: f64) -> CallToolResult {
        handle_rotate_schematic_component(
            &json!({ "schematic": path, "reference": reference, "rotation": rotation }),
            &context(),
        )
        .await
        .unwrap()
    }

    #[tokio::test]
    async fn turning_a_pin_off_a_wire_prunes_the_dot_it_strands() {
        let (_directory, path) = fixture(SHEET);
        assert!(has_junction(SHEET, STRANDED_DOT));

        let response = body(&rotate(&path, "R1", 90.0).await);

        assert_eq!(response["junctions_pruned_count"], 1);
        assert_eq!(response["junctions_added_count"], 0);
        let committed = std::fs::read_to_string(&path).unwrap();
        assert!(
            !has_junction(&committed, STRANDED_DOT),
            "the dot has only the wire passing through it now"
        );
        assert!(
            has_junction(&committed, UNRELATED_DOT),
            "the wire T is justified independently of R1"
        );
    }

    #[tokio::test]
    async fn turning_a_pin_onto_a_wire_adds_the_junction_it_needs() {
        let (_directory, path) = fixture(SHEET);
        assert!(!has_junction(SHEET, LANDING_POINT));

        let response = body(&rotate(&path, "R2", 90.0).await);

        assert_eq!(response["junctions_added_count"], 1);
        assert_eq!(response["junctions_pruned_count"], 0);
        let committed = std::fs::read_to_string(&path).unwrap();
        assert!(
            has_junction(&committed, LANDING_POINT),
            "without the dot KiCad leaves the pin on unconnected-(R2-Pad1)"
        );
        assert!(
            has_junction(&committed, STRANDED_DOT),
            "R1's dot is nowhere near the turn and stays as it was"
        );
    }

    #[tokio::test]
    async fn a_no_connect_at_the_landing_point_keeps_the_dot_away() {
        // The caller has said this pin stays unconnected; a turn must not
        // overrule that by wiring it into the net it lands on.
        let closing = SHEET.rfind("\n)").unwrap();
        let marker = format!(
            "\t(no_connect\n\t\t(at {} {})\n\t\t(uuid \"no-connect-on-the-landing-point\")\n\t)\n",
            LANDING_POINT.0, LANDING_POINT.1
        );
        let original = format!("{}{marker}{}", &SHEET[..closing + 1], &SHEET[closing + 1..]);
        let (_directory, path) = fixture(&original);

        let response = body(&rotate(&path, "R2", 90.0).await);

        assert_eq!(response["junctions_added_count"], 0);
        let committed = std::fs::read_to_string(&path).unwrap();
        assert!(!has_junction(&committed, LANDING_POINT));
        assert!(committed.contains("no-connect-on-the-landing-point"));
    }

    #[tokio::test]
    async fn a_pin_leaving_a_wire_end_neither_adds_nor_prunes() {
        // R0 pin 1 turns off the *end* of the NETA wire, not its interior. A
        // wire end needs no dot, so there is none to prune and none to add —
        // and the counts are still reported rather than omitted.
        let (_directory, path) = fixture(SHEET);

        let response = body(&rotate(&path, "R0", 90.0).await);

        assert_eq!(response["junctions_added_count"], 0);
        assert_eq!(response["junctions_pruned_count"], 0);
        let committed = std::fs::read_to_string(&path).unwrap();
        assert!(has_junction(&committed, STRANDED_DOT));
        assert!(has_junction(&committed, UNRELATED_DOT));
    }

    #[tokio::test]
    async fn a_refused_turn_writes_nothing() {
        let (_directory, path) = fixture(SHEET);

        let result = rotate(&path, "R404", 90.0).await;

        assert_eq!(extract_error_kind(&result).as_deref(), Some("stale_target"));
        assert_eq!(std::fs::read_to_string(&path).unwrap(), SHEET);
    }
}

/// Both pin-location tools report each placement's mirror beside its pins
/// (#855). `pin_location_mirror_kicad10.kicad_sch` places two LM358s whose
/// units disagree, and `ORACLE` is KiCad's answer; see its README.
mod pin_location_mirror_tests {
    use super::super::*;
    use crate::mcp::handler::McpHandler;
    use crate::tools::ServerConfig;

    const SHEET: &str =
        include_str!("../../../tests/fixtures/pin_location_mirror_kicad10.kicad_sch");

    struct Placement {
        unit: u64,
        mirror_x: bool,
        mirror_y: bool,
        /// Pin number and position, from `kicad-cli sch erc --severity-all`.
        pins: [(&'static str, f64, f64); 3],
    }

    /// Each reference's units, lowest first: the mirrors eeschema wrote and
    /// the pins where KiCad puts them.
    const ORACLE: [(&str, [Placement; 2]); 2] = [
        (
            "U1",
            [
                Placement {
                    unit: 1,
                    mirror_x: false,
                    mirror_y: false,
                    pins: [
                        ("1", 109.22, 76.2),
                        ("2", 93.98, 78.74),
                        ("3", 93.98, 73.66),
                    ],
                },
                Placement {
                    unit: 2,
                    mirror_x: false,
                    mirror_y: true,
                    pins: [
                        ("5", 147.32, 73.66),
                        ("6", 147.32, 78.74),
                        ("7", 132.08, 76.2),
                    ],
                },
            ],
        ),
        (
            "U2",
            [
                Placement {
                    unit: 1,
                    mirror_x: true,
                    mirror_y: false,
                    pins: [
                        ("1", 109.22, 127.0),
                        ("2", 93.98, 124.46),
                        ("3", 93.98, 129.54),
                    ],
                },
                Placement {
                    unit: 2,
                    mirror_x: false,
                    mirror_y: false,
                    pins: [
                        ("5", 132.08, 124.46),
                        ("6", 132.08, 129.54),
                        ("7", 147.32, 127.0),
                    ],
                },
            ],
        ),
    ];

    /// The anchor reports unit 1's mirror, and each unit its own, beside pins
    /// where KiCad puts them.
    fn assert_matches_kicad(component: &serde_json::Value, expected: &[Placement; 2]) {
        assert_eq!(component["mirror_x"], expected[0].mirror_x, "{component}");
        assert_eq!(component["mirror_y"], expected[0].mirror_y, "{component}");
        let units = component["units"].as_array().unwrap();
        assert_eq!(units.len(), expected.len(), "{component}");
        for placement in expected {
            let unit = units
                .iter()
                .find(|unit| unit["unit"] == placement.unit)
                .unwrap_or_else(|| panic!("unit {}: {component}", placement.unit));
            assert_eq!(unit["mirror_x"], placement.mirror_x, "{unit}");
            assert_eq!(unit["mirror_y"], placement.mirror_y, "{unit}");
            let pins = unit["pins"].as_array().unwrap();
            assert_eq!(pins.len(), placement.pins.len(), "{unit}");
            for (number, x, y) in placement.pins {
                let pin = pins
                    .iter()
                    .find(|pin| pin["number"] == number)
                    .unwrap_or_else(|| panic!("pin {number}: {unit}"));
                assert!(
                    copperpour_sexp::geometry::points_coincident(
                        x,
                        y,
                        pin["x"].as_f64().unwrap(),
                        pin["y"].as_f64().unwrap(),
                        0.005,
                    ),
                    "pin {number}: KiCad has ({x}, {y}), got {pin}"
                );
            }
        }
    }

    async fn handler() -> McpHandler {
        McpHandler::new(ServerConfig {
            auto_load_toolsets: true,
            ..ServerConfig::default()
        })
        .await
        .unwrap()
    }

    /// Call `tool` through the served dispatch and parse its JSON answer.
    async fn served(
        handler: &McpHandler,
        tool: &str,
        arguments: serde_json::Value,
    ) -> serde_json::Value {
        let message = json!({"jsonrpc": "2.0", "id": 855, "method": "tools/call",
            "params": {"name": tool, "arguments": arguments}});
        let result = handler
            .handle_message(message)
            .await
            .unwrap()
            .result
            .unwrap();
        assert_ne!(result["isError"], true, "{result}");
        serde_json::from_str(result["content"][0]["text"].as_str().unwrap()).unwrap()
    }

    fn fixture() -> (tempfile::TempDir, String) {
        let directory = tempfile::tempdir().unwrap();
        let path = directory
            .path()
            .join("pin_location_mirror_kicad10.kicad_sch");
        std::fs::write(&path, SHEET).unwrap();
        (directory, path.display().to_string())
    }

    #[tokio::test]
    async fn get_schematic_pin_locations_reports_each_units_mirror() {
        let (_directory, schematic) = fixture();
        let handler = handler().await;
        for (reference, expected) in &ORACLE {
            let component = served(
                &handler,
                "get_schematic_pin_locations",
                json!({ "schematic": schematic, "reference": reference }),
            )
            .await;
            assert_eq!(component["reference"], *reference, "{component}");
            assert_matches_kicad(&component, expected);
        }
    }

    #[tokio::test]
    async fn batch_get_schematic_pin_locations_reports_each_units_mirror() {
        let (_directory, schematic) = fixture();
        let result = served(
            &handler().await,
            "batch_get_schematic_pin_locations",
            json!({ "schematic": schematic, "references": ["U1", "U2"] }),
        )
        .await;
        let components = result["components"].as_array().unwrap();
        assert_eq!(components.len(), ORACLE.len(), "{result}");
        for (component, (reference, expected)) in components.iter().zip(&ORACLE) {
            assert_eq!(component["reference"], *reference, "{component}");
            assert_matches_kicad(component, expected);
        }
    }
}

mod mirror_component_tests {
    use super::super::*;
    use super::rotate_field_text_tests::{field_positions, to_nanometres};
    use crate::mcp::protocol::ToolContent;
    use crate::tools::ServerConfig;
    use std::sync::Arc;

    /// KiCad-saved twins (#613): each symbol, and the same symbol placed with
    /// the target reflection 25.4mm below it.
    const SHEET: &str = include_str!("../../../tests/fixtures/mirror_fields_kicad10.kicad_sch");
    const TWIN_OFFSET_MM: f64 = 25.4;

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

    fn sheet(source: &str, name: &str) -> (tempfile::TempDir, std::path::PathBuf) {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join(name);
        std::fs::write(&path, source).unwrap();
        (directory, path)
    }

    /// A real eeschema save placing U1 as all three units of the dual triode
    /// `ECC83`: two triodes and the heater, none of them mirrored.
    fn ecc83() -> (tempfile::TempDir, std::path::PathBuf) {
        sheet(
            include_str!("../../../tests/fixtures/ecc83_multiunit.kicad_sch"),
            "ecc83.kicad_sch",
        )
    }

    async fn mirror(args: serde_json::Value) -> CallToolResult {
        handle_mirror_schematic_component(&args, &context())
            .await
            .unwrap()
    }

    fn body(result: &CallToolResult) -> serde_json::Value {
        assert!(!result.is_error, "mirror unexpectedly failed: {result:?}");
        let ToolContent::Text { text } = &result.content[0] else {
            panic!("expected text result");
        };
        serde_json::from_str(text).unwrap()
    }

    /// Each placed unit's mirror token, by unit number, read from the file.
    fn mirrors(path: &std::path::Path, reference: &str) -> Vec<(u32, Option<String>)> {
        let schematic = cse::Schematic::load(path).unwrap();
        let mut placed = schematic
            .symbols
            .iter()
            .filter(|symbol| symbol.reference() == Some(reference))
            .map(|symbol| (symbol.unit, symbol.mirror.clone()))
            .collect::<Vec<_>>();
        placed.sort();
        placed
    }

    /// Every pin endpoint of `reference`, in nanometres, lifted by `lift_mm`.
    fn pins(path: &std::path::Path, reference: &str, lift_mm: f64) -> Vec<(String, i64, i64)> {
        let (_, tree) = read_schematic(path).unwrap();
        let located = pin_locations_for_reference(&tree, reference).unwrap();
        let mut pins = located["pins"]
            .as_array()
            .unwrap()
            .iter()
            .map(|pin| {
                (
                    pin["number"].as_str().unwrap().to_owned(),
                    (pin["x"].as_f64().unwrap() * 1_000_000.0).round() as i64,
                    ((pin["y"].as_f64().unwrap() - lift_mm) * 1_000_000.0).round() as i64,
                )
            })
            .collect::<Vec<_>>();
        pins.sort();
        pins
    }

    /// Reflecting a placed symbol through the tool must leave it exactly as
    /// placing it reflected does: KiCad's own save of the twin is the answer.
    /// The cases cover both axes, a turned body (rotate first, mirror
    /// second), swapping one axis for the other, and clearing.
    #[tokio::test]
    async fn a_reflected_symbol_lands_on_its_twin() {
        for (reference, axis, twin) in [
            ("D1", "x", "D2"),
            ("U1", "y", "U2"),
            ("U3", "x", "U4"),
            ("U5", "y", "U6"),
            ("U7", "none", "U8"),
        ] {
            let (_directory, path) = sheet(SHEET, "mirror.kicad_sch");
            let (_original, original) = sheet(SHEET, "original.kicad_sch");
            let response = body(
                &mirror(json!({ "schematic": path, "reference": reference, "mirror": axis })).await,
            );
            assert_eq!(response["changed_unit_count"], 1, "{reference}: {response}");
            let (was_x, was_y) = match mirrors(&original, twin)[0].1.as_deref() {
                Some("x") => (true, false),
                Some("y") => (false, true),
                _ => (false, false),
            };
            assert_eq!(
                (
                    &response["mirrored_units"][0]["mirror_x"],
                    &response["mirrored_units"][0]["mirror_y"]
                ),
                (&json!(was_x), &json!(was_y)),
                "{reference}: {response}"
            );

            let committed = std::fs::read_to_string(&path).unwrap();
            assert_eq!(
                mirrors(&path, reference)[0].1,
                mirrors(&original, twin)[0].1,
                "{reference} must carry {twin}'s mirror token"
            );
            let lifted =
                field_positions(SHEET, twin).map(|(x, y, angle)| (x, y - TWIN_OFFSET_MM, angle));
            assert_eq!(
                to_nanometres(field_positions(&committed, reference)),
                to_nanometres(lifted),
                "{reference}'s Reference and Value must land on {twin}'s"
            );
            assert_eq!(
                pins(&path, reference, 0.0),
                pins(&original, twin, TWIN_OFFSET_MM),
                "{reference}'s pins must land on {twin}'s"
            );
        }
    }

    /// One gate of a package reflected alone: the other triode and the
    /// heater keep the state the file gave them.
    #[tokio::test]
    async fn a_unit_is_reflected_alone() {
        let (_directory, path) = ecc83();
        assert_eq!(
            mirrors(&path, "U1"),
            vec![(1, None), (2, None), (3, None)],
            "fixture premise"
        );
        let response = body(
            &mirror(json!({ "schematic": path, "reference": "U1", "mirror": "y", "unit": 2 }))
                .await,
        );
        assert_eq!(response["changed_unit_count"], 1, "{response}");
        let reported = &response["mirrored_units"];
        assert_eq!(reported.as_array().unwrap().len(), 1, "{response}");
        assert_eq!(reported[0]["unit"], 2, "{response}");
        assert_eq!(reported[0]["mirror_y"], true, "{response}");
        assert_eq!(reported[0]["mirror_x"], false, "{response}");
        assert_eq!(reported[0]["changed"], true, "{response}");
        assert_eq!(
            mirrors(&path, "U1"),
            vec![(1, None), (2, Some("y".to_owned())), (3, None)]
        );
    }

    /// Clearing one unit leaves a sibling that is still reflected alone.
    #[tokio::test]
    async fn clearing_a_unit_leaves_its_sibling_reflected() {
        let (_directory, path) = ecc83();
        body(&mirror(json!({ "schematic": path, "reference": "U1", "mirror": "x" })).await);
        body(
            &mirror(json!({ "schematic": path, "reference": "U1", "mirror": "none", "unit": 1 }))
                .await,
        );
        assert_eq!(
            mirrors(&path, "U1"),
            vec![
                (1, None),
                (2, Some("x".to_owned())),
                (3, Some("x".to_owned()))
            ]
        );
    }

    #[tokio::test]
    async fn without_a_unit_every_placed_unit_is_reflected() {
        let (_directory, path) = ecc83();
        let response =
            body(&mirror(json!({ "schematic": path, "reference": "U1", "mirror": "x" })).await);
        assert_eq!(response["changed_unit_count"], 3, "{response}");
        let mut numbers = response["mirrored_units"]
            .as_array()
            .unwrap()
            .iter()
            .map(|unit| unit["unit"].as_u64().unwrap())
            .collect::<Vec<_>>();
        numbers.sort_unstable();
        assert_eq!(numbers, [1, 2, 3], "{response}");
        assert!(mirrors(&path, "U1")
            .iter()
            .all(|(_, mirror)| mirror.as_deref() == Some("x")));
    }

    /// Each refusal names its reason and writes nothing. An omitted axis is
    /// refused rather than read as "none", which would clear a mirror the
    /// caller never mentioned.
    #[tokio::test]
    async fn refusals_write_nothing() {
        for (args, reason) in [
            (
                json!({ "reference": "U1", "unit": 4, "mirror": "x" }),
                "no placed unit 4 on this sheet (placed: 1, 2, 3)",
            ),
            (
                json!({ "reference": "U1", "unit": 0, "mirror": "x" }),
                "no placed unit 0 on this sheet (placed: 1, 2, 3)",
            ),
            (
                json!({ "reference": "U1" }),
                "Argument 'mirror' is invalid: missing or not a string",
            ),
            (json!({ "reference": "U1", "mirror": "X" }), r#"got "X""#),
            (json!({ "reference": "U1", "mirror": "xy" }), r#"got "xy""#),
            (json!({ "reference": "U9", "mirror": "x" }), "U9"),
        ] {
            let (_directory, path) = ecc83();
            let before = std::fs::read_to_string(&path).unwrap();
            let mut args = args;
            args["schematic"] = json!(path);
            let result = mirror(args.clone()).await;
            assert!(result.is_error, "{args} must be refused");
            let ToolContent::Text { text } = &result.content[0] else {
                panic!("expected text result");
            };
            // Structured refusals carry their reason in `message`.
            let message = serde_json::from_str::<serde_json::Value>(text)
                .ok()
                .and_then(|body| body["message"].as_str().map(str::to_owned))
                .unwrap_or_else(|| text.clone());
            assert!(message.contains(reason), "{args}: {message}");
            assert_eq!(std::fs::read_to_string(&path).unwrap(), before, "{args}");
        }
    }

    /// `(mirror x)` on an upright resistor swaps its pin ends, so the
    /// no-connect on pin 2's top end must follow the pin to the bottom.
    #[tokio::test]
    async fn a_reflection_carries_the_marker_with_its_pin() {
        const R2_MARKER: &str = "1a251b9a-30be-43fa-bf5f-04706ed82788";
        let (_directory, path) = sheet(
            include_str!("../../../tests/fixtures/no_connect_carry_kicad10.kicad_sch"),
            "carry.kicad_sch",
        );
        let before = std::fs::read_to_string(&path).unwrap();
        assert_eq!(
            super::no_connect_carry_tests::marker_at(&before, R2_MARKER),
            (158.75, 156.21),
            "fixture premise"
        );
        let response =
            body(&mirror(json!({ "schematic": path, "reference": "R2", "mirror": "x" })).await);
        assert_eq!(response["no_connects_moved_count"], 1, "{response}");
        assert_eq!(
            response["no_connects_moved"][0]["uuid"], R2_MARKER,
            "{response}"
        );
        let committed = std::fs::read_to_string(&path).unwrap();
        assert_eq!(
            super::no_connect_carry_tests::marker_at(&committed, R2_MARKER),
            (158.75, 163.83)
        );
    }

    /// A reflection on a turned body is stored in the pair eeschema writes
    /// for it, and the re-expression moves nothing: the fields and pins are
    /// where the requested reflection, applied without it, puts them.
    #[tokio::test]
    async fn the_orientation_is_stored_as_eeschema_writes_it() {
        for rotation in [0.0, 90.0, 180.0, 270.0] {
            for axis in ["x", "y"] {
                let (_directory, path) = sheet(SHEET, "turned.kicad_sch");
                body(
                    &handle_rotate_schematic_component(
                        &json!({ "schematic": path, "reference": "U1", "rotation": rotation }),
                        &context(),
                    )
                    .await
                    .unwrap(),
                );
                // The expectation: the same reflection, left as requested.
                let turned = std::fs::read_to_string(&path).unwrap();
                let mut unnormalized =
                    cse::Schematic::from_source(&path, turned.clone()).expect("sheet parses");
                unnormalized
                    .symbols
                    .iter_mut()
                    .find(|symbol| symbol.reference() == Some("U1"))
                    .unwrap()
                    .set_mirror(Some(axis));
                let (_expected_dir, expected) =
                    sheet(&unnormalized.to_source(), "expected.kicad_sch");

                let response = body(
                    &mirror(json!({ "schematic": path, "reference": "U1", "mirror": axis })).await,
                );
                let (want_rotation, want_mirror) = cse::kicad_orientation(rotation, Some(axis));
                let reported = &response["mirrored_units"][0];
                assert_eq!(
                    reported["rotation"], want_rotation,
                    "{rotation}° {axis}: {response}"
                );
                assert_eq!(
                    mirrors(&path, "U1")[0].1.as_deref(),
                    want_mirror,
                    "{rotation}° {axis}"
                );
                let committed = std::fs::read_to_string(&path).unwrap();
                assert!(
                    committed.contains(&format!("(at 139.7 50.8 {want_rotation})")),
                    "{rotation}° {axis}: the stored turn must be {want_rotation}"
                );
                assert_eq!(
                    to_nanometres(field_positions(&committed, "U1")),
                    to_nanometres(field_positions(&unnormalized.to_source(), "U1")),
                    "{rotation}° {axis}: fields must not move"
                );
                assert_eq!(
                    pins(&path, "U1", 0.0),
                    pins(&expected, "U1", 0.0),
                    "{rotation}° {axis}: pins must not move"
                );
            }
        }
    }

    /// Asking for the state a unit already has changes nothing and says so:
    /// the file is not rewritten, not even its timestamp.
    #[tokio::test]
    async fn a_unit_already_as_requested_is_left_alone() {
        let (_directory, path) = sheet(SHEET, "again.kicad_sch");
        assert_eq!(
            mirrors(&path, "D2")[0].1.as_deref(),
            Some("x"),
            "fixture premise"
        );
        let modified = std::fs::metadata(&path).unwrap().modified().unwrap();
        let response =
            body(&mirror(json!({ "schematic": path, "reference": "D2", "mirror": "x" })).await);
        assert_eq!(response["changed_unit_count"], 0, "{response}");
        assert_eq!(
            response["mirrored_units"][0]["changed"], false,
            "{response}"
        );
        assert_eq!(std::fs::read_to_string(&path).unwrap(), SHEET);
        assert_eq!(
            std::fs::metadata(&path).unwrap().modified().unwrap(),
            modified
        );
    }

    /// A reflected pin that lands mid-span on a wire gets its dot, as one
    /// moved or turned onto the wire does. KiCad's own netlist puts U1 pin 3
    /// on `/NETM` with the dot and leaves it unconnected without it (see the
    /// fixture's README).
    #[tokio::test]
    async fn a_pin_reflected_onto_a_wire_gains_a_junction() {
        let (_directory, path) = sheet(
            include_str!("../../../tests/fixtures/mirror_junctions_kicad10.kicad_sch"),
            "junction.kicad_sch",
        );
        let before = std::fs::read_to_string(&path).unwrap();
        assert!(
            !super::super::component_ops_tests::component_delete_connectivity_tests::has_junction(
                &before, 147.32, 50.8
            ),
            "fixture premise"
        );
        let response =
            body(&mirror(json!({ "schematic": path, "reference": "U1", "mirror": "y" })).await);
        assert_eq!(response["junctions_added_count"], 1, "{response}");
        assert_eq!(response["junctions_pruned_count"], 0, "{response}");
        let committed = std::fs::read_to_string(&path).unwrap();
        assert!(
            super::super::component_ops_tests::component_delete_connectivity_tests::has_junction(
                &committed, 147.32, 50.8
            ),
            "pin 3 must carry a dot where it lands on the NETM wire"
        );
    }

    /// The ECC83 sheet with two states KiCad itself never writes: the heater
    /// (unit 3) turned 180° with `(mirror x)`, and unit 1 carrying a literal
    /// `(mirror none)`, as an older writer could leave them.
    fn ecc83_with_odd_orientations() -> (tempfile::TempDir, std::path::PathBuf) {
        let (directory, path) = ecc83();
        let mut schematic = cse::Schematic::load(&path).unwrap();
        for symbol in schematic
            .symbols
            .iter_mut()
            .filter(|symbol| symbol.reference() == Some("U1"))
        {
            match symbol.unit {
                1 => symbol.mirror = Some("none".to_owned()),
                3 => {
                    symbol.at.rotation = Some(180.0);
                    symbol.mirror = Some("x".to_owned());
                }
                _ => {}
            }
        }
        std::fs::write(&path, schematic.to_source()).unwrap();
        (directory, path)
    }

    /// Only the selected unit is normalised. A sibling left out keeps the
    /// pair it has, even one eeschema would rewrite: the caller did not ask
    /// for it to change.
    #[tokio::test]
    async fn a_unit_left_out_keeps_its_pair_even_a_non_canonical_one() {
        let (_directory, path) = ecc83_with_odd_orientations();
        body(
            &mirror(json!({ "schematic": path, "reference": "U1", "mirror": "y", "unit": 2 }))
                .await,
        );
        assert_eq!(
            mirrors(&path, "U1"),
            vec![
                (1, Some("none".to_owned())),
                (2, Some("y".to_owned())),
                (3, Some("x".to_owned()))
            ]
        );
        let schematic = cse::Schematic::load(&path).unwrap();
        let heater = schematic
            .symbols
            .iter()
            .find(|symbol| symbol.reference() == Some("U1") && symbol.unit == 3)
            .unwrap();
        assert_eq!(heater.at.rotation, Some(180.0));
    }

    /// A literal `(mirror none)` is the same drawing as no token, but not the
    /// same file. Clearing it is a change: the token goes, and the response
    /// says so.
    #[tokio::test]
    async fn clearing_a_literal_none_token_is_reported_as_a_change() {
        let (_directory, path) = ecc83_with_odd_orientations();
        let response = body(
            &mirror(json!({ "schematic": path, "reference": "U1", "mirror": "none", "unit": 1 }))
                .await,
        );
        assert_eq!(response["changed_unit_count"], 1, "{response}");
        assert_eq!(response["mirrored_units"][0]["changed"], true, "{response}");
        assert_eq!(mirrors(&path, "U1")[0], (1, None));
        assert!(!std::fs::read_to_string(&path)
            .unwrap()
            .contains("(mirror none)"));
    }

    #[test]
    fn the_tool_is_registered_in_sch_components() {
        assert!(tools()
            .iter()
            .any(|tool| tool.name == "mirror_schematic_component"));
    }
}

/// Issue #568: placement (`add_schematic_component`, `batch_place_components`,
/// `add_power_symbol`) and `rotate_schematic_component` must store and report
/// KiCad's canonical `(rotation, mirror)` pair so a KiCad save changes nothing
/// and later rotations compute their delta from the stored angle.
mod orientation_normalization_tests {
    use super::super::*;
    use crate::mcp::handler::McpHandler;
    use crate::tools::ServerConfig;

    const ECC83_SHEET: &str = include_str!("../../../tests/fixtures/ecc83_multiunit.kicad_sch");
    const POWER_SHEET: &str = include_str!("../../../tests/fixtures/power_rows_kicad10.kicad_sch");

    /// Oracle measured via `kicad-cli sch upgrade --force` (KiCad 10.0.8) —
    /// restated as literals here rather than imported from `kicad_orientation`
    /// so production and test cannot silently drift together.
    const KICAD_PAIR_ORACLE: [(f64, &str, f64, Option<&str>); 6] = [
        (0.0, "x", 0.0, Some("x")),
        (90.0, "x", 90.0, Some("x")),
        (180.0, "x", 0.0, Some("y")),
        (90.0, "y", 270.0, Some("x")),
        (180.0, "y", 0.0, Some("x")),
        (270.0, "y", 90.0, Some("x")),
    ];

    fn sheet(source: &str, name: &str) -> (tempfile::TempDir, std::path::PathBuf) {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join(name);
        std::fs::write(&path, source).unwrap();
        (directory, path)
    }

    async fn call(
        handler: &McpHandler,
        name: &str,
        arguments: serde_json::Value,
    ) -> serde_json::Value {
        let response = handler
            .handle_message(json!({
                "jsonrpc": "2.0",
                "id": 568,
                "method": "tools/call",
                "params": { "name": name, "arguments": arguments }
            }))
            .await
            .unwrap()
            .result
            .unwrap();
        assert_ne!(
            response["isError"],
            json!(true),
            "{name} unexpectedly failed: {response}"
        );
        serde_json::from_str(response["content"][0]["text"].as_str().unwrap()).unwrap()
    }

    fn stored_orientation(path: &std::path::Path, reference: &str) -> (f64, Option<String>) {
        let sch = cse::Schematic::load(path).unwrap();
        let sym = sch
            .symbols
            .iter()
            .find(|symbol| symbol.reference() == Some(reference))
            .unwrap_or_else(|| panic!("symbol {reference} not found"));
        (sym.at.rotation.unwrap_or(0.0), sym.mirror.clone())
    }

    fn pins_nm(path: &std::path::Path, reference: &str) -> Vec<(String, i64, i64)> {
        let (_, tree) = read_schematic(path).unwrap();
        let located = pin_locations_for_reference(&tree, reference).unwrap();
        let mut pins = located["pins"]
            .as_array()
            .unwrap()
            .iter()
            .map(|pin| {
                (
                    pin["number"].as_str().unwrap().to_owned(),
                    (pin["x"].as_f64().unwrap() * 1_000_000.0).round() as i64,
                    (pin["y"].as_f64().unwrap() * 1_000_000.0).round() as i64,
                )
            })
            .collect::<Vec<_>>();
        pins.sort();
        pins
    }

    #[tokio::test]
    async fn add_schematic_component_normalizes_orientation_and_reports_stored_pair() {
        let handler = McpHandler::new(ServerConfig {
            eager_toolsets: true,
            ..ServerConfig::default()
        })
        .await
        .unwrap();
        let (_dir, path) = sheet(ECC83_SHEET, "ecc83-pp.kicad_sch");

        for (index, &(req_rot, req_mirror, want_rot, want_mirror)) in
            KICAD_PAIR_ORACLE.iter().enumerate()
        {
            let reference = format!("C{}", 20 + index);
            let x = 25.4 + (index as f64) * 25.4;
            let body = call(
                &handler,
                "add_schematic_component",
                json!({
                    "schematic": path.display().to_string(),
                    "lib_id": "ecc83-pp:CP",
                    "x": x,
                    "y": 127.0,
                    "rotation": req_rot,
                    "mirror": req_mirror,
                    "reference": reference,
                }),
            )
            .await;

            assert_eq!(
                body["rotation"], want_rot,
                "{req_rot}° {req_mirror} response: {body}"
            );
            assert_eq!(
                body["mirror_x"].as_bool().unwrap_or(false),
                want_mirror == Some("x"),
                "{req_rot}° {req_mirror} response: {body}"
            );
            assert_eq!(
                body["mirror_y"].as_bool().unwrap_or(false),
                want_mirror == Some("y"),
                "{req_rot}° {req_mirror} response: {body}"
            );
            let (stored_rot, stored_mirror) = stored_orientation(&path, &reference);
            assert_eq!(
                (stored_rot, stored_mirror.as_deref()),
                (want_rot, want_mirror),
                "{req_rot}° {req_mirror} on disk"
            );
        }
    }

    #[tokio::test]
    async fn batch_place_components_normalizes_orientation_and_reports_stored_pair() {
        let handler = McpHandler::new(ServerConfig {
            eager_toolsets: true,
            ..ServerConfig::default()
        })
        .await
        .unwrap();
        let (_dir, path) = sheet(ECC83_SHEET, "ecc83-pp.kicad_sch");

        let components = KICAD_PAIR_ORACLE
            .iter()
            .enumerate()
            .map(|(index, &(req_rot, req_mirror, _, _))| {
                json!({
                    "lib_id": "ecc83-pp:CP",
                    "x": 25.4 + (index as f64) * 25.4,
                    "y": 127.0,
                    "rotation": req_rot,
                    "mirror": req_mirror,
                    "reference": format!("C{}", 40 + index),
                })
            })
            .collect::<Vec<_>>();

        let body = call(
            &handler,
            "batch_place_components",
            json!({
                "schematic": path.display().to_string(),
                "components": components,
            }),
        )
        .await;
        assert_eq!(body["placed_count"], KICAD_PAIR_ORACLE.len(), "{body}");
        let placed = body["placed"].as_array().unwrap();

        for (index, &(req_rot, req_mirror, want_rot, want_mirror)) in
            KICAD_PAIR_ORACLE.iter().enumerate()
        {
            let reference = format!("C{}", 40 + index);
            let entry = placed
                .iter()
                .find(|item| item["reference"] == reference)
                .unwrap_or_else(|| panic!("missing {reference} in {body}"));
            assert_eq!(
                entry["rotation"], want_rot,
                "batch {req_rot}° {req_mirror}: {entry}"
            );
            assert_eq!(
                entry["mirror_x"].as_bool().unwrap_or(false),
                want_mirror == Some("x"),
                "batch {req_rot}° {req_mirror}: {entry}"
            );
            assert_eq!(
                entry["mirror_y"].as_bool().unwrap_or(false),
                want_mirror == Some("y"),
                "batch {req_rot}° {req_mirror}: {entry}"
            );
            let (stored_rot, stored_mirror) = stored_orientation(&path, &reference);
            assert_eq!(
                (stored_rot, stored_mirror.as_deref()),
                (want_rot, want_mirror),
                "batch {req_rot}° {req_mirror} on disk"
            );
        }
    }

    #[tokio::test]
    async fn rotate_schematic_component_normalizes_mirrored_symbol_and_preserves_pins() {
        let handler = McpHandler::new(ServerConfig {
            eager_toolsets: true,
            ..ServerConfig::default()
        })
        .await
        .unwrap();

        // Start from a 0° y-mirrored asymmetric polarized cap (C20) and rotate to 90°, 180°, 270°.
        for &(target_rot, want_rot, want_mirror) in &[
            (90.0, 270.0, Some("x")),
            (180.0, 0.0, Some("x")),
            (270.0, 90.0, Some("x")),
        ] {
            let (_dir, path) = sheet(ECC83_SHEET, "ecc83-pp.kicad_sch");
            call(
                &handler,
                "add_schematic_component",
                json!({
                    "schematic": path.display().to_string(),
                    "lib_id": "ecc83-pp:CP",
                    "x": 50.8,
                    "y": 127.0,
                    "rotation": 0.0,
                    "mirror": "y",
                    "reference": "C20",
                }),
            )
            .await;

            // Reference expectation: the unnormalized rotation applied directly.
            let mut unnormalized = cse::Schematic::load(&path).unwrap();
            unnormalized
                .symbols
                .iter_mut()
                .find(|s| s.reference() == Some("C20"))
                .unwrap()
                .set_rotation(target_rot);
            let (_exp_dir, exp_path) = sheet(&unnormalized.to_source(), "expected.kicad_sch");

            let body = call(
                &handler,
                "rotate_schematic_component",
                json!({
                    "schematic": path.display().to_string(),
                    "reference": "C20",
                    "rotation": target_rot,
                }),
            )
            .await;

            assert_eq!(
                body["rotation"], want_rot,
                "rotate 0° y -> {target_rot}°: {body}"
            );
            assert_eq!(
                body["mirror_x"],
                want_mirror == Some("x"),
                "rotate 0° y -> {target_rot}°: {body}"
            );
            assert_eq!(
                body["mirror_y"],
                want_mirror == Some("y"),
                "rotate 0° y -> {target_rot}°: {body}"
            );
            assert_eq!(
                body["placements"][0]["rotation"], want_rot,
                "placements[0]: {body}"
            );
            assert_eq!(
                body["placements"][0]["mirror_x"],
                want_mirror == Some("x"),
                "placements[0]: {body}"
            );
            assert_eq!(
                body["placements"][0]["mirror_y"],
                want_mirror == Some("y"),
                "placements[0]: {body}"
            );
            let (stored_rot, stored_mirror) = stored_orientation(&path, "C20");
            assert_eq!(
                (stored_rot, stored_mirror.as_deref()),
                (want_rot, want_mirror),
                "rotate 0° y -> {target_rot}° on disk"
            );
            assert_eq!(
                pins_nm(&path, "C20"),
                pins_nm(&exp_path, "C20"),
                "normalization must not move pins for 0° y -> {target_rot}°"
            );
        }
    }

    #[tokio::test]
    async fn rotate_schematic_component_normalizes_each_unit_of_multi_unit_symbol() {
        let handler = McpHandler::new(ServerConfig {
            eager_toolsets: true,
            ..ServerConfig::default()
        })
        .await
        .unwrap();
        let (_dir, path) = sheet(ECC83_SHEET, "ecc83-pp.kicad_sch");

        // Mirror only unit 2 to `y` (0° y), leaving unit 1 unmirrored (0°).
        call(
            &handler,
            "mirror_schematic_component",
            json!({
                "schematic": path.display().to_string(),
                "reference": "U1",
                "mirror": "y",
                "unit": 2,
            }),
        )
        .await;

        // Rotate U1 to 90°: unit 1 becomes (90°, none), unit 2 becomes (270°, x).
        let body = call(
            &handler,
            "rotate_schematic_component",
            json!({
                "schematic": path.display().to_string(),
                "reference": "U1",
                "rotation": 90.0,
            }),
        )
        .await;

        assert_eq!(body["rotation"], 90.0, "{body}");
        assert_eq!(body["mirror_x"], false, "{body}");
        assert_eq!(body["mirror_y"], false, "{body}");
        let placements = body["placements"].as_array().unwrap();
        let u2 = placements
            .iter()
            .find(|u| u["unit"] == 2)
            .expect("unit 2 in placements");
        assert_eq!(
            u2["rotation"], 270.0,
            "unit 2 must normalize 90° y -> 270° x: {u2}"
        );
        assert_eq!(u2["mirror_x"], true, "{u2}");
        assert_eq!(u2["mirror_y"], false, "{u2}");

        let sch = cse::Schematic::load(&path).unwrap();
        let sym2 = sch
            .symbols
            .iter()
            .find(|s| s.reference() == Some("U1") && s.unit == 2)
            .unwrap();
        assert_eq!(sym2.at.rotation, Some(270.0));
        assert_eq!(sym2.mirror.as_deref(), Some("x"));
    }

    #[tokio::test]
    async fn add_power_symbol_normalizes_out_of_range_quarter_turn_rotation() {
        let handler = McpHandler::new(ServerConfig {
            eager_toolsets: true,
            ..ServerConfig::default()
        })
        .await
        .unwrap();
        let (_dir, path) = sheet(POWER_SHEET, "power_rows_kicad10.kicad_sch");

        for &(req_rot, want_rot, x) in &[(360.0, 0.0, 25.4), (-90.0, 270.0, 50.8)] {
            let body = call(
                &handler,
                "add_power_symbol",
                json!({
                    "schematic": path.display().to_string(),
                    "power_net": "GND",
                    "x": x,
                    "y": 25.4,
                    "rotation": req_rot,
                }),
            )
            .await;
            assert_eq!(body["rotation"], want_rot, "power {req_rot}°: {body}");
            let reference = body["reference"].as_str().unwrap();
            let (stored_rot, stored_mirror) = stored_orientation(&path, reference);
            assert_eq!((stored_rot, stored_mirror), (want_rot, None));
        }
    }

    #[test]
    fn placement_readback_rejects_unnormalized_orientation_left_on_disk() {
        let (_dir, path) = sheet(ECC83_SHEET, "ecc83-pp.kicad_sch");
        let mut sch = cse::Schematic::load(&path).unwrap();
        let context = crate::tools::sheet_instance_context(&path, &mut sch).unwrap();
        // Simulate a broken writer that wrote (180°, x) directly to disk.
        let mut sym = cse::Symbol::new("ecc83-pp:CP", 50.8, 127.0);
        sym.at.rotation = Some(180.0);
        sym.mirror = Some("x".to_string());
        sym.unit = 1;
        let metadata = cse::library::symbol_metadata(&sch, "ecc83-pp:CP");
        let fields = PlacementFields::resolve("ecc83-pp:CP", &metadata, None, None);
        sym.in_bom = fields.in_bom;
        sym.on_board = fields.on_board;
        sym.properties.push(crate::tools::positioned_property(
            "Reference",
            "C99",
            50.8,
            127.0,
            0.0,
            false,
            cse::library::FieldJustify::default(),
        ));
        sym.properties.push(crate::tools::positioned_property(
            "Value",
            &fields.value,
            50.8,
            127.0,
            0.0,
            false,
            cse::library::FieldJustify::default(),
        ));
        sym.properties.push(crate::tools::positioned_property(
            "Footprint",
            &fields.footprint,
            50.8,
            127.0,
            0.0,
            true,
            cse::library::FieldJustify::default(),
        ));
        for instance_path in &context.instance_paths {
            sym.set_instance_path(&context.project_name, instance_path, "C99", 1);
        }
        let uuid = sym.uuid.clone();
        sch.add_symbol(sym);

        let expected = ComponentTargetUnit::placement(
            &uuid,
            &context,
            "ecc83-pp:CP",
            50.8,
            127.0,
            180.0,
            Some("x"),
            "C99",
            &fields,
            1,
        );
        let err = placed_component_readback(&path, &sch, &expected, &context)
            .expect_err("unnormalized (180°, x) on disk must fail canonical (0°, y) readback");
        let text = match &err.content[0] {
            ToolContent::Text { text } => text,
            other => panic!("expected text error, got {other:?}"),
        };
        assert!(
            text.contains("observed rotation differs"),
            "expected rotation mismatch against canonical (0°, y), got: {text}"
        );
    }
}
