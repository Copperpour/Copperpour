//! KiCad 10.0.8's captured `GetItems` answer for `teardrops_kicad10.kicad_pcb`
//! decodes to the teardrop settings that board's file declares (#555). The
//! board and the capture are both KiCad's output; the fixture's README says
//! how each was made. No KiCad required.

use copperpour_ipc::builders;
use copperpour_ipc::client::{IpcTeardropTargetKind, KiCadIpcClient};
use copperpour_ipc::gen::kiapi;
use prost::Message;
use std::path::Path;

mod mock_kicad;
use mock_kicad::spawn_mock;

mod teardrop_common;
use teardrop_common::{file_items, settings_in_file};

const CAPTURE: &[u8] = include_bytes!("fixtures/teardrops_kicad10.ipc.bin");
const BOARD: &str = include_str!("fixtures/teardrops_kicad10.kicad_pcb");

fn reply(inner: prost_types::Any) -> kiapi::common::ApiResponse {
    kiapi::common::ApiResponse {
        status: Some(kiapi::common::ApiResponseStatus {
            status: kiapi::common::ApiStatusCode::AsOk as i32,
            error_message: String::new(),
        }),
        header: None,
        message: Some(inner),
    }
}

fn document() -> kiapi::common::types::DocumentSpecifier {
    kiapi::common::types::DocumentSpecifier {
        r#type: kiapi::common::types::DocumentType::DoctypePcb as i32,
        project: None,
        identifier: Some(
            kiapi::common::types::document_specifier::Identifier::BoardFilename(
                "teardrops_kicad10.kicad_pcb".to_string(),
            ),
        ),
    }
}

#[test]
fn kicads_captured_teardrop_settings_are_what_its_board_file_declares() {
    // The capture answers the pad-and-via read; the footprint read that
    // names each pad's owner gets no footprints, so no references.
    let server = spawn_mock(move |request| {
        let command = request.message.expect("a command");
        assert!(
            command.type_url.ends_with("GetItems"),
            "{}",
            command.type_url
        );
        let get = kiapi::common::commands::GetItems::decode(command.value.as_slice()).unwrap();
        let pads = kiapi::common::types::KiCadObjectType::KotPcbPad as i32;
        if get.types.contains(&pads) {
            return Some(reply(prost_types::Any {
                type_url: "type.googleapis.com/kiapi.common.commands.GetItemsResponse".into(),
                value: CAPTURE.to_vec(),
            }));
        }
        Some(reply(builders::pack_any(
            &kiapi::common::commands::GetItemsResponse {
                header: None,
                status: kiapi::common::types::ItemRequestStatus::IrsOk as i32,
                items: vec![],
            },
            "kiapi.common.commands.GetItemsResponse",
        )))
    });
    let client = KiCadIpcClient::new(&server.url);

    let targets = client
        .get_teardrop_targets_in(document())
        .expect("the capture decodes");

    let tree = copperpour_sexp::parse_sexp(BOARD).unwrap();
    let in_file = file_items(&tree);
    assert_eq!(targets.len(), in_file.len(), "one target per pad and via");
    for target in &targets {
        let (_, item) = in_file
            .iter()
            .find(|(id, _)| *id == target.uuid)
            .unwrap_or_else(|| panic!("{} is not in the board file", target.uuid));
        assert_eq!(
            target.settings,
            settings_in_file(item),
            "{:?} {} ({:?})",
            target.kind,
            target.uuid,
            target.pad_number
        );
    }
    // What makes this capture worth having: both forms KiCad writes, every
    // field away from its default on the enabled ones, a via and pads.
    let enabled: Vec<_> = targets.iter().filter(|t| t.settings.enabled).collect();
    assert_eq!(enabled.len(), 4);
    assert!(enabled.iter().all(|t| t.net == "GND"));
    assert!(enabled.iter().any(|t| t.kind == IpcTeardropTargetKind::Via));
    let via = enabled
        .iter()
        .find(|t| t.kind == IpcTeardropTargetKind::Via)
        .unwrap();
    assert!(via.settings.curved_edges);
    assert!(!via.settings.allow_multiple_track_segments);
    assert!(!via.settings.prefer_zone_connection);
    assert_eq!(via.settings.max_length_nm, Some(1_500_000));
    assert_eq!(via.settings.max_width_nm, None);
    assert_eq!(via.settings.best_length_ratio, 0.6);
    assert_eq!(via.settings.best_width_ratio, 0.9);
    assert_eq!(via.settings.max_track_width_ratio, 0.8);
    assert!(targets.iter().any(|t| !t.settings.enabled));
    // Every pad names its footprint, which a whole-footprint update needs.
    assert!(targets
        .iter()
        .filter(|t| t.kind == IpcTeardropTargetKind::Pad)
        .all(|t| t.footprint_uuid.is_some()));
}

/// KiCad 10.0.7 and later always send both length limits, as 0 when there is none
/// (`PackTeardropSettings`), and `TEARDROP_PARAMETERS` treats `<= 0` as no
/// constraint. The capture holds such a 0.
#[test]
fn a_zero_length_limit_in_the_capture_is_no_constraint() {
    let response = kiapi::common::commands::GetItemsResponse::decode(CAPTURE).unwrap();
    let via = response
        .items
        .iter()
        .find(|item| builders::any_is(item, "kiapi.board.types.Via"))
        .unwrap();
    let via = kiapi::board::types::Via::decode(via.value.as_slice()).unwrap();
    let teardrop = via.teardrop.expect("10.0.7 and later send the settings");
    assert_eq!(teardrop.max_width.map(|d| d.value_nm), Some(0));
    let targets =
        copperpour_ipc::client::teardrop_targets_from_items(&response.items, &Default::default())
            .unwrap();
    let decoded = targets
        .iter()
        .find(|t| t.kind == IpcTeardropTargetKind::Via)
        .unwrap();
    assert_eq!(decoded.settings.max_width_nm, None);
}

/// The fixture board is KiCad's own output, as the README says.
#[test]
fn the_fixture_board_is_kicads_output_with_its_teardrop_zones() {
    assert!(BOARD.starts_with("(kicad_pcb\n\t(version 20260206)\n\t(generator \"pcbnew\")"));
    let tree = copperpour_sexp::parse_sexp(BOARD).unwrap();
    let teardrop_zones = tree
        .find_all("zone")
        .into_iter()
        .filter(|zone| {
            zone.find("attr")
                .is_some_and(|attr| attr.find("teardrop").is_some())
        })
        .count();
    assert_eq!(teardrop_zones, 3);
    assert!(Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/teardrops_kicad10.README.md")
        .exists());
}
