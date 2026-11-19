//! JLCPCB's coordinate conversion uses KiCad's own board-space pad boxes.
//! No pad-shape approximation or local-box rotation lives here.
use anyhow::{Context, Result};
use copperpour_ipc::{gen::kiapi, KiCadIpcClient};
use prost::Message;
use serde::Serialize;
use std::collections::{BTreeMap, BTreeSet};

#[derive(Clone, Debug, Serialize)]
pub(crate) struct Midpoint {
    pub designator: String,
    pub anchor_x_mm: f64,
    pub anchor_y_mm: f64,
    pub midpoint_x_mm: f64,
    pub midpoint_y_mm: f64,
    pub algorithm: &'static str,
    /// The pads KiCad reports, the centre of pad 1's board-space box, and
    /// whether the footprint is SMD: the EasyEDA CPL of an order link takes
    /// them (#208). Kept out of the export's geometry evidence.
    #[serde(skip)]
    #[cfg_attr(not(feature = "jlcpcb"), allow(dead_code))]
    pub pad_count: usize,
    #[serde(skip)]
    #[cfg_attr(not(feature = "jlcpcb"), allow(dead_code))]
    pub pin1_mm: Option<(f64, f64)>,
    #[serde(skip)]
    #[cfg_attr(not(feature = "jlcpcb"), allow(dead_code))]
    pub smd: bool,
}

pub(crate) struct Snapshot {
    pub contents: String,
    pub midpoints: BTreeMap<String, Midpoint>,
    /// The parts fitted by hand (#681): footprints KiCad keeps out of position
    /// files while they are still fitted (not DNP) and still bought (in the
    /// BOM, in the schematic). The JLCPCB BOM leaves them to their own list,
    /// as the CPL already leaves them out. A designator the CPL places too
    /// stays with the assembler.
    pub hand_fitted: BTreeSet<String>,
}

pub(crate) fn capture(
    client: &KiCadIpcClient,
    document: kiapi::common::types::DocumentSpecifier,
) -> Result<Snapshot> {
    let contents = client.save_document_to_string_in(document.clone())?;
    let items = client.get_items_in(
        document.clone(),
        kiapi::common::types::KiCadObjectType::KotPcbFootprint,
    )?;
    let mut midpoints = BTreeMap::new();
    let mut hand_fitted = BTreeSet::new();
    for item in items {
        let fp = kiapi::board::types::FootprintInstance::decode(item.value.as_slice())?;
        if let Some(attrs) = fp.attributes.as_ref() {
            if attrs.do_not_populate {
                continue;
            }
            if attrs.exclude_from_position_files {
                let reference = fp
                    .reference_field
                    .as_ref()
                    .and_then(|field| field.text.as_ref())
                    .and_then(|text| text.text.as_ref())
                    .map(|text| text.text.as_str())
                    .filter(|reference| !reference.is_empty());
                if let Some(reference) = reference
                    .filter(|_| !attrs.exclude_from_bill_of_materials && !attrs.not_in_schematic)
                {
                    hand_fitted.insert(reference.to_string());
                }
                continue;
            }
        }
        let reference = fp
            .reference_field
            .as_ref()
            .and_then(|field| field.text.as_ref())
            .and_then(|text| text.text.as_ref())
            .context("footprint missing reference text")?
            .text
            .clone();
        anyhow::ensure!(!reference.is_empty(), "footprint has empty designator");
        let anchor = fp.position.context("footprint missing anchor")?;
        let definition = fp.definition.context("footprint missing definition")?;
        let mut ids = Vec::new();
        let mut pin1_id = None;
        let mut all_pads_smd = true;
        for item in definition.items {
            if item.type_url.ends_with("kiapi.board.types.Pad") {
                let pad = kiapi::board::types::Pad::decode(item.value.as_slice())?;
                let id = pad.id.context("pad missing UUID")?.value;
                anyhow::ensure!(!id.is_empty(), "pad has empty UUID");
                all_pads_smd &= pad.r#type == kiapi::board::types::PadType::PtSmd as i32;
                if pin1_id.is_none() && pad.number == "1" {
                    pin1_id = Some(id.clone());
                }
                ids.push(id);
            }
        }
        let boxes = client.get_item_boxes_in(document.clone(), &ids)?;
        let pin1_mm = pin1_id
            .and_then(|id| boxes.get(&id))
            .map(|bounds| union_center(std::iter::once(bounds)))
            .transpose()?;
        // The footprint's mounting style when it has one; otherwise its pads
        // decide, as an `(attr)`-less footprint of SMD pads is still SMD.
        use kiapi::board::types::FootprintMountingStyle as Style;
        let smd = match fp.attributes.as_ref().map(|attrs| attrs.mounting_style) {
            Some(style) if style == Style::FmsSmd as i32 => true,
            Some(style) if style == Style::FmsThroughHole as i32 => false,
            _ => !ids.is_empty() && all_pads_smd,
        };
        let (x, y) = if boxes.is_empty() {
            (anchor.x_nm as f64 / 1e6, anchor.y_nm as f64 / 1e6)
        } else {
            union_center(boxes.values())?
        };
        let midpoint = Midpoint {
            designator: reference.clone(),
            anchor_x_mm: anchor.x_nm as f64 / 1e6,
            anchor_y_mm: anchor.y_nm as f64 / 1e6,
            midpoint_x_mm: x,
            midpoint_y_mm: y,
            algorithm: if ids.is_empty() {
                "no_pads_observed_anchor"
            } else {
                "kicad_native_board_pad_boxes_union_center"
            },
            pad_count: ids.len(),
            pin1_mm,
            smd,
        };
        anyhow::ensure!(
            midpoints.insert(reference, midpoint).is_none(),
            "duplicate footprint designator"
        );
    }
    anyhow::ensure!(
        client.save_document_to_string_in(document)? == contents,
        "board changed while reading JLCPCB midpoint geometry; retry after edits stop"
    );
    hand_fitted.retain(|reference| !midpoints.contains_key(reference));
    Ok(Snapshot {
        contents,
        midpoints,
        hand_fitted,
    })
}

fn union_center<'a>(
    boxes: impl Iterator<Item = &'a kiapi::common::types::Box2>,
) -> Result<(f64, f64)> {
    let (mut left, mut top, mut right, mut bottom) = (i128::MAX, i128::MAX, i128::MIN, i128::MIN);
    for bounds in boxes {
        let p = bounds.position.as_ref().context("box missing position")?;
        let size = bounds.size.as_ref().context("box missing size")?;
        anyhow::ensure!(size.x_nm >= 0 && size.y_nm >= 0, "negative box size");
        left = left.min(i128::from(p.x_nm));
        top = top.min(i128::from(p.y_nm));
        right = right.max(i128::from(p.x_nm) + i128::from(size.x_nm));
        bottom = bottom.max(i128::from(p.y_nm) + i128::from(size.y_nm));
    }
    anyhow::ensure!(left != i128::MAX, "empty box union");
    // BOX2I::GetCenter uses integer half-size; preserve KiCad's nm rounding.
    Ok((
        (left + (right - left) / 2) as f64 / 1e6,
        (top + (bottom - top) / 2) as f64 / 1e6,
    ))
}

pub(crate) fn convert_positions(
    source: &str,
    midpoints: &BTreeMap<String, Midpoint>,
) -> Result<(String, Vec<Midpoint>)> {
    let mut reader = csv::ReaderBuilder::new()
        .trim(csv::Trim::All)
        .from_reader(source.as_bytes());
    let header = reader.headers()?.clone();
    let index = |name| {
        header
            .iter()
            .position(|field| field == name)
            .with_context(|| format!("missing position column {name}"))
    };
    let (reference, ix, iy) = (index("Ref")?, index("PosX")?, index("PosY")?);
    let mut writer = csv::Writer::from_writer(Vec::new());
    writer.write_record(&header)?;
    let mut evidence = Vec::new();
    for row in reader.records() {
        let row = row?;
        let designator = row.get(reference).context("missing designator")?;
        let midpoint = midpoints
            .get(designator)
            .with_context(|| format!("no native midpoint for {designator}"))?;
        let mut fields = row.iter().map(str::to_owned).collect::<Vec<_>>();
        // The native CLI wrapper uses board origin, mm, Y-up, no bottom-X negation.
        let x: f64 = row.get(ix).context("missing PosX")?.parse()?;
        let y: f64 = row.get(iy).context("missing PosY")?.parse()?;
        anyhow::ensure!(
            x.is_finite()
                && y.is_finite()
                && (x - midpoint.anchor_x_mm).abs() <= 0.000001
                && (y + midpoint.anchor_y_mm).abs() <= 0.000001,
            "CLI anchor disagrees with native snapshot for {designator}"
        );
        if (midpoint.midpoint_x_mm - midpoint.anchor_x_mm).abs() > 1e-9 {
            fields[ix] = format!("{:.6}", midpoint.midpoint_x_mm);
        }
        if (midpoint.midpoint_y_mm - midpoint.anchor_y_mm).abs() > 1e-9 {
            fields[iy] = format!("{:.6}", -midpoint.midpoint_y_mm);
        }
        writer.write_record(fields)?;
        evidence.push(midpoint.clone());
    }
    Ok((String::from_utf8(writer.into_inner()?)?, evidence))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::MockIpcServer;
    use copperpour_ipc::builders::pack_any;

    fn box_at(x: i64, y: i64, w: i64, h: i64) -> kiapi::common::types::Box2 {
        kiapi::common::types::Box2 {
            position: Some(kiapi::common::types::Vector2 { x_nm: x, y_nm: y }),
            size: Some(kiapi::common::types::Vector2 { x_nm: w, y_nm: h }),
        }
    }

    fn fixture_server(board: &std::path::Path, mode: &'static str) -> MockIpcServer {
        use std::sync::atomic::{AtomicUsize, Ordering};
        let document =
            crate::tools::pcb_board::board_mock::board_document(&board.to_string_lossy());
        let reads = AtomicUsize::new(0);
        MockIpcServer::spawn("midpoint-capture", move |request| {
            let command = request.message.unwrap();
            let response = if command.type_url.ends_with("GetOpenDocuments") {
                pack_any(
                    &kiapi::common::commands::GetOpenDocumentsResponse {
                        documents: if mode == "closed" {
                            vec![]
                        } else {
                            vec![document.clone()]
                        },
                    },
                    "kiapi.common.commands.GetOpenDocumentsResponse",
                )
            } else if command.type_url.ends_with("SaveDocumentToString") {
                let command =
                    kiapi::common::commands::SaveDocumentToString::decode(command.value.as_slice())
                        .unwrap();
                assert_eq!(command.document.as_ref(), Some(&document));
                let mut contents =
                    include_str!("../../../tests/fixtures/jlcpcb_midpoints/midpoints.kicad_pcb")
                        .to_string();
                if mode == "drift" && reads.fetch_add(1, Ordering::SeqCst) > 0 {
                    contents.push('\n');
                }
                pack_any(
                    &kiapi::common::commands::SavedDocumentResponse {
                        document: Some(document.clone()),
                        contents,
                    },
                    "kiapi.common.commands.SavedDocumentResponse",
                )
            } else if command.type_url.ends_with("GetItems") {
                let command =
                    kiapi::common::commands::GetItems::decode(command.value.as_slice()).unwrap();
                assert_eq!(command.header.unwrap().document.as_ref(), Some(&document));
                let mut items = kiapi::common::commands::GetItemsResponse::decode(
                    include_bytes!("../../../tests/fixtures/jlcpcb_midpoints/footprints.pb")
                        .as_slice(),
                )
                .unwrap();
                if mode == "pad1-last" {
                    // Reorder native-captured items without changing any
                    // UUID or geometry: pad 1 must be found by its number.
                    for item in &mut items.items {
                        let mut fp =
                            kiapi::board::types::FootprintInstance::decode(item.value.as_slice())
                                .unwrap();
                        fp.definition.as_mut().unwrap().items.reverse();
                        item.value = fp.encode_to_vec();
                    }
                    let first = kiapi::board::types::FootprintInstance::decode(
                        items.items[0].value.as_slice(),
                    )
                    .unwrap();
                    let first_pad = first
                        .definition
                        .unwrap()
                        .items
                        .into_iter()
                        .find(|item| item.type_url.ends_with("kiapi.board.types.Pad"))
                        .unwrap();
                    assert_ne!(
                        kiapi::board::types::Pad::decode(first_pad.value.as_slice())
                            .unwrap()
                            .number,
                        "1"
                    );
                }
                if mode == "no-pads" {
                    items.items.truncate(1);
                    let mut fp = kiapi::board::types::FootprintInstance::decode(
                        items.items[0].value.as_slice(),
                    )
                    .unwrap();
                    fp.definition
                        .as_mut()
                        .unwrap()
                        .items
                        .retain(|item| !item.type_url.ends_with("kiapi.board.types.Pad"));
                    items.items[0].value = fp.encode_to_vec();
                }
                if matches!(
                    mode,
                    "excluded-with-pads" | "excluded-no-pads" | "dnp" | "duplicate" | "bom-only"
                ) {
                    // Change only attributes/reference on native-captured items;
                    // leave the pad geometry and UUID oracle untouched.
                    let first = kiapi::board::types::FootprintInstance::decode(
                        items.items[0].value.as_slice(),
                    )
                    .unwrap();
                    for item in &mut items.items[1..3] {
                        let mut fp =
                            kiapi::board::types::FootprintInstance::decode(item.value.as_slice())
                                .unwrap();
                        fp.reference_field = first.reference_field.clone();
                        let attrs = fp.attributes.get_or_insert_default();
                        attrs.exclude_from_position_files = mode.starts_with("excluded-");
                        attrs.do_not_populate = mode == "dnp";
                        attrs.exclude_from_bill_of_materials = mode == "bom-only";
                        if mode == "excluded-no-pads" {
                            fp.definition
                                .as_mut()
                                .unwrap()
                                .items
                                .retain(|item| !item.type_url.ends_with("kiapi.board.types.Pad"));
                        }
                        item.value = fp.encode_to_vec();
                    }
                }
                if matches!(mode, "hand-fitted" | "hand-fitted-hole") {
                    // JF3 and JF4 are fitted by hand: out of position files
                    // only. In "hand-fitted-hole" JF4 is out of the BOM too,
                    // as a mounting hole is.
                    for item in &mut items.items {
                        let mut fp =
                            kiapi::board::types::FootprintInstance::decode(item.value.as_slice())
                                .unwrap();
                        let reference = fp
                            .reference_field
                            .as_ref()
                            .and_then(|field| field.text.as_ref())
                            .and_then(|text| text.text.as_ref())
                            .map(|text| text.text.clone())
                            .unwrap();
                        if reference == "JF3" || reference == "JF4" {
                            let attrs = fp.attributes.get_or_insert_default();
                            attrs.exclude_from_position_files = true;
                            attrs.exclude_from_bill_of_materials =
                                mode == "hand-fitted-hole" && reference == "JF4";
                            item.value = fp.encode_to_vec();
                        }
                    }
                }
                pack_any(&items, "kiapi.common.commands.GetItemsResponse")
            } else if command.type_url.ends_with("GetBoundingBox") {
                let requested =
                    kiapi::common::commands::GetBoundingBox::decode(command.value.as_slice())
                        .unwrap();
                assert_eq!(requested.header.unwrap().document.as_ref(), Some(&document));
                let captured = kiapi::common::commands::GetBoundingBoxResponse::decode(
                    include_bytes!("../../../tests/fixtures/jlcpcb_midpoints/boxes.pb").as_slice(),
                )
                .unwrap();
                let map: BTreeMap<_, _> = captured
                    .items
                    .into_iter()
                    .zip(captured.boxes)
                    .map(|(id, bounds)| (id.value, bounds))
                    .collect();
                let mut response = kiapi::common::commands::GetBoundingBoxResponse::default();
                for id in requested.items {
                    if mode != "missing-box" {
                        response.boxes.push(map[&id.value]);
                        response.items.push(id);
                    }
                }
                pack_any(&response, "kiapi.common.commands.GetBoundingBoxResponse")
            } else {
                panic!("unexpected command: {}", command.type_url)
            };
            kiapi::common::ApiResponse {
                status: Some(kiapi::common::ApiResponseStatus {
                    status: kiapi::common::ApiStatusCode::AsOk as i32,
                    error_message: String::new(),
                }),
                message: Some(response),
                ..Default::default()
            }
        })
    }

    #[test]
    fn position_exclusions_precede_duplicate_detection_and_geometry() {
        let board = std::path::Path::new("midpoints.kicad_pcb");
        let document =
            crate::tools::pcb_board::board_mock::board_document(&board.to_string_lossy());
        for mode in ["excluded-with-pads", "excluded-no-pads", "dnp"] {
            let mock = fixture_server(board, mode);
            let snapshot = capture(&KiCadIpcClient::new(mock.address()), document.clone()).unwrap();
            assert_eq!(snapshot.midpoints.len(), 9, "{mode}");
            // Two excluded items share the exported first item's reference.
            // They must neither refuse the export nor replace its geometry.
            let point = &snapshot.midpoints["JF0"];
            assert_eq!((point.midpoint_x_mm, point.midpoint_y_mm), (20.0, 21.27));
            let positions =
                "Ref,Val,Package,PosX,PosY,Rot,Side\nJF0,X,JST,20.000000,-20.000000,0,top\n";
            let (converted, evidence) = convert_positions(positions, &snapshot.midpoints).unwrap();
            assert!(converted.contains("20.000000,-21.270000"), "{mode}");
            assert_eq!(evidence.len(), 1, "{mode}");
            // A footprint left out of positions under a placed part's
            // reference does not take that part away from the assembler.
            assert!(snapshot.hand_fitted.is_empty(), "{mode}");
            assert_eq!(
                snapshot.contents,
                include_str!("../../../tests/fixtures/jlcpcb_midpoints/midpoints.kicad_pcb")
            );
        }
        for mode in ["duplicate", "bom-only"] {
            let mock = fixture_server(board, mode);
            let error = capture(&KiCadIpcClient::new(mock.address()), document.clone())
                .err()
                .expect("position-included duplicates must still refuse");
            assert!(
                error.to_string().contains("duplicate footprint designator"),
                "{mode}: {error}"
            );
        }
    }

    /// #681: a footprint out of position files but fitted, bought and in
    /// the schematic is fitted by hand; one also out of the BOM is not.
    #[test]
    fn hand_fitted_parts_are_left_out_of_positions_only() {
        let board = std::path::Path::new("midpoints.kicad_pcb");
        let document =
            crate::tools::pcb_board::board_mock::board_document(&board.to_string_lossy());
        for (mode, expected) in [
            ("hand-fitted", vec!["JF3", "JF4"]),
            ("hand-fitted-hole", vec!["JF3"]),
            ("good", vec![]),
        ] {
            let mock = fixture_server(board, mode);
            let snapshot = capture(&KiCadIpcClient::new(mock.address()), document.clone()).unwrap();
            let expected = expected
                .into_iter()
                .map(String::from)
                .collect::<BTreeSet<_>>();
            assert_eq!(snapshot.hand_fitted, expected, "{mode}");
            let placed = if mode == "good" { 11 } else { 9 };
            assert_eq!(snapshot.midpoints.len(), placed, "{mode}");
            assert!(
                expected
                    .iter()
                    .all(|part| !snapshot.midpoints.contains_key(part)),
                "{mode}"
            );
        }
    }

    #[test]
    fn pad_one_is_selected_by_number_when_another_pad_comes_first() {
        let board = std::path::Path::new("midpoints.kicad_pcb");
        let document =
            crate::tools::pcb_board::board_mock::board_document(&board.to_string_lossy());
        let mock = fixture_server(board, "pad1-last");
        let snapshot = capture(&KiCadIpcClient::new(mock.address()), document).unwrap();
        assert_eq!(snapshot.midpoints["R1"].pin1_mm, Some((79.0875, 20.0)));
        assert_eq!(snapshot.midpoints["JF0"].pin1_mm, Some((20.0, 20.0)));
    }

    #[test]
    fn captured_native_geometry_matches_literal_board_space_oracle() {
        let board = std::path::Path::new("midpoints.kicad_pcb");
        let document =
            crate::tools::pcb_board::board_mock::board_document(&board.to_string_lossy());
        let mock = fixture_server(board, "good");
        let snapshot = capture(&KiCadIpcClient::new(mock.address()), document.clone()).unwrap();
        let expected = [
            (20.0, 21.27),
            (31.27, 20.0),
            (40.0, 18.73),
            (48.73, 20.0),
            (60.721984, 20.721984),
        ];
        // What an order link's EasyEDA CPL takes (#208): the pad count, pad
        // 1's box centre in board space, and SMD from the pads when the
        // footprint states no mounting style, as these don't.
        let r1 = &snapshot.midpoints["R1"];
        assert_eq!(
            (r1.pad_count, r1.pin1_mm, r1.smd),
            (2, Some((79.0875, 20.0)), true)
        );
        let jf0 = &snapshot.midpoints["JF0"];
        assert_eq!(
            (jf0.pad_count, jf0.pin1_mm, jf0.smd),
            (2, Some((20.0, 20.0)), false)
        );
        assert_eq!(snapshot.midpoints.len(), 11);
        let centered = &snapshot.midpoints["R1"];
        assert_eq!(
            (centered.midpoint_x_mm, centered.midpoint_y_mm),
            (80.0, 20.0)
        );
        for side in ["F", "B"] {
            for (i, &point) in expected.iter().enumerate() {
                let observed = &snapshot.midpoints[&format!("J{side}{i}")];
                assert_eq!((observed.midpoint_x_mm, observed.midpoint_y_mm), point);
            }
        }
        for mode in ["drift", "missing-box"] {
            let mock = fixture_server(board, mode);
            assert!(
                capture(&KiCadIpcClient::new(mock.address()), document.clone()).is_err(),
                "{mode}"
            );
        }
        let mock = fixture_server(board, "no-pads");
        let snapshot = capture(&KiCadIpcClient::new(mock.address()), document).unwrap();
        let observed = snapshot.midpoints.values().next().unwrap();
        assert_eq!(observed.algorithm, "no_pads_observed_anchor");
        assert_eq!(observed.midpoint_x_mm, observed.anchor_x_mm);
        assert_eq!(observed.midpoint_y_mm, observed.anchor_y_mm);
    }

    #[tokio::test]
    async fn served_jlcpcb_refuses_closed_drifting_or_missing_geometry_without_output() {
        use crate::mcp::handler::McpHandler;
        use crate::tools::ServerConfig;
        for mode in ["closed", "drift", "missing-box", "duplicate", "bom-only"] {
            let dir = tempfile::tempdir().unwrap();
            let board = dir.path().join("midpoints.kicad_pcb");
            let before =
                include_str!("../../../tests/fixtures/jlcpcb_midpoints/midpoints.kicad_pcb");
            std::fs::write(&board, before).unwrap();
            let output = dir.path().join("package");
            let mock = fixture_server(&board, mode);
            let handler = McpHandler::new(ServerConfig {
                kicad_cli: String::new(),
                kicad_binary: String::new(),
                ipc_address: mock.address().into(),
                project_dir: None,
                jlcpcb_db_path: None,
                auto_load_toolsets: false,
                eager_toolsets: true,
            })
            .await
            .unwrap();
            let response = handler
                .handle_message(serde_json::json!({
                    "jsonrpc": "2.0", "id": 667, "method": "tools/call",
                    "params": {"name": "export_manufacturing_package", "arguments": {
                        "board": board, "output_dir": output, "fab_house": "jlcpcb"
                    }}
                }))
                .await
                .unwrap();
            let result = response.result.unwrap();
            assert_eq!(result["isError"], true, "{mode}: {result}");
            assert!(!output.exists(), "{mode}: refusal must not create output");
            assert_eq!(std::fs::read_to_string(&board).unwrap(), before);
        }
    }

    /// The JLCPCB package of this fixture's board, saved as
    /// `<stem>.kicad_pcb`, through `handle_message` at `limit` 1: the
    /// response text and the output directory. The stand-in kicad-cli
    /// writes one Gerber per requested layer and a drill file, named from
    /// the board, copies KiCad's own position export of the fixture, and
    /// writes a BOM naming its eleven parts, as the fixture has no
    /// schematic. Unix only: the stand-in is a shell script.
    #[cfg(unix)]
    async fn served_package(dir: &std::path::Path, stem: &str) -> (String, std::path::PathBuf) {
        served_package_in(dir, stem, "good").await
    }

    /// [`served_package`] with the IPC fixture's `mode`. In the
    /// "hand-fitted" modes the stand-in's positions leave out JF3 and JF4,
    /// as KiCad does for `exclude_from_pos_files`.
    #[cfg(unix)]
    async fn served_package_in(
        dir: &std::path::Path,
        stem: &str,
        mode: &'static str,
    ) -> (String, std::path::PathBuf) {
        use crate::mcp::handler::McpHandler;
        use crate::tools::ServerConfig;
        let board = dir.join(format!("{stem}.kicad_pcb"));
        std::fs::write(
            &board,
            include_str!("../../../tests/fixtures/jlcpcb_midpoints/midpoints.kicad_pcb"),
        )
        .unwrap();
        let positions = dir.join("positions.csv");
        let all_positions = include_str!("../../../tests/fixtures/jlcpcb_midpoints/positions.csv");
        std::fs::write(
            &positions,
            all_positions
                .lines()
                .filter(|line| {
                    !mode.starts_with("hand-fitted")
                        || !(line.starts_with("\"JF3\"") || line.starts_with("\"JF4\""))
                })
                .map(|line| format!("{line}\n"))
                .collect::<String>(),
        )
        .unwrap();
        // The board is the last argument of each export.
        let cli = crate::tools::cli::test_support::write_script(
            dir,
            "package-cli",
            &format!(
                "#!/bin/sh\nkind=$3\n\
                 while [ \"$#\" -gt 0 ]; do\n\
                 \x20 case \"$1\" in --output) out=$2; shift ;; --layers) layers=$2; shift ;; esac\n\
                 \x20 board=$1\n\
                 \x20 shift\n\
                 done\n\
                 stem=$(basename \"$board\" .kicad_pcb)\n\
                 case \"$kind\" in\n\
                 \x20 gerbers) for layer in $(echo \"$layers\" | tr , ' '); do\n\
                 \x20   echo G04 > \"$out/$stem-$(echo \"$layer\" | tr . _).gbr\"; done ;;\n\
                 \x20 drill) echo M48 > \"${{out}}$stem-PTH.drl\" ;;\n\
                 \x20 pos) cp '{}' \"$out\" ;;\n\
                 \x20 bom) printf 'Reference\\n\"JB0,JB1,JB2,JB3,JB4,JF0,JF1,JF2,JF3,JF4\"\\nR1\\n' > \"$out\" ;;\n\
                 esac\n",
                positions.display()
            ),
            "@exit /b 1\r\n",
        );
        // One verified rule for the ten pin headers; R1 matches none.
        let policy = dir.join("corrections.json");
        std::fs::write(
            &policy,
            serde_json::json!({
                "schema_version": 1,
                "policy_id": "midpoint-headers",
                "provenance": "test",
                "footprint_rules": [{
                    "id": "header",
                    "footprint_exact": "PinHeader_1x02_P2.54mm_Vertical",
                    "rotation_degrees": 90
                }]
            })
            .to_string(),
        )
        .unwrap();
        let output = dir.join("package");
        let mock = fixture_server(&board, mode);
        let handler = McpHandler::new(ServerConfig {
            kicad_cli: cli.display().to_string(),
            kicad_binary: String::new(),
            ipc_address: mock.address().into(),
            project_dir: None,
            jlcpcb_db_path: None,
            auto_load_toolsets: false,
            eager_toolsets: true,
        })
        .await
        .unwrap();
        let response = handler
            .handle_message(serde_json::json!({
                "jsonrpc": "2.0", "id": 69, "method": "tools/call",
                "params": {"name": "export_manufacturing_package", "arguments": {
                    "board": board, "output_dir": output, "fab_house": "jlcpcb",
                    "schematic": board.with_extension("kicad_sch"),
                    "jlcpcb_cpl_corrections_path": policy, "limit": 1
                }}
            }))
            .await
            .unwrap();
        let result = response.result.unwrap();
        let text = result["content"][0]["text"].as_str().unwrap().to_string();
        (text, output)
    }

    /// #69: the package writes its whole placement evidence beside the CPL
    /// and its response carries the counts and the first `limit` applied
    /// corrections, the verdict first.
    #[cfg(unix)]
    #[tokio::test]
    async fn served_jlcpcb_package_writes_its_evidence_and_summarises_it() {
        let dir = tempfile::tempdir().unwrap();
        let (text, output) = served_package(dir.path(), "midpoints").await;
        assert!(
            text.starts_with(r#"{"complete":true,"summary":"Complete package"#),
            "{text}"
        );
        let body: serde_json::Value = serde_json::from_str(&text).unwrap();
        let orientation = &body["placement_orientation"];
        assert_eq!(orientation["status"], "PREVIEW_REQUIRED", "{body:#}");
        assert_eq!(orientation["geometry_count"], 11);
        assert_eq!(orientation["applied_corrections_count"], 10);
        assert_eq!(orientation["applied_corrections_shown"], 1);
        assert_eq!(orientation["applied_corrections_truncated"], true);
        assert_eq!(orientation["unmatched_footprints_count"], 1);
        assert_eq!(orientation["unverified_footprints_count"], 0);
        for list in ["geometry", "unmatched_footprints", "unverified_footprints"] {
            assert!(orientation.get(list).is_none(), "{list}: {orientation:#}");
        }
        let pick_and_place = body["files_generated"]
            .as_array()
            .unwrap()
            .iter()
            .find(|file| file["type"] == "pick_and_place")
            .unwrap();
        assert!(
            pick_and_place.get("placement_orientation").is_none(),
            "{pick_and_place}"
        );

        let file = output.join("CPL-midpoints.orientation.json");
        assert_eq!(orientation["evidence_path"], file.to_str().unwrap());
        let uploads = body["files"].as_array().unwrap();
        assert!(
            uploads.contains(&serde_json::json!("CPL-midpoints.csv")),
            "{uploads:?}"
        );
        assert!(
            !uploads
                .iter()
                .any(|path| path.as_str().unwrap().ends_with(".json")),
            "{uploads:?}"
        );
        let evidence: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&file).unwrap()).unwrap();
        assert_eq!(evidence["geometry"].as_array().unwrap().len(), 11);
        assert_eq!(
            evidence["applied_corrections"].as_array().unwrap().len(),
            10
        );
        assert_eq!(evidence["unmatched_footprints"][0]["designator"], "R1");
        assert_eq!(
            evidence["applied_corrections"][0],
            orientation["applied_corrections"][0]
        );
        assert_eq!(evidence["policies"], orientation["policies"]);
    }

    /// #681: parts fitted by hand leave the BOM as they leave the CPL, so
    /// the package is complete, and get a list of their own for purchase,
    /// which is not uploaded.
    #[cfg(unix)]
    #[tokio::test]
    async fn served_jlcpcb_package_lists_hand_fitted_parts_apart() {
        let dir = tempfile::tempdir().unwrap();
        let (text, output) = served_package_in(dir.path(), "midpoints", "hand-fitted").await;
        let body: serde_json::Value = serde_json::from_str(&text).unwrap();
        assert_eq!(body["complete"], true, "{body:#}");
        assert_eq!(body["placement_orientation"]["geometry_count"], 9);
        let hand = body["files_generated"]
            .as_array()
            .unwrap()
            .iter()
            .find(|file| file["type"] == "bom_hand_fitted")
            .unwrap_or_else(|| panic!("{body:#}"));
        assert_eq!(hand["designators"], serde_json::json!(["JF3", "JF4"]));
        let path = output.join("BOM-hand-fitted-midpoints.csv");
        assert_eq!(hand["path"], path.to_str().unwrap());
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            "\"Reference\"\n\"JF3,JF4\"\n"
        );
        assert_eq!(
            std::fs::read_to_string(output.join("BOM-midpoints.csv")).unwrap(),
            "\"Reference\"\n\"JB0,JB1,JB2,JB3,JB4,JF0,JF1,JF2\"\n\"R1\"\n"
        );
        let uploads = body["files"].as_array().unwrap();
        assert!(
            uploads.contains(&serde_json::json!("BOM-midpoints.csv")),
            "{uploads:?}"
        );
        assert!(
            !uploads.contains(&serde_json::json!("BOM-hand-fitted-midpoints.csv")),
            "{uploads:?}"
        );

        // Without a hand-fitted part, the BOM is KiCad's, byte for byte.
        let dir = tempfile::tempdir().unwrap();
        let (text, output) = served_package(dir.path(), "midpoints").await;
        assert!(!text.contains("bom_hand_fitted"), "{text}");
        assert!(!output.join("BOM-hand-fitted-midpoints.csv").exists());
        assert_eq!(
            std::fs::read_to_string(output.join("BOM-midpoints.csv")).unwrap(),
            "Reference\n\"JB0,JB1,JB2,JB3,JB4,JF0,JF1,JF2,JF3,JF4\"\nR1\n"
        );
    }

    /// #69: evidence that cannot be written leaves a warning, no path and
    /// the package incomplete; the CPL and the counts stay. The evidence's
    /// name is 13 bytes longer than the CPL's, so a 236-byte project name
    /// puts it alone past the 255-byte file-name limit.
    #[cfg(unix)]
    #[tokio::test]
    async fn served_jlcpcb_package_warns_when_its_evidence_cannot_be_written() {
        let dir = tempfile::tempdir().unwrap();
        let stem = "m".repeat(236);
        let (text, output) = served_package(dir.path(), &stem).await;
        let body: serde_json::Value = serde_json::from_str(&text).unwrap();
        assert_eq!(body["complete"], false, "{body:#}");
        // The one warning, so it alone makes the package incomplete.
        let warnings = body["warnings"].as_array().unwrap();
        assert_eq!(warnings.len(), 1, "{warnings:#?}");
        assert!(
            warnings[0]
                .as_str()
                .unwrap()
                .starts_with("Placement orientation evidence was not written"),
            "{warnings:#?}"
        );
        let orientation = &body["placement_orientation"];
        assert!(orientation["evidence_path"].is_null(), "{orientation:#}");
        assert_eq!(orientation["geometry_count"], 11);
        assert!(
            body["files"]
                .as_array()
                .unwrap()
                .contains(&serde_json::json!(format!("CPL-{stem}.csv"))),
            "{body:#}"
        );
        assert!(!output.join(format!("CPL-{stem}.orientation.json")).exists());
    }

    #[test]
    fn native_union_uses_board_boxes_and_integer_nanometres() {
        let boxes = [
            box_at(-1_000_000, -2_000_000, 2_000_000, 4_000_000),
            box_at(3_000_000, 6_000_000, 2_000_000, 2_000_000),
        ];
        assert_eq!(union_center(boxes.iter()).unwrap(), (2.0, 3.0));
        assert_eq!(
            union_center([box_at(-5, -3, 5, 5)].iter()).unwrap(),
            (-0.000003, -0.000001)
        );
        assert!(union_center([box_at(0, 0, -1, 1)].iter()).is_err());
        assert!(union_center([].iter()).is_err());
    }

    #[test]
    fn geometry_conversion_keeps_centred_values_and_refuses_missing_or_wrong_source() {
        let midpoint = Midpoint {
            designator: "J1".into(),
            anchor_x_mm: 15.0,
            anchor_y_mm: 6.0,
            midpoint_x_mm: 16.25,
            midpoint_y_mm: 6.0,
            algorithm: "kicad_native_board_pad_boxes_union_center",
            pad_count: 2,
            pin1_mm: None,
            smd: true,
        };
        let mut points = BTreeMap::from([("J1".into(), midpoint.clone())]);
        let source = "Ref,Val,Package,PosX,PosY,Rot,Side\nJ1,X,JST,15.000000,-6.000000,0,top\n";
        let (converted, evidence) = convert_positions(source, &points).unwrap();
        assert!(converted.contains("16.250000,-6.000000"));
        assert_eq!(evidence[0].anchor_x_mm, 15.0);
        points.get_mut("J1").unwrap().midpoint_x_mm = 15.0;
        assert_eq!(convert_positions(source, &points).unwrap().0, source);
        assert!(convert_positions(source, &BTreeMap::new()).is_err());
        assert!(convert_positions(&source.replace("15.000000", "14.000000"), &points).is_err());
    }

    #[test]
    fn native_box_ids_must_be_complete_unique_and_exact() {
        for mode in ["missing", "duplicate", "unexpected", "negative", "good"] {
            let mock = MockIpcServer::spawn("midpoint-boxes", move |request| {
                let command = request.message.unwrap();
                let requested =
                    kiapi::common::commands::GetBoundingBox::decode(command.value.as_slice())
                        .unwrap();
                assert_eq!(
                    requested
                        .items
                        .iter()
                        .map(|id| id.value.as_str())
                        .collect::<Vec<_>>(),
                    ["p1", "p2"]
                );
                let (ids, boxes) = match mode {
                    "missing" => (vec!["p1"], vec![box_at(0, 0, 1, 1)]),
                    "duplicate" => (vec!["p1", "p1"], vec![box_at(0, 0, 1, 1); 2]),
                    "unexpected" => (vec!["p1", "other"], vec![box_at(0, 0, 1, 1); 2]),
                    "negative" => (vec!["p1", "p2"], vec![box_at(0, 0, -1, 1); 2]),
                    _ => (
                        vec!["p2", "p1"],
                        vec![box_at(4, 0, 2, 2), box_at(0, 0, 2, 2)],
                    ),
                };
                kiapi::common::ApiResponse {
                    status: Some(kiapi::common::ApiResponseStatus {
                        status: kiapi::common::ApiStatusCode::AsOk as i32,
                        error_message: String::new(),
                    }),
                    message: Some(pack_any(
                        &kiapi::common::commands::GetBoundingBoxResponse {
                            items: ids
                                .into_iter()
                                .map(|value| kiapi::common::types::Kiid {
                                    value: value.into(),
                                })
                                .collect(),
                            boxes,
                        },
                        "kiapi.common.commands.GetBoundingBoxResponse",
                    )),
                    ..Default::default()
                }
            });
            let client = KiCadIpcClient::new(mock.address());
            let result = client.get_item_boxes_in(Default::default(), &["p1".into(), "p2".into()]);
            assert_eq!(result.is_ok(), mode == "good", "{mode}: {result:?}");
        }
    }

    #[tokio::test]
    #[ignore = "requires the generated midpoint fixture open in KiCad IPC and kicad-cli"]
    async fn live_native_midpoint_acceptance() {
        let board = std::path::PathBuf::from(crate::app_dirs::env_var("MIDPOINT_BOARD").unwrap());
        let client = KiCadIpcClient::new(copperpour_ipc::detect_ipc_address().unwrap());
        let document = client.find_open_board(&board).unwrap();
        let snapshot = capture(&client, document.clone()).unwrap();
        let expected = [
            (20.0, 21.27),
            (31.27, 20.0),
            (40.0, 18.73),
            (48.73, 20.0),
            (60.721984, 20.721984),
        ];
        assert_eq!(snapshot.midpoints.len(), 11);
        for side in ["F", "B"] {
            for (i, &(x, y)) in expected.iter().enumerate() {
                let point = &snapshot.midpoints[&format!("J{side}{i}")];
                assert_eq!((point.midpoint_x_mm, point.midpoint_y_mm), (x, y));
            }
        }
        let dir = tempfile::tempdir().unwrap();
        let staged = dir.path().join("midpoints.kicad_pcb");
        std::fs::write(&staged, &snapshot.contents).unwrap();
        let output = dir.path().join("CPL.csv");
        let cli = std::env::var("KICAD_CLI_PATH").unwrap_or_else(|_| "kicad-cli".into());
        let export = super::super::export_jlcpcb_cpl(
            &cli,
            &staged,
            &output,
            "both",
            None,
            &snapshot.midpoints,
        )
        .await
        .unwrap();
        let mut reader = csv::Reader::from_reader(export.bytes.as_slice());
        assert_eq!(reader.records().count(), 11);
        let mut reader = csv::Reader::from_reader(export.bytes.as_slice());
        for row in reader.records() {
            let row = row.unwrap();
            let reference = &row[0];
            if reference == "R1" {
                assert_eq!(&row[1], "80.000000");
                assert_eq!(&row[2], "-20.000000");
                continue;
            }
            let i: usize = reference[2..].parse().unwrap();
            assert_eq!(row[1].parse::<f64>().unwrap(), expected[i].0);
            assert_eq!(row[2].parse::<f64>().unwrap(), -expected[i].1);
            assert_eq!(
                &row[3],
                if reference.starts_with("JF") {
                    "top"
                } else {
                    "bottom"
                }
            );
        }
        let published = std::fs::read(&output).unwrap();
        let mut incomplete = snapshot.midpoints.clone();
        incomplete.remove("JF0");
        assert!(
            super::super::export_jlcpcb_cpl(&cli, &staged, &output, "both", None, &incomplete)
                .await
                .is_err()
        );
        assert_eq!(
            std::fs::read(&output).unwrap(),
            published,
            "a failed conversion must preserve the previously published CPL"
        );
        // Capture real native protobuf fixtures for deterministic replay in CI.
        if let Some(destination) = crate::app_dirs::env_var("MIDPOINT_CAPTURE_DIR") {
            let destination = std::path::PathBuf::from(destination);
            std::fs::create_dir_all(&destination).unwrap();
            std::fs::write(destination.join("midpoints.kicad_pcb"), snapshot.contents).unwrap();
            let items = client
                .get_items_in(
                    document.clone(),
                    kiapi::common::types::KiCadObjectType::KotPcbFootprint,
                )
                .unwrap();
            let mut ids = Vec::new();
            for item in &items {
                let fp =
                    kiapi::board::types::FootprintInstance::decode(item.value.as_slice()).unwrap();
                for item in fp.definition.unwrap().items {
                    if item.type_url.ends_with("kiapi.board.types.Pad") {
                        ids.push(
                            kiapi::board::types::Pad::decode(item.value.as_slice())
                                .unwrap()
                                .id
                                .unwrap()
                                .value,
                        );
                    }
                }
            }
            let boxes = client.get_item_boxes_in(document, &ids).unwrap();
            let response = kiapi::common::commands::GetBoundingBoxResponse {
                items: boxes
                    .keys()
                    .map(|value| kiapi::common::types::Kiid {
                        value: value.clone(),
                    })
                    .collect(),
                boxes: boxes.into_values().collect(),
            };
            std::fs::write(destination.join("boxes.pb"), response.encode_to_vec()).unwrap();
            let response = kiapi::common::commands::GetItemsResponse {
                status: kiapi::common::types::ItemRequestStatus::IrsOk as i32,
                items,
                ..Default::default()
            };
            std::fs::write(destination.join("footprints.pb"), response.encode_to_vec()).unwrap();
        }
    }

    #[test]
    #[ignore = "requires disposable sole open COPPERPOUR_MIDPOINT_BOARD and COPPERPOUR_MIDPOINT_LIBRARY; KiCad IPC enabled"]
    fn live_native_midpoint_fixture() {
        let board = std::path::PathBuf::from(crate::app_dirs::env_var("MIDPOINT_BOARD").unwrap());
        let library =
            std::path::PathBuf::from(crate::app_dirs::env_var("MIDPOINT_LIBRARY").unwrap());
        let client = KiCadIpcClient::new(copperpour_ipc::detect_ipc_address().unwrap());
        let document = client.find_open_board(&board).unwrap();
        assert_eq!(client.get_open_documents().unwrap(), vec![document.clone()]);
        let source = std::fs::read_to_string(library).unwrap();
        let pads = crate::tools::pcb_components::extract_pad_definitions(&source).unwrap();
        let graphics = crate::tools::pcb_components::extract_graphic_definitions(&source).unwrap();
        let fields = crate::tools::pcb_components::extract_field_placement(&source);
        let mut creates = Vec::new();
        for (i, angle) in [0.0, 90.0, 180.0, 270.0, 45.0].iter().enumerate() {
            for (side, layer) in [("F", "F.Cu"), ("B", "B.Cu")] {
                creates.push(
                    KiCadIpcClient::build_footprint_item(
                        "Connector_PinHeader_2.54mm:PinHeader_1x02_P2.54mm_Vertical",
                        &format!("J{side}{i}"),
                        "header",
                        &pads,
                        &graphics,
                        &fields,
                        20.0 + i as f64 * 10.0,
                        20.0,
                        *angle,
                        layer,
                    )
                    .unwrap(),
                );
            }
        }
        let centered =
            std::fs::read_to_string(crate::app_dirs::env_var("MIDPOINT_CENTERED_LIBRARY").unwrap())
                .unwrap();
        creates.push(
            KiCadIpcClient::build_footprint_item(
                "Resistor_SMD:R_0805_2012Metric",
                "R1",
                "10k",
                &crate::tools::pcb_components::extract_pad_definitions(&centered).unwrap(),
                &crate::tools::pcb_components::extract_graphic_definitions(&centered).unwrap(),
                &crate::tools::pcb_components::extract_field_placement(&centered),
                80.0,
                20.0,
                0.0,
                "F.Cu",
            )
            .unwrap(),
        );
        client
            .run_commit("midpoint acceptance fixture", |client| {
                client.create_items_in(document.clone(), creates)
            })
            .unwrap();
        let snapshot = capture(&client, document).unwrap();
        // Persist only to the explicitly disposable test file, not a user design.
        std::fs::write(&board, &snapshot.contents).unwrap();
        eprintln!(
            "{}",
            serde_json::to_string_pretty(&snapshot.midpoints).unwrap()
        );
    }
}
