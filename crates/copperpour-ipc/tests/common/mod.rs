//! A board file's own `(stackup …)` block, which KiCad's `GetBoardStackup`
//! answer is checked against (#716). Shared by the mock-server test, which
//! replays KiCad's captured answer, and the live test, which asks KiCad.

use copperpour_ipc::IpcBoardStackup;
use copperpour_sexp::{parse_sexp, SexpNode};
use std::path::Path;

/// Assert that `stackup` is the one `board`'s file declares: every entry in
/// order, with its layer or dielectric type, its declared thickness and a
/// dielectric's material, εr and loss tangent; then the finish, impedance
/// control and edge settings, each as KiCad writes it to the file.
pub fn assert_matches_file(stackup: &IpcBoardStackup, board: &Path) {
    let source = std::fs::read_to_string(board).expect("failed to read the board file");
    let tree = parse_sexp(&source).expect("failed to parse the board file");
    let declared = tree
        .find("setup")
        .and_then(|setup| setup.find("stackup"))
        .expect("the board declares no (stackup …) in its setup");
    let entries = declared.find_all("layer");

    let nm = |mm: f64| (mm * 1_000_000.0).round() as i64;
    assert_eq!(
        stackup.layers.len(),
        entries.len(),
        "KiCad reports {:?}",
        stackup.layers
    );
    for (served, file) in stackup.layers.iter().zip(&entries) {
        let name = file.get(1).and_then(SexpNode::as_str).unwrap();
        let kind = file.find_str("type").unwrap_or_default();
        if name.starts_with("dielectric") {
            assert_eq!(served.kind, "dielectric", "{name}: {served:?}");
            assert_eq!(served.layer, None, "{name}: {served:?}");
            assert_eq!(served.dielectric_type.as_deref(), Some(kind), "{name}");
            let sub = served
                .dielectric
                .first()
                .expect("a dielectric with no sub-layer");
            assert_eq!(sub.material, file.find_str("material").unwrap(), "{name}");
            assert_eq!(sub.epsilon_r, file.find_f64("epsilon_r").unwrap(), "{name}");
            assert_eq!(
                sub.loss_tangent,
                file.find_f64("loss_tangent").unwrap(),
                "{name}"
            );
        } else {
            assert_eq!(served.layer.as_deref(), Some(name), "{served:?}");
            let expected = match kind {
                "copper" => "copper",
                k if k.contains("Mask") => "soldermask",
                k if k.contains("Silk") => "silkscreen",
                k if k.contains("Paste") => "solderpaste",
                other => panic!("{name}: a stackup entry of type {other:?}"),
            };
            assert_eq!(served.kind, expected, "{name}");
        }
        if let Some(thickness) = file.find_f64("thickness") {
            assert_eq!(served.thickness_nm, nm(thickness), "{name}");
        }
    }

    // KiCad writes the finish only when one is set, `dielectric_constraints`
    // always, and the edge settings only when they are on.
    if let Some(finish) = declared.find_str("copper_finish") {
        assert_eq!(stackup.finish, finish);
    }
    let yes = |key: &str| declared.find_str(key) == Some("yes");
    assert_eq!(stackup.impedance_controlled, yes("dielectric_constraints"));
    assert_eq!(stackup.has_edge_plating, yes("edge_plating"));
    let connector = match declared.find_str("edge_connector") {
        None => "none",
        Some("yes") => "plain",
        Some("bevelled") => "beveled",
        Some(other) => panic!("an edge connector of {other:?}"),
    };
    assert_eq!(stackup.edge_connector, connector);
}
