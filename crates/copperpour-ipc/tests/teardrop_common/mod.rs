//! KiCad's file form of pad and via teardrop settings, read independently of
//! the IPC decoder, for the teardrop tests.

#![allow(dead_code)]

use copperpour_ipc::client::IpcTeardropSettings;
use copperpour_sexp::{parse_sexp, SexpNode};

/// KiCad's own file form of a pad's or via's settings, read independently of
/// the IPC decoder. No block is KiCad's default (`isDefaultTeardropParameters`).
pub fn settings_in_file(item: &SexpNode) -> IpcTeardropSettings {
    let Some(block) = item.find("teardrops") else {
        return IpcTeardropSettings {
            enabled: false,
            curved_edges: false,
            allow_multiple_track_segments: true,
            prefer_zone_connection: true,
            max_length_nm: Some(1_000_000),
            max_width_nm: Some(2_000_000),
            best_length_ratio: 0.5,
            best_width_ratio: 1.0,
            max_track_width_ratio: 0.9,
        };
    };
    let flag = |name: &str| block.find_str(name) == Some("yes");
    let number = |name: &str| {
        block
            .find_f64(name)
            .unwrap_or_else(|| panic!("teardrops block lacks {name}"))
    };
    let length = |name: &str| {
        let nm = (number(name) * 1_000_000.0).round() as i64;
        (nm > 0).then_some(nm)
    };
    IpcTeardropSettings {
        enabled: flag("enabled"),
        curved_edges: flag("curved_edges"),
        allow_multiple_track_segments: flag("allow_two_segments"),
        prefer_zone_connection: flag("prefer_zone_connections"),
        max_length_nm: length("max_length"),
        max_width_nm: length("max_width"),
        best_length_ratio: number("best_length_ratio"),
        best_width_ratio: number("best_width_ratio"),
        max_track_width_ratio: number("filter_ratio"),
    }
}

/// Every pad and via in KiCad's serialization, by UUID.
pub fn file_items(tree: &SexpNode) -> Vec<(String, SexpNode)> {
    let mut items = Vec::new();
    for via in tree.find_all("via") {
        items.push((via.find_str("uuid").unwrap().to_string(), via.clone()));
    }
    for footprint in tree.find_all("footprint") {
        for pad in footprint.find_all("pad") {
            items.push((pad.find_str("uuid").unwrap().to_string(), pad.clone()));
        }
    }
    items
}

pub fn file_settings(text: &str, uuid: &str) -> IpcTeardropSettings {
    let tree = parse_sexp(text).expect("KiCad's board text did not parse");
    let (_, item) = file_items(&tree)
        .into_iter()
        .find(|(id, _)| id == uuid)
        .unwrap_or_else(|| panic!("{uuid} is not in KiCad's board text"));
    settings_in_file(&item)
}
