use super::board::{exact_rule_area_um, pin_rotation, roundrect_corner_radius_um};
use super::native::{
    dsn_identity, normalize_courtyard_outline_polygons, parse_dsn_sexp, polygons_trace_roundrect,
    validate_dsn_syntax, DsnShapeIdentity, OrderedF64,
};
use super::outline::outline_arc_points;
use super::writer::{oval_spine, roundrect_outline};
use super::*;
use copperpour_ipc::IpcRoutingRules;
use specctra::read::{ListTokenizer, ReadDsn};
use specctra::structure as dsn;
use std::io::{BufReader, Cursor};

fn rules() -> IpcEffectiveRoutingRules {
    ["GND", "VCC"]
        .into_iter()
        .map(|net| {
            (
                net.to_string(),
                IpcRoutingRules {
                    class_name: "Default".to_string(),
                    constituents: vec!["Default".to_string()],
                    track_width_mm: Some(0.25),
                    clearance_mm: Some(0.2),
                    via_diameter_mm: Some(0.6),
                    via_drill_mm: Some(0.3),
                },
            )
        })
        .collect()
}

fn native_fixture_rules() -> IpcEffectiveRoutingRules {
    let mut rules = rules();
    for rule in rules.values_mut() {
        rule.track_width_mm = Some(0.2);
    }
    rules
}

const STOCK_ROUNDRECT: &str =
    include_str!("../../tests/fixtures/specctra_stock_roundrect.kicad_pcb");
const STOCK_ROUNDRECT_NATIVE: &str =
    include_str!("../../tests/fixtures/specctra_stock_roundrect.native-kicad-10.dsn");

/// KiCad's default netclass, which every net of the stock fixture uses.
fn stock_rules() -> IpcEffectiveRoutingRules {
    let mut rules = native_fixture_rules();
    let mut sig = rules["VCC"].clone();
    sig.class_name = "Default".to_string();
    rules.insert("SIG".to_string(), sig);
    rules
}

fn stock_roundrect_native() -> String {
    STOCK_ROUNDRECT_NATIVE.to_string()
}

fn stock_export() -> ExportBundle {
    export_dsn(
        Path::new("board.kicad_pcb"),
        STOCK_ROUNDRECT,
        &stock_rules(),
    )
    .unwrap()
}

fn manifest_pads(manifest: &str) -> Vec<ManifestPadstack> {
    let manifest: Manifest = serde_json::from_str(manifest).unwrap();
    manifest
        .padstacks
        .into_iter()
        .filter(|padstack| padstack.purpose == "pad")
        .collect()
}

#[test]
fn stock_roundrect_pads_export_as_polygons() {
    let export = stock_export();

    // Pad sizes and radii KiCad 10.0.6 named in its own export of this
    // board (`RoundRect[T]Pad_<w>x<h>_<r + inflation>_um`), see the README.
    let pads = manifest_pads(&export.manifest)
        .iter()
        .map(|pad| {
            (
                pad.shape,
                pad.size_x_um,
                pad.size_y_um,
                pad.corner_radius_um,
            )
        })
        .collect::<BTreeSet<_>>();
    assert_eq!(
        pads,
        BTreeSet::from([
            (PadShape::RoundRect, 540, 640, Some(135)),
            (PadShape::RoundRect, 800, 950, Some(200)),
            (PadShape::RoundRect, 900, 950, Some(225)),
        ])
    );
    let manifest: serde_json::Value = serde_json::from_str(&export.manifest).unwrap();
    assert_eq!(
        manifest["supported_profile"]["pad_shapes"],
        serde_json::json!(["circle", "rect", "roundrect", "oval"])
    );
    assert_eq!(export.pad_count, 6);
    assert!(export.dsn.contains("(polygon F.Cu 0"), "{}", export.dsn);
}

#[test]
fn roundrect_outline_encloses_the_pad_within_its_error() {
    for (size_x, size_y, radius) in [(540, 640, 135), (1025, 1400, 250), (600, 600, 300)] {
        let outline = roundrect_outline(size_x, size_y, radius);
        let model = ManifestPadstack {
            name: String::new(),
            purpose: "pad".to_string(),
            shape: PadShape::RoundRect,
            layers: vec!["F.Cu".to_string()],
            size_x_um: size_x,
            size_y_um: size_y,
            corner_radius_um: Some(radius),
            drill_um: None,
        };
        let (half_x, half_y, r) = (size_x as f64 / 2.0, size_y as f64 / 2.0, radius as f64);
        // Every point of the copper outline is inside the polygon, up to
        // the 0.05 um the DSN's resolution can move a vertex.
        for step in 0..3600 {
            let angle = f64::from(step).to_radians() / 10.0;
            let (cos, sin) = (angle.cos(), angle.sin());
            let copper = (
                (half_x - r).copysign(cos) + r * cos,
                (half_y - r).copysign(sin) + r * sin,
            );
            let outside = outline.windows(2).any(|edge| {
                let (a, b) = (edge[0], edge[1]);
                let cross = (b.0 - a.0) * (copper.1 - a.1) - (b.1 - a.1) * (copper.0 - a.0);
                cross < -0.05 * (b.0 - a.0).hypot(b.1 - a.1)
            });
            assert!(!outside, "{copper:?} outside {size_x}x{size_y} r{radius}");
        }
        let shape = DsnShapeIdentity::Polygon {
            layer: "F.Cu".to_string(),
            points: outline
                .iter()
                .map(|(x, y)| (OrderedF64::new(*x).unwrap(), OrderedF64::new(*y).unwrap()))
                .collect(),
        };
        assert!(polygons_trace_roundrect(&[shape], &model));
    }
}

#[test]
fn native_kicad_roundrect_polygons_are_adopted() {
    let baseline = stock_export();
    let native = stock_roundrect_native();
    let adopted = adopt_native_dsn(baseline, native.clone()).unwrap();

    assert_eq!(adopted.dsn, native);
    let names = manifest_pads(&adopted.manifest)
        .into_iter()
        .map(|pad| (pad.name, pad.corner_radius_um))
        .collect::<Vec<_>>();
    assert_eq!(
        names,
        [
            (
                "RoundRect[T]Pad_540.000000x640.000000_135.514000_um_0.000000_0".to_string(),
                Some(135)
            ),
            (
                "RoundRect[T]Pad_800.000000x950.000000_200.761000_um_0.000000_0".to_string(),
                Some(200)
            ),
            (
                "RoundRect[T]Pad_900.000000x950.000000_225.856000_um_0.000000_0".to_string(),
                Some(225)
            ),
        ]
    );
}

const STOCK_HALF_UM: &str = include_str!("../../tests/fixtures/specctra_stock_half_um.kicad_pcb");
const STOCK_HALF_UM_NATIVE: &str =
    include_str!("../../tests/fixtures/specctra_stock_half_um.native-kicad-10.dsn");

#[test]
fn stock_half_um_native_dsn_with_courtyards_is_adopted() {
    let baseline = export_dsn(Path::new("board.kicad_pcb"), STOCK_HALF_UM, &stock_rules()).unwrap();
    assert!(baseline.dsn.contains(" -912.5 0)"), "{}", baseline.dsn);
    assert!(baseline.dsn.contains(" 912.5 0)"), "{}", baseline.dsn);

    let adopted = adopt_native_dsn(baseline, STOCK_HALF_UM_NATIVE.to_string()).unwrap();
    assert_eq!(adopted.dsn, STOCK_HALF_UM_NATIVE);
    assert!(manifest_pads(&adopted.manifest).iter().any(|pad| {
        pad.name == "RoundRect[T]Pad_1025.000000x1400.000000_250.951000_um_0.000000_0"
            && pad.corner_radius_um == Some(250)
    }));
}

#[test]
fn rotated_45_native_dsn_with_nanometre_pins_is_adopted() {
    let source = include_str!("../../tests/fixtures/specctra_rotated_footprints_45.kicad_pcb");
    let native =
        include_str!("../../tests/fixtures/specctra_rotated_footprints_45.native-kicad-10.dsn");
    let baseline = native_baseline(source);
    assert!(baseline.dsn.contains("-499.999 0)"), "{}", baseline.dsn);
    let adopted = adopt_native_dsn(baseline, native.to_string()).unwrap();
    assert_eq!(adopted.dsn, native);
}

#[test]
fn native_dsn_pin_shifted_past_resolution_tolerance_is_refused() {
    let baseline =
        || export_dsn(Path::new("board.kicad_pcb"), STOCK_HALF_UM, &stock_rules()).unwrap();
    for within in ["1 -912.6 0)", "1 -912.4 0)"] {
        let shifted = STOCK_HALF_UM_NATIVE.replacen("1 -912.5 0)", within, 1);
        assert_ne!(shifted, STOCK_HALF_UM_NATIVE);
        adopt_native_dsn(baseline(), shifted).unwrap();
    }
    let moved = STOCK_HALF_UM_NATIVE.replacen("1 -912.5 0)", "1 -912.7 0)", 1);
    assert_ne!(moved, STOCK_HALF_UM_NATIVE);
    let error = adopt_native_dsn(baseline(), moved).unwrap_err().to_string();
    assert!(
        error.contains("native DSN changed pin position for component 'R3' pin '1'"),
        "{error}"
    );
}

#[test]
fn invalid_courtyard_outline_polygon_still_fails_syntax_validation() {
    let baseline = export_dsn(Path::new("board.kicad_pcb"), STOCK_HALF_UM, &stock_rules()).unwrap();
    let broken = STOCK_HALF_UM_NATIVE.replacen(
        "(outline (polygon signal 0  1675 -945",
        "(outline (polygon signal 0  not_a_number -945",
        1,
    );
    assert_ne!(broken, STOCK_HALF_UM_NATIVE);
    let error = adopt_native_dsn(baseline, broken).unwrap_err().to_string();
    assert!(
        error.contains("failed Specctra parser validation"),
        "{error}"
    );
}

#[test]
fn multiline_courtyard_polygon_normalizes_outside_quotes_only() {
    let multiline = STOCK_HALF_UM_NATIVE.replacen(
        "(outline (polygon signal 0  1675 -945",
        "(outline\n        (polygon signal 0  1675 -945",
        1,
    );
    validate_dsn_syntax(&multiline, "multiline").unwrap();
    let quoted = r#"(pcb "board (outline (polygon x))" (string_quote "))"#;
    let normalized = normalize_courtyard_outline_polygons(quoted);
    assert_eq!(normalized, quoted);
}

/// Freerouting 2.3.0 routed both exports of the stock board completely;
/// the strict import planner accepts each against its own manifest. It
/// still accepts a `konnect_*` job written before the rename
/// (Copperpour/Copperpour#250), because SES names are checked against the
/// manifest, and refuses an SES paired with the other naming.
#[test]
fn freerouting_sessions_of_stock_roundrect_pads_pass_the_import_planner() {
    let temp = tempfile::tempdir().unwrap();
    let board_path = temp.path().join("specctra_stock_roundrect.kicad_pcb");
    std::fs::write(&board_path, STOCK_ROUNDRECT).unwrap();
    let rust = export_dsn(&board_path, STOCK_ROUNDRECT, &stock_rules()).unwrap();
    let native = adopt_native_dsn(rust.clone(), stock_roundrect_native()).unwrap();
    let mut legacy: serde_json::Value = serde_json::from_str(include_str!(
        "../../tests/fixtures/specctra_stock_roundrect.konnect-manifest.json"
    ))
    .unwrap();
    legacy["board_path"] = board_path.to_string_lossy().into();
    let legacy = legacy.to_string();
    let current_ses =
        include_str!("../../tests/fixtures/specctra_stock_roundrect.freerouting-2.3.0.ses");
    let legacy_ses =
        include_str!("../../tests/fixtures/specctra_stock_roundrect.konnect.freerouting-2.3.0.ses");
    for (manifest, ses) in [
        (&rust.manifest, current_ses),
        (&legacy, legacy_ses),
        (
            &native.manifest,
            include_str!(
                "../../tests/fixtures/specctra_stock_roundrect.native-kicad-10.freerouting-2.3.0.ses"
            ),
        ),
    ] {
        let plan =
            crate::specctra_ses::parse_import_plan(&board_path, STOCK_ROUNDRECT, manifest, ses)
                .unwrap();
        let nets = plan
            .tracks
            .iter()
            .map(|track| track.net_name.as_str())
            .collect::<BTreeSet<_>>();
        assert_eq!(nets, BTreeSet::from(["GND", "SIG", "VCC"]));
    }
    for (manifest, ses, expected) in [
        (&rust.manifest, legacy_ses, "copperpour_image_C1"),
        (&legacy, current_ses, "konnect_image_C1"),
    ] {
        let error =
            crate::specctra_ses::parse_import_plan(&board_path, STOCK_ROUNDRECT, manifest, ses)
                .unwrap_err()
                .to_string();
        assert!(
            error.contains(&format!("changed from '{expected}'")),
            "{error}"
        );
    }
}

/// KiCad 10.0.6 named the stock 0805 padstack (1025 x 1400 um,
/// `roundrect_rratio 0.243902`) `RoundRect[T]Pad_..._250.951000_um`:
/// radius 250 um plus 0.951 um of growth. A plain floor of 249.99955
/// would give 249.
#[test]
fn roundrect_corner_radius_follows_kicad_rounding() {
    let pad = |ratio: &str| {
        parse_sexp(&format!(
            "(pad \"1\" smd roundrect (size 1.025 1.4) (roundrect_rratio {ratio}))"
        ))
        .unwrap()
    };
    assert_eq!(
        roundrect_corner_radius_um(&pad("0.243902"), 1025, 1400, "R2-1").unwrap(),
        250
    );
    let error = roundrect_corner_radius_um(&pad("0.6"), 1025, 1400, "R2-1")
        .unwrap_err()
        .to_string();
    assert!(error.contains("outside 0..=0.5"), "{error}");
}

/// KiCad's own polygon for the 0402 pad traces that pad, and no pad a few
/// micrometres larger, smaller, or rounder.
#[test]
fn kicad_roundrect_polygon_traces_only_its_own_pad() {
    let native = dsn_identity(&parse_dsn_sexp(&stock_roundrect_native()).unwrap()).unwrap();
    let shapes =
        &native.padstacks["RoundRect[T]Pad_540.000000x640.000000_135.514000_um_0.000000_0"];
    let model = |size_x_um, size_y_um, corner_radius_um| ManifestPadstack {
        name: String::new(),
        purpose: "pad".to_string(),
        shape: PadShape::RoundRect,
        layers: vec!["F.Cu".to_string()],
        size_x_um,
        size_y_um,
        corner_radius_um: Some(corner_radius_um),
        drill_um: None,
    };
    assert!(polygons_trace_roundrect(shapes, &model(540, 640, 135)));
    assert!(!polygons_trace_roundrect(shapes, &model(550, 650, 137)));
    assert!(!polygons_trace_roundrect(shapes, &model(530, 630, 132)));
    // Within the arc error everywhere, but 1.5 um short of each side.
    assert!(!polygons_trace_roundrect(shapes, &model(544, 644, 135)));
    // KiCad's sharper corners stand about 4 um outside a 145 um corner.
    assert!(!polygons_trace_roundrect(shapes, &model(540, 640, 145)));
}

#[test]
fn native_roundrect_of_another_size_is_refused() {
    let baseline = stock_export();
    // R1's pins point at the 0603 padstack in the native file.
    let native = stock_roundrect_native();
    let moved = native.replace(
        "(pin RoundRect[T]Pad_540.000000x640.000000_135.514000_um_0.000000_0 1",
        "(pin RoundRect[T]Pad_900.000000x950.000000_225.856000_um_0.000000_0 1",
    );
    assert_ne!(moved, native);
    let error = adopt_native_dsn(baseline, moved).unwrap_err().to_string();
    assert!(
        error.contains("changed pad geometry for component 'R1' pin '1'"),
        "{error}"
    );
}

#[test]
fn chamfered_roundrect_pad_is_refused() {
    let source = STOCK_ROUNDRECT.replacen(
        "(roundrect_rratio 0.25)",
        "(roundrect_rratio 0.25)\n\t\t\t(chamfer_ratio 0.2)\n\t\t\t(chamfer top_left)",
        1,
    );
    assert_ne!(source, STOCK_ROUNDRECT);
    let error = export_dsn(Path::new("board.kicad_pcb"), &source, &stock_rules())
        .unwrap_err()
        .to_string();
    assert!(error.contains("chamfered corners"), "{error}");
}

fn native_baseline(source: &str) -> ExportBundle {
    export_dsn(
        Path::new("board.kicad_pcb"),
        source,
        &native_fixture_rules(),
    )
    .unwrap()
}

/// KiCad's stock JST SH `1MP` connector as J1: pad 1 on VCC, pad 2 and
/// both `MP` pads on GND (#338). See its README.
const DUPLICATE_MP: &str = include_str!("../../tests/fixtures/specctra_duplicate_mp.kicad_pcb");
/// The two `MP` pads' UUIDs, in the order that names them `MP` and `MP@1`.
const MP_UUIDS: [&str; 2] = [
    "550c489f-8422-4919-91a6-15d69b43a49a",
    "ce9e8222-11b6-4e53-9716-46236e235ef7",
];

fn export_board(source: &str) -> Result<ExportBundle> {
    export_dsn(Path::new("board.kicad_pcb"), source, &rules())
}

/// J1's manifest pins as (pad number, pad UUID, DSN pin, net).
fn j1_pins(bundle: &ExportBundle) -> Vec<(String, String, String, Option<String>)> {
    let manifest: Manifest = serde_json::from_str(&bundle.manifest).unwrap();
    manifest
        .components
        .into_iter()
        .find(|component| component.reference == "J1")
        .unwrap()
        .pads
        .into_iter()
        .map(|pin| (pin.pad_number, pin.pad_uuid.unwrap(), pin.dsn_pin, pin.net))
        .collect()
}

/// `source` with `edit` applied to the pad block holding `uuid`.
fn edit_pad(source: &str, uuid: &str, edit: impl Fn(&str) -> String) -> String {
    let at = source.find(uuid).unwrap();
    let start = source[..at].rfind("\t\t(pad ").unwrap();
    let end = at + source[at..].find("\n\t\t)").unwrap() + "\n\t\t)".len();
    format!(
        "{}{}{}",
        &source[..start],
        edit(&source[start..end]),
        &source[end..]
    )
}

fn pin(
    number: &str,
    uuid: &str,
    dsn: &str,
    net: Option<&str>,
) -> (String, String, String, Option<String>) {
    (
        number.into(),
        uuid.into(),
        dsn.into(),
        net.map(String::from),
    )
}

/// Two pads share `MP`, both on GND: each is its own DSN pin on GND,
/// named as KiCad's exporter names them, and the manifest records which
/// physical pad each name is (#338).
#[test]
fn repeated_mp_pads_export_as_two_pins_on_their_net() {
    let bundle = export_board(DUPLICATE_MP).unwrap();
    assert_eq!(bundle.pad_count, 8);
    assert!(
        bundle
            .dsn
            .contains("(pin copperpour_pad_0003 MP -1800 -1875)"),
        "{}",
        bundle.dsn
    );
    assert!(
        bundle
            .dsn
            .contains("(pin copperpour_pad_0003 MP@1 1800 -1875)"),
        "{}",
        bundle.dsn
    );
    assert!(
        bundle.dsn.contains("(pins J1-2 J1-MP J1-MP@1 R1-2 R2-1)"),
        "{}",
        bundle.dsn
    );
    let manifest: serde_json::Value = serde_json::from_str(&bundle.manifest).unwrap();
    assert_eq!(manifest["schema_version"], MANIFEST_SCHEMA_VERSION);
    assert_eq!(
        j1_pins(&bundle),
        [
            pin(
                "1",
                "95236974-fb55-4796-a22a-73703cb8f5cf",
                "J1-1",
                Some("VCC")
            ),
            pin(
                "2",
                "3078e2f7-2337-4b75-95e4-f648e3e3ed38",
                "J1-2",
                Some("GND")
            ),
            pin("MP", MP_UUIDS[0], "J1-MP", Some("GND")),
            pin("MP", MP_UUIDS[1], "J1-MP@1", Some("GND")),
        ]
    );
    // A number one pad has keeps its name: nothing changes there.
    let two = export_board(include_str!(
        "../../tests/fixtures/specctra_two_resistors.kicad_pcb"
    ))
    .unwrap();
    assert!(
        two.dsn.contains("(pins R1-2 R2-1)") || two.dsn.contains("(pins R1-1 R2-2)"),
        "{}",
        two.dsn
    );
    assert!(!two.dsn.contains('@'), "{}", two.dsn);
}

/// The names follow the pads' UUIDs, not the order the file lists them.
#[test]
fn repeated_pad_names_do_not_depend_on_pad_order() {
    let first = edit_pad(DUPLICATE_MP, MP_UUIDS[0], |block| block.to_string());
    let second_block = {
        let at = DUPLICATE_MP.find(MP_UUIDS[1]).unwrap();
        let start = DUPLICATE_MP[..at].rfind("\t\t(pad ").unwrap();
        let end = at + DUPLICATE_MP[at..].find("\n\t\t)").unwrap() + "\n\t\t)".len();
        DUPLICATE_MP[start..end].to_string()
    };
    // Move the second MP block in front of the first.
    let without = DUPLICATE_MP.replacen(&format!("\n{second_block}"), "", 1);
    let first_start = without
        .find(MP_UUIDS[0])
        .map(|at| without[..at].rfind("\t\t(pad ").unwrap())
        .unwrap();
    let swapped = format!(
        "{}{second_block}\n{}",
        &without[..first_start],
        &without[first_start..]
    );
    assert_ne!(swapped, first);
    assert!(swapped.find(MP_UUIDS[1]) < swapped.find(MP_UUIDS[0]));
    let original = export_board(DUPLICATE_MP).unwrap();
    let reordered = export_board(&swapped).unwrap();
    assert_eq!(reordered.dsn, original.dsn);
    assert_eq!(j1_pins(&reordered), j1_pins(&original));
}

/// Pads sharing a number on different nets stay separate pins, each on
/// its own net; pads sharing a number on no net are exported, on no net.
#[test]
fn repeated_signal_and_netless_pads_keep_their_own_nets() {
    let as_one = |block: &str| block.replace("(pad \"MP\"", "(pad \"1\"");
    let signal = edit_pad(
        &edit_pad(DUPLICATE_MP, MP_UUIDS[0], as_one),
        MP_UUIDS[1],
        as_one,
    );
    let bundle = export_board(&signal).unwrap();
    assert_eq!(
        j1_pins(&bundle),
        [
            pin("1", MP_UUIDS[0], "J1-1", Some("GND")),
            pin(
                "1",
                "95236974-fb55-4796-a22a-73703cb8f5cf",
                "J1-1@1",
                Some("VCC")
            ),
            pin("1", MP_UUIDS[1], "J1-1@2", Some("GND")),
            pin(
                "2",
                "3078e2f7-2337-4b75-95e4-f648e3e3ed38",
                "J1-2",
                Some("GND")
            ),
        ]
    );
    assert!(
        bundle.dsn.contains("(pins J1-1 J1-1@2 J1-2 R1-2 R2-1)"),
        "{}",
        bundle.dsn
    );
    assert!(
        bundle.dsn.contains("(pins J1-1@1 R1-1 R2-2)"),
        "{}",
        bundle.dsn
    );

    let netless = |block: &str| block.replace("\t\t\t(net \"GND\")\n", "");
    let netless = edit_pad(
        &edit_pad(DUPLICATE_MP, MP_UUIDS[0], netless),
        MP_UUIDS[1],
        netless,
    );
    let bundle = export_board(&netless).unwrap();
    assert_eq!(
        &j1_pins(&bundle)[2..],
        [
            pin("MP", MP_UUIDS[0], "J1-MP", None),
            pin("MP", MP_UUIDS[1], "J1-MP@1", None),
        ]
    );
    assert!(bundle.dsn.contains("MP@1 1800 -1875)"), "{}", bundle.dsn);
    assert!(
        bundle.dsn.contains("(pins J1-2 R1-2 R2-1)"),
        "{}",
        bundle.dsn
    );
}

/// A name the suffix would give that is another pad's number, a pad
/// without a UUID, and a UUID two pads share are refused.
#[test]
fn ambiguous_pad_identities_are_refused() {
    let collides = edit_pad(
        DUPLICATE_MP,
        "3078e2f7-2337-4b75-95e4-f648e3e3ed38",
        |block| block.replace("(pad \"2\"", "(pad \"MP@1\""),
    );
    let error = export_board(&collides).unwrap_err().to_string();
    assert!(
        error.contains("its DSN name 'MP@1' is another pad's number"),
        "{error}"
    );

    let no_uuid = edit_pad(DUPLICATE_MP, MP_UUIDS[1], |block| {
        block.replace(&format!("\t\t\t(uuid \"{}\")\n", MP_UUIDS[1]), "")
    });
    let error = export_board(&no_uuid).unwrap_err().to_string();
    assert!(error.contains("pad J1-MP has no UUID"), "{error}");

    let shared = DUPLICATE_MP.replace(MP_UUIDS[1], MP_UUIDS[0]);
    let error = export_board(&shared).unwrap_err().to_string();
    assert!(
        error.contains(&format!("repeats the UUID {}", MP_UUIDS[0])),
        "{error}"
    );
}

/// FreeRouting 2.3.0 routed the fixture's export, reading `J1-MP` and
/// `J1-MP@1` as two pins on GND, and the strict import planner accepts
/// its session: GND tracks end on both `MP` pads (#338).
#[test]
fn freerouting_routes_both_repeated_pads_and_the_planner_accepts_it() {
    let temp = tempfile::tempdir().unwrap();
    let board = temp.path().join("specctra_duplicate_mp.kicad_pcb");
    std::fs::write(&board, DUPLICATE_MP).unwrap();
    let bundle = export_dsn(&board, DUPLICATE_MP, &rules()).unwrap();
    let plan = crate::specctra_ses::parse_import_plan(
        &board,
        DUPLICATE_MP,
        &bundle.manifest,
        include_str!("../../tests/fixtures/specctra_duplicate_mp.freerouting-2.3.0.ses"),
    )
    .unwrap();
    let gnd_ends = plan
        .tracks
        .iter()
        .filter(|track| track.net_name == "GND")
        .flat_map(|track| [(track.x1_mm, track.y1_mm), (track.x2_mm, track.y2_mm)])
        .collect::<Vec<_>>();
    // J1 sits at (105, 60); its MP pads at x 105 -/+ 1.8, y 60 + 1.875.
    for pad in [(103.2, 61.875), (106.8, 61.875)] {
        assert!(
            gnd_ends
                .iter()
                .any(|end| (end.0 - pad.0).abs() < 1e-6 && (end.1 - pad.1).abs() < 1e-6),
            "no GND track ends on the MP pad at {pad:?}: {gnd_ends:?}"
        );
    }
}

/// KiCad's stock through-hole JST PH as J1, turned 90 degrees, whose pad
/// 2 is oval, and R3 and R4 with oval SMD pads: tall, wide and
/// equal-sided (#384). See its README.
const OVAL_PADS: &str = include_str!("../../tests/fixtures/specctra_oval_pads.kicad_pcb");
const OVAL_PADS_NATIVE: &str =
    include_str!("../../tests/fixtures/specctra_oval_pads.native-kicad-10.dsn");

/// An oval pad is the `path` KiCad's own exporter writes: the shorter
/// side as its width, along the longer axis, on each copper layer.
#[test]
fn oval_pads_export_as_kicad_writes_them() {
    let bundle = export_dsn(Path::new("board.kicad_pcb"), OVAL_PADS, &rules()).unwrap();
    for (ours, kicad) in [
        (
            "(path F.Cu 1200 0 -275 0 275)",
            "(path F.Cu 1200  0 -275  0 275)",
        ),
        (
            "(path B.Cu 1200 0 -275 0 275)",
            "(path B.Cu 1200  0 -275  0 275)",
        ),
        (
            "(path F.Cu 600 0 -150 0 150)",
            "(path F.Cu 600  0 -150  0 150)",
        ),
        (
            "(path F.Cu 600 -150 0 150 0)",
            "(path F.Cu 600  -150 0  150 0)",
        ),
        ("(path F.Cu 700 0 0 0 0)", "(path F.Cu 700  0 0  0 0)"),
    ] {
        assert!(
            OVAL_PADS_NATIVE.contains(kicad),
            "KiCad's DSN lacks {kicad}"
        );
        assert!(
            bundle.dsn.contains(ours),
            "{ours} missing from\n{}",
            bundle.dsn
        );
    }
    let manifest: serde_json::Value = serde_json::from_str(&bundle.manifest).unwrap();
    let ovals = manifest["padstacks"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|padstack| padstack["shape"] == "oval")
        .map(|padstack| {
            (
                padstack["size_x_um"].as_i64().unwrap(),
                padstack["size_y_um"].as_i64().unwrap(),
            )
        })
        .collect::<BTreeSet<_>>();
    assert_eq!(
        ovals,
        BTreeSet::from([(600, 900), (700, 700), (900, 600), (1200, 1750)])
    );
    assert!(manifest["supported_profile"]["pad_shapes"]
        .as_array()
        .unwrap()
        .contains(&serde_json::json!("oval")));
}

#[test]
fn an_oval_spine_runs_along_the_longer_side() {
    assert_eq!(oval_spine(600, 900), [(0.0, -150.0), (0.0, 150.0)]);
    assert_eq!(oval_spine(900, 600), [(-150.0, 0.0), (150.0, 0.0)]);
    assert_eq!(oval_spine(700, 700), [(0.0, 0.0), (0.0, 0.0)]);
}

/// An oval pad with an oval drill (a slot) is still refused: Specctra's
/// padstacks can't carry the slot.
#[test]
fn an_oval_drill_is_still_refused() {
    let at = OVAL_PADS.find("(pad \"2\" thru_hole oval").unwrap();
    let drill = at + OVAL_PADS[at..].find("(drill 0.75)").unwrap();
    let slotted = format!(
        "{}(drill oval 0.75 1.2){}",
        &OVAL_PADS[..drill],
        &OVAL_PADS[drill + "(drill 0.75)".len()..]
    );
    let error = export_dsn(Path::new("board.kicad_pcb"), &slotted, &rules())
        .unwrap_err()
        .to_string();
    assert!(error.contains("pad J1-2: oval drill"), "{error}");
}

/// KiCad's DSN of the stock JST PH carries its courtyard as an
/// `(outline (polygon ...))`, which `validate_dsn_syntax` accepts (#98),
/// so the oval padstacks map to KiCad's own names without stripping
/// courtyards.
#[test]
fn native_oval_padstacks_map_to_kicads_names() {
    let baseline = export_dsn(
        Path::new("board.kicad_pcb"),
        OVAL_PADS,
        &native_fixture_rules(),
    )
    .unwrap();
    let adopted = adopt_native_dsn(baseline, OVAL_PADS_NATIVE.to_string()).unwrap();
    let manifest: serde_json::Value = serde_json::from_str(&adopted.manifest).unwrap();
    let names = manifest["padstacks"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|padstack| padstack["shape"] == "oval")
        .map(|padstack| padstack["name"].as_str().unwrap().to_string())
        .collect::<BTreeSet<_>>();
    assert_eq!(
        names,
        BTreeSet::from([
            "Oval[A]Pad_1200.000000x1750.000000_um".to_string(),
            "Oval[T]Pad_600.000000x900.000000_um".to_string(),
            "Oval[T]Pad_700.000000x700.000000_um".to_string(),
            "Oval[T]Pad_900.000000x600.000000_um".to_string(),
        ])
    );
}

/// An oval whose sides differ by an odd number of micrometres has its
/// spine's ends on a half micrometre, in KiCad's DSN as in Copperpour's,
/// and the two still match.
#[test]
fn a_half_micrometre_oval_spine_matches_kicads() {
    let odd = OVAL_PADS.replacen("(size 0.6 0.9)", "(size 0.6 0.901)", 1);
    assert_ne!(odd, OVAL_PADS);
    let baseline = export_dsn(Path::new("board.kicad_pcb"), &odd, &native_fixture_rules()).unwrap();
    assert!(
        baseline.dsn.contains("(path F.Cu 600 0 -150.5 0 150.5)"),
        "{}",
        baseline.dsn
    );
    let native =
        include_str!("../../tests/fixtures/specctra_oval_pads_odd.native-kicad-10.dsn").to_string();
    assert!(native.contains("(path F.Cu 600  0 -150.5  0 150.5)"));
    let adopted = adopt_native_dsn(baseline, native).unwrap();
    assert!(adopted
        .manifest
        .contains("Oval[T]Pad_600.000000x901.000000_um"));
}

/// Freerouting 2.3.0 routed the fixture's export, oval pads included, and
/// the strict import planner accepts its session: each oval pad has a
/// track of its net ending on its centre.
#[test]
fn freerouting_routes_to_oval_pads_and_the_planner_accepts_it() {
    let temp = tempfile::tempdir().unwrap();
    let board = temp.path().join("specctra_oval_pads.kicad_pcb");
    std::fs::write(&board, OVAL_PADS).unwrap();
    let bundle = export_dsn(&board, OVAL_PADS, &rules()).unwrap();
    let plan = crate::specctra_ses::parse_import_plan(
        &board,
        OVAL_PADS,
        &bundle.manifest,
        include_str!("../../tests/fixtures/specctra_oval_pads.freerouting-2.3.0.ses"),
    )
    .unwrap();
    let ends = |net: &str| {
        plan.tracks
            .iter()
            .filter(|track| track.net_name == net)
            .flat_map(|track| [(track.x1_mm, track.y1_mm), (track.x2_mm, track.y2_mm)])
            .collect::<Vec<_>>()
    };
    // J1 at (92, 60) turned 90 degrees puts its pad 2 at (92, 58).
    for (net, pad) in [
        ("GND", (92.0, 58.0)),
        ("VCC", (115.5, 60.0)),
        ("GND", (116.5, 60.0)),
        ("VCC", (123.5, 60.0)),
        ("GND", (124.5, 60.0)),
    ] {
        assert!(
            ends(net)
                .iter()
                .any(|end| (end.0 - pad.0).abs() < 1e-6 && (end.1 - pad.1).abs() < 1e-6),
            "no {net} track ends on the oval pad at {pad:?}: {:?}",
            ends(net)
        );
    }
}

/// KiCad names repeated pads by file order, and adoption of its DSN
/// isn't proven for them: it is refused, naming the footprint (#338).
#[test]
fn native_adoption_refuses_repeated_pad_numbers() {
    let native = include_str!("../../tests/fixtures/specctra_duplicate_mp.native-kicad-10.dsn");
    assert!(native.contains("(pins R1-2 R2-1 J1-2 J1-MP J1-MP@1)"));
    let error = adopt_native_dsn(export_board(DUPLICATE_MP).unwrap(), native.to_string())
        .unwrap_err()
        .to_string();
    assert!(
        error.contains("does not map repeated pad numbers yet: J1 has more than one pad 'MP'"),
        "{error}"
    );
}

#[test]
fn deterministic_export_round_trips_through_specctra_parser() {
    let source = include_str!("../../tests/fixtures/specctra_two_resistors.kicad_pcb");
    let first = export_dsn(Path::new("board.kicad_pcb"), source, &rules()).unwrap();
    let second = export_dsn(Path::new("board.kicad_pcb"), source, &rules()).unwrap();

    assert_eq!(first.dsn, second.dsn);
    assert_eq!(first.manifest, second.manifest);
    assert_eq!(first.component_count, 2);
    assert_eq!(first.pad_count, 4);
    assert_eq!(first.net_count, 2);
    assert!(first.dsn.contains("(pcb board.kicad_pcb"));
    assert!(first.dsn.contains("(boundary"));
    assert!(first.dsn.contains("(net GND"));
    assert!(first.dsn.contains("R1-1"));
    let manifest: serde_json::Value = serde_json::from_str(&first.manifest).unwrap();
    assert_eq!(manifest["components"][0]["reference"], "R1");
    assert_eq!(manifest["components"][0]["x_um"], 100_000);
    assert_eq!(manifest["components"][0]["y_um"], -50_000);
    assert_eq!(manifest["components"][0]["rotation_degrees"], 0.0);
    assert_eq!(manifest["components"][0]["side"], "front");
}

#[test]
fn freerouting_owned_dsn_corpus_parses() {
    let source = include_str!("../../tests/fixtures/freerouting_issue269_minimal_v2_3_0.dsn");
    let mut tokenizer = ListTokenizer::new(BufReader::new(Cursor::new(source.as_bytes())));
    dsn::DsnFile::read_dsn(&mut tokenizer)
        .expect("Freerouting v2.3.0 corpus fixture must remain parseable");
}

#[test]
fn native_kicad_dsn_rewrites_manifest_identifiers_without_changing_semantics() {
    let source = include_str!("../../tests/fixtures/specctra_two_resistors.kicad_pcb");
    let native = include_str!("../../tests/fixtures/specctra_two_resistors.native-kicad-10.dsn");
    let baseline = native_baseline(source);
    let adopted = adopt_native_dsn(baseline, native.to_string()).unwrap();
    let manifest: serde_json::Value = serde_json::from_str(&adopted.manifest).unwrap();

    assert_eq!(adopted.dsn, native);
    assert_eq!(
        manifest["components"][0]["image_name"],
        "Resistor_SMD:R_0402"
    );
    assert_eq!(
        manifest["components"][0]["pads"][0]["padstack_name"],
        "Rect[T]Pad_600.000000x500.000000_um"
    );
    assert_eq!(manifest["nets"][0]["class_name"], "kicad_default");
    assert!(manifest["padstacks"]
        .as_array()
        .unwrap()
        .iter()
        .any(|padstack| padstack["name"] == "Via[0-1]_600:300_um"));
}

/// Optional local parity check against the Freerouting engine. CI does not
/// install Java or Freerouting; maintainers can opt in with
/// `FREEROUTING_JAR=/path/to/freerouting.jar cargo test -p copperpour-core
/// freerouting_accepts_exported_fixture -- --ignored`.
#[cfg(feature = "freerouting")]
#[test]
#[ignore = "requires Java and FREEROUTING_JAR"]
fn freerouting_accepts_exported_fixture() {
    let jar = std::env::var_os("FREEROUTING_JAR").expect("set FREEROUTING_JAR");
    let source = include_str!("../../tests/fixtures/specctra_two_resistors.kicad_pcb");
    let temp = tempfile::tempdir().expect("tempdir");
    let board_path = temp.path().join("board.kicad_pcb");
    let dsn_path = temp.path().join("board.dsn");
    let ses_path = temp.path().join("board.ses");
    std::fs::write(&board_path, source).expect("write board fixture");
    let export = export_dsn(&board_path, source, &rules()).unwrap();
    std::fs::write(&dsn_path, export.dsn).expect("write DSN");

    let _env = crate::java_runtime::JAVA_ENV_LOCK
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let output = std::process::Command::new(crate::java_runtime::java_executable())
        .arg("-jar")
        .arg(jar)
        .arg("-de")
        .arg(&dsn_path)
        .arg("-do")
        .arg(&ses_path)
        .arg("-mp")
        .arg("2")
        .output()
        .expect("launch Freerouting");

    assert!(
        output.status.success(),
        "Freerouting refused generated DSN:\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        std::fs::metadata(&ses_path)
            .map(|metadata| metadata.len() > 0)
            .unwrap_or(false),
        "Freerouting did not produce a non-empty SES"
    );
    let ses = std::fs::read_to_string(&ses_path).expect("read Freerouting SES");
    crate::specctra_ses::parse_import_plan(&board_path, source, &export.manifest, &ses)
        .expect("Freerouting SES must pass Copperpour's strict import planner");
}

/// Real-engine parity for KiCad 10's native exporter identifiers plus the
/// rewritten revision manifest consumed by the strict SES planner.
#[cfg(feature = "freerouting")]
#[test]
#[ignore = "requires Java and FREEROUTING_JAR"]
fn freerouting_round_trips_native_kicad_export() {
    let jar = std::env::var_os("FREEROUTING_JAR").expect("set FREEROUTING_JAR");
    let source = include_str!("../../tests/fixtures/specctra_two_resistors.kicad_pcb");
    let native = include_str!("../../tests/fixtures/specctra_two_resistors.native-kicad-10.dsn");
    let temp = tempfile::tempdir().expect("tempdir");
    let board_path = temp.path().join("board.kicad_pcb");
    let dsn_path = temp.path().join("board.dsn");
    let ses_path = temp.path().join("board.ses");
    std::fs::write(&board_path, source).expect("write board fixture");
    let baseline = export_dsn(&board_path, source, &native_fixture_rules()).unwrap();
    let export = adopt_native_dsn(baseline, native.to_string()).unwrap();
    std::fs::write(&dsn_path, &export.dsn).expect("write native DSN");

    let _env = crate::java_runtime::JAVA_ENV_LOCK
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let output = std::process::Command::new(crate::java_runtime::java_executable())
        .arg("-jar")
        .arg(jar)
        .arg("-de")
        .arg(&dsn_path)
        .arg("-do")
        .arg(&ses_path)
        .arg("-mp")
        .arg("2")
        .output()
        .expect("run Freerouting");
    assert!(
        output.status.success(),
        "Freerouting failed:\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let ses = std::fs::read_to_string(&ses_path).expect("read SES");
    let plan = crate::specctra_ses::parse_import_plan(&board_path, source, &export.manifest, &ses)
        .expect("strict SES planner accepts native KiCad identifiers");
    assert!(!plan.tracks.is_empty() || !plan.vias.is_empty());
}

#[test]
fn incomplete_effective_rules_are_refused() {
    let source = include_str!("../../tests/fixtures/specctra_two_resistors.kicad_pcb");
    let mut rules = rules();
    rules.get_mut("GND").unwrap().via_drill_mm = None;
    let error = export_dsn(Path::new("board.kicad_pcb"), source, &rules)
        .unwrap_err()
        .to_string();
    assert!(error.contains("via drill"), "{error}");
}

#[test]
fn unlocked_routing_is_refused_before_export() {
    let source = include_str!("../../tests/fixtures/specctra_two_resistors.kicad_pcb").replace(
        "\n)",
        "\n  (segment (start 1 1) (end 2 2) (width 0.2) (layer \"F.Cu\") (net 1))\n)",
    );
    let error = export_dsn(Path::new("board.kicad_pcb"), &source, &rules())
        .unwrap_err()
        .to_string();
    assert!(error.contains("existing track segment"), "{error}");
}

#[test]
fn locked_tracks_and_vias_are_exported_as_fixed_wiring() {
    let source = include_str!("../../tests/fixtures/specctra_two_resistors_locked.kicad_pcb");
    let export = export_dsn(Path::new("board.kicad_pcb"), source, &rules()).unwrap();
    assert!(export.dsn.contains("(type fix)"));
    assert!(export.dsn.contains("(path F.Cu 250"));
    // The names a DSN, its manifest and Freerouting's SES share (Copperpour/Copperpour#250).
    for name in [
        "(via copperpour_via_",
        "(image copperpour_image_",
        "(pin copperpour_pad_",
        "(class copperpour_class_",
    ] {
        assert!(export.dsn.contains(name), "{name}");
    }
    let manifest: serde_json::Value = serde_json::from_str(&export.manifest).unwrap();
    assert_eq!(manifest["supported_profile"]["existing_routing"], true);
    assert_eq!(manifest["supported_profile"]["locked_track_count"], 1);
    assert_eq!(manifest["supported_profile"]["locked_via_count"], 1);
}

#[test]
fn freerouting_session_preserves_locked_routing_outside_the_import_plan() {
    let source = include_str!("../../tests/fixtures/specctra_two_resistors_locked.kicad_pcb");
    let native =
        include_str!("../../tests/fixtures/specctra_two_resistors_locked.native-kicad-10.dsn");
    let ses =
        include_str!("../../tests/fixtures/specctra_two_resistors_locked.freerouting-2.3.0.ses");
    let temp = tempfile::tempdir().unwrap();
    let board_path = temp.path().join("specctra_two_resistors_locked.kicad_pcb");
    std::fs::write(&board_path, source).unwrap();
    let baseline = export_dsn(&board_path, source, &native_fixture_rules()).unwrap();
    let adopted = adopt_native_dsn(baseline, native.to_string()).unwrap();
    let plan = crate::specctra_ses::parse_import_plan(&board_path, source, &adopted.manifest, ses)
        .unwrap();
    assert_eq!(plan.locked_track_count, 1);
    assert_eq!(plan.locked_via_count, 1);
    assert!(!plan.tracks.is_empty() || !plan.vias.is_empty());
}

#[test]
fn locked_arcs_are_refused_instead_of_approximated() {
    let source = include_str!("../../tests/fixtures/specctra_two_resistors_locked_arc.kicad_pcb");
    let error = export_dsn(Path::new("board.kicad_pcb"), source, &rules())
        .unwrap_err()
        .to_string();
    assert!(error.contains("locked routed arc"), "{error}");
    assert!(error.contains("without approximation"), "{error}");
}

#[test]
fn branched_outline_is_refused() {
    let source = include_str!("../../tests/fixtures/specctra_two_resistors.kicad_pcb")
        .replace(
            "\n)",
            "\n  (gr_line (start 80 30) (end 90 40) (stroke (width 0.05) (type default)) (layer \"Edge.Cuts\") (uuid \"branch\"))\n)",
        );
    let error = export_dsn(Path::new("board.kicad_pcb"), &source, &rules())
        .unwrap_err()
        .to_string();
    assert!(error.contains("degree"), "{error}");
}

const ROUNDED_OUTLINE: &str =
    include_str!("../../tests/fixtures/specctra_rounded_outline.kicad_pcb");

fn point_segment_distance(point: (f64, f64), a: (f64, f64), b: (f64, f64)) -> f64 {
    let delta = (b.0 - a.0, b.1 - a.1);
    let t = (((point.0 - a.0) * delta.0 + (point.1 - a.1) * delta.1)
        / (delta.0 * delta.0 + delta.1 * delta.1))
        .clamp(0.0, 1.0);
    (point.0 - a.0 - t * delta.0).hypot(point.1 - a.1 - t * delta.1)
}

fn boundary_points(dsn: &str) -> Vec<(f64, f64)> {
    let tree = parse_dsn_sexp(dsn).unwrap();
    let path = tree
        .find("structure")
        .unwrap()
        .find("boundary")
        .unwrap()
        .find("path")
        .unwrap();
    path.children().unwrap()[3..]
        .chunks_exact(2)
        .map(|pair| {
            (
                pair[0].as_str().unwrap().parse().unwrap(),
                pair[1].as_str().unwrap().parse().unwrap(),
            )
        })
        .collect()
}

#[test]
fn rounded_outline_arc_is_flattened_and_closed() {
    let tree = parse_sexp(ROUNDED_OUTLINE).unwrap();
    let outline = simple_closed_outline(&tree, 5.0).unwrap();
    assert_eq!(outline.first(), outline.last());
    // Three convex corners (12 chords each), one concave notch (19), four lines.
    assert_eq!(outline.len(), 12 * 3 + 19 + 4 + 1);
}

#[test]
fn exporter_accepts_rounded_outline_arcs_and_reports_tolerance() {
    let temp = tempfile::tempdir().unwrap();
    let board_path = temp.path().join("rounded.kicad_pcb");
    std::fs::write(
        board_path.with_extension("kicad_pro"),
        include_str!("../../tests/fixtures/specctra_rounded_outline.kicad_pro"),
    )
    .unwrap();
    let export = export_dsn(&board_path, ROUNDED_OUTLINE, &rules()).unwrap();
    let manifest: serde_json::Value = serde_json::from_str(&export.manifest).unwrap();
    assert_eq!(
        manifest["supported_profile"]["outline"],
        "one closed loop of Edge.Cuts lines and circular arcs, with arcs flattened within board max_error"
    );
    let native = include_str!("../../tests/fixtures/specctra_rounded_outline.native-kicad-10.dsn");
    // Independent circles and counts from KiCad's native export, not the flattener.
    for points in [boundary_points(&export.dsn), boundary_points(native)] {
        assert_eq!(points.first(), points.last());
        for (center, radius, expected_segments) in [
            ((128_000.0, -32_000.0), 2_000.0, 12),
            ((130_000.0, -70_000.0), 5_000.0, 19),
            ((82_000.0, -68_000.0), 2_000.0, 12),
            ((82_000.0, -32_000.0), 2_000.0, 12),
        ] {
            let radial_error =
                |p: (f64, f64)| ((p.0 - center.0).hypot(p.1 - center.1) - radius).abs();
            let arc_edges = points
                .windows(2)
                .filter(|edge| radial_error(edge[0]) < 5.0 && radial_error(edge[1]) < 5.0)
                .collect::<Vec<_>>();
            assert_eq!(arc_edges.len(), expected_segments, "circle {center:?}");
            for edge in arc_edges {
                assert!(radial_error(edge[0]) <= 5.0);
                assert!(radial_error(edge[1]) <= 5.0);
                let midpoint = ((edge[0].0 + edge[1].0) / 2.0, (edge[0].1 + edge[1].1) / 2.0);
                assert!(radial_error(midpoint) <= 5.0, "chord {edge:?}");
            }
        }
    }
}

#[test]
fn outline_arc_flattening_respects_tolerance_in_both_directions() {
    let tree = parse_sexp(ROUNDED_OUTLINE).unwrap();
    for arc in tree.find_all("gr_arc") {
        let start = point_um_f64(arc, "start").unwrap();
        let mid = point_um_f64(arc, "mid").unwrap();
        let end = point_um_f64(arc, "end").unwrap();
        for (start, end) in [(start, end), (end, start)] {
            let points = outline_arc_points(start, mid, end, 5.0).unwrap();
            assert_eq!(
                points.first(),
                Some(&(start.0.round() as i64, start.1.round() as i64))
            );
            assert_eq!(
                points.last(),
                Some(&(end.0.round() as i64, end.1.round() as i64))
            );
            let distance = points
                .windows(2)
                .map(|edge| {
                    point_segment_distance(
                        mid,
                        (edge[0].0 as f64, edge[0].1 as f64),
                        (edge[1].0 as f64, edge[1].1 as f64),
                    )
                })
                .fold(f64::INFINITY, f64::min);
            assert!(distance <= 5.0, "arc misses its midpoint by {distance} um");
        }
    }
}

const D_OUTLINE: &str = include_str!("../../tests/fixtures/specctra_d_outline.kicad_pcb");
const D_OUTLINE_ARC: &str = "(gr_arc\n\t\t(start 90 25)\n\t\t(mid 115 50)\n\t\t(end 90 75)";

#[test]
fn exporter_accepts_a_two_edge_d_shaped_outline() {
    let export = export_dsn(Path::new("board.kicad_pcb"), D_OUTLINE, &rules()).unwrap();
    let native = include_str!("../../tests/fixtures/specctra_d_outline.native-kicad-10.dsn");
    // KiCad's native export splits the 25 mm half circle into 80 chords.
    // The Rust flattener keeps each chord within max_error minus the DSN
    // grid allowance, which needs 85 at this radius (the small corners of
    // the rounded fixture happen to give equal counts).
    let (center, radius) = ((90_000.0, -50_000.0), 25_000.0);
    let radial_error = |p: (f64, f64)| ((p.0 - center.0).hypot(p.1 - center.1) - radius).abs();
    for (points, expected_chords) in [
        (boundary_points(&export.dsn), 85),
        (boundary_points(native), 80),
    ] {
        assert_eq!(points.first(), points.last());
        let (arc_edges, straight_edges): (Vec<_>, Vec<_>) = points.windows(2).partition(|edge| {
            let midpoint = ((edge[0].0 + edge[1].0) / 2.0, (edge[0].1 + edge[1].1) / 2.0);
            radial_error(midpoint) <= 5.0
        });
        assert_eq!(arc_edges.len(), expected_chords);
        for edge in arc_edges {
            assert!(radial_error(edge[0]) <= 5.0, "chord {edge:?}");
            assert!(radial_error(edge[1]) <= 5.0, "chord {edge:?}");
            // The half circle bulges through (115, 50), not to x < 90 mm.
            assert!(edge.iter().all(|p| p.0 >= 89_999.5), "chord {edge:?}");
        }
        // The straight edge is the line x = 90 mm from y = 25 mm to 75 mm.
        assert_eq!(straight_edges.len(), 1);
        let line = straight_edges[0];
        assert!(line.iter().all(|p| (p.0 - 90_000.0).abs() < 0.5));
        assert!(((line[0].1 - line[1].1).abs() - 50_000.0).abs() < 0.5);
    }
}

#[test]
fn two_edge_outlines_without_area_are_refused() {
    let two_lines = D_OUTLINE.replace(
        D_OUTLINE_ARC,
        "(gr_line\n\t\t(start 90 75)\n\t\t(end 90 25)",
    );
    let same_arc_twice = D_OUTLINE.replace(
        "(gr_line\n\t\t(start 90 25)\n\t\t(end 90 75)",
        D_OUTLINE_ARC,
    );
    for source in [two_lines, same_arc_twice] {
        assert_ne!(source, D_OUTLINE);
        let error = export_dsn(Path::new("board.kicad_pcb"), &source, &rules())
            .unwrap_err()
            .to_string();
        assert!(error.contains("enclose no area"), "{error}");
    }
}

#[test]
fn board_without_edge_cuts_is_refused() {
    let source = D_OUTLINE.replace("(layer \"Edge.Cuts\")", "(layer \"F.SilkS\")");
    assert_ne!(source, D_OUTLINE);
    let error = export_dsn(Path::new("board.kicad_pcb"), &source, &rules())
        .unwrap_err()
        .to_string();
    assert!(error.contains("no Edge.Cuts lines or arcs"), "{error}");
}

#[test]
fn multiple_outline_loops_are_refused() {
    let source = include_str!("../../tests/fixtures/specctra_multiple_outline_loops.kicad_pcb");
    let error = export_dsn(Path::new("board.kicad_pcb"), source, &rules())
        .unwrap_err()
        .to_string();
    assert!(error.contains("more than one loop"), "{error}");
}

#[test]
fn outline_arc_error_uses_project_max_error_when_available() {
    let temp = tempfile::tempdir().unwrap();
    let board_path = temp.path().join("board.kicad_pcb");
    let tree = parse_sexp(ROUNDED_OUTLINE).unwrap();
    let mut project: serde_json::Value = serde_json::from_str(include_str!(
        "../../tests/fixtures/specctra_rounded_outline.kicad_pro"
    ))
    .unwrap();
    project["board"]["design_settings"]["rules"]["max_error"] = serde_json::json!(0.01);
    std::fs::write(
        board_path.with_extension("kicad_pro"),
        serde_json::to_vec(&project).unwrap(),
    )
    .unwrap();
    assert_eq!(outline_arc_error_um(&board_path, &tree).unwrap(), 10.0);
    let outline =
        simple_closed_outline(&tree, outline_arc_error_um(&board_path, &tree).unwrap()).unwrap();
    assert_eq!(outline.len(), 9 * 3 + 13 + 4 + 1);
}

#[test]
fn malformed_outline_project_is_refused() {
    let temp = tempfile::tempdir().unwrap();
    let board_path = temp.path().join("board.kicad_pcb");
    for (project, expected) in [
        ("{", "parse project"),
        (
            r#"{"board":{"design_settings":{"rules":{"max_error":"bad"}}}}"#,
            "max_error is not a number",
        ),
    ] {
        std::fs::write(board_path.with_extension("kicad_pro"), project).unwrap();
        let error = export_dsn(&board_path, ROUNDED_OUTLINE, &rules())
            .unwrap_err()
            .to_string();
        assert!(error.contains(expected), "{error}");
    }
}

#[test]
fn nonzero_pad_shape_offset_is_refused() {
    let source = include_str!("../../tests/fixtures/specctra_two_resistors.kicad_pcb").replacen(
        "(size 0.6 0.5)",
        "(size 0.6 0.5)\n\t\t\t(offset 0.1 0)",
        1,
    );
    let error = export_dsn(Path::new("board.kicad_pcb"), &source, &rules())
        .unwrap_err()
        .to_string();
    assert!(error.contains("shape offset"), "{error}");
}

const ROTATED: &str = include_str!("../../tests/fixtures/specctra_rotated_footprints.kicad_pcb");
const ROTATED_NATIVE: &str =
    include_str!("../../tests/fixtures/specctra_rotated_footprints.native-kicad-10.dsn");

/// Each placed pin's `(rotate ...)`, keyed by reference and pin number,
/// with an absent rotate read as 0. Read straight off the S-expression so
/// KiCad's sub-micrometre pin positions (#841) don't stop the comparison.
pub(crate) fn pin_rotations(dsn: &str) -> BTreeMap<(String, String), String> {
    let root = parse_dsn_sexp(dsn).unwrap();
    let library = root.find("library").unwrap();
    let mut rotations = BTreeMap::new();
    for component in root.find("placement").unwrap().find_all("component") {
        let image_name = component.get(1).and_then(SexpNode::as_str).unwrap();
        let image = library
            .find_all("image")
            .into_iter()
            .find(|image| image.get(1).and_then(SexpNode::as_str) == Some(image_name))
            .unwrap();
        for place in component.find_all("place") {
            let reference = place.get(1).and_then(SexpNode::as_str).unwrap();
            for pin in image.find_all("pin") {
                let number = pin.children().unwrap()[2..]
                    .iter()
                    .find_map(SexpNode::as_str)
                    .unwrap();
                let rotate = pin
                    .find("rotate")
                    .and_then(|rotate| rotate.get(1))
                    .and_then(SexpNode::as_str)
                    .unwrap_or("0");
                rotations.insert(
                    (reference.to_string(), number.to_string()),
                    rotate.to_string(),
                );
            }
        }
    }
    rotations
}

/// A pad's file angle includes its footprint's, and `(place ...)` turns
/// the image again, so a pin's `(rotate ...)` must be the pad's angle
/// within the footprint: what KiCad's own exporter writes (#840).
#[test]
fn rotated_footprint_pins_carry_the_rotation_kicad_writes() {
    let source = include_str!("../../tests/fixtures/specctra_rotated_footprints_45.kicad_pcb");
    let native =
        include_str!("../../tests/fixtures/specctra_rotated_footprints_45.native-kicad-10.dsn");
    let baseline = native_baseline(source);

    let expected = [
        ("R1", "1", "12.3457"),
        ("R1", "2", "12.3457"),
        ("R2", "1", "0"),
        ("R2", "2", "0"),
        ("R3", "1", "0"),
        ("R3", "2", "0"),
        ("R4", "1", "90"),
        ("R4", "2", "270"),
    ]
    .into_iter()
    .map(|(reference, pin, rotate)| ((reference.to_string(), pin.to_string()), rotate.to_string()))
    .collect::<BTreeMap<_, _>>();
    assert_eq!(pin_rotations(native), expected);
    assert_eq!(pin_rotations(&baseline.dsn), expected);
}

/// `12.3 - 12.2` is `0.10000000000000142` in f64.
#[test]
fn pin_rotation_drops_float_noise() {
    assert_eq!(pin_rotation(12.3, 12.2), 0.1);
}

#[test]
fn native_dsn_of_rotated_footprints_is_adopted() {
    let adopted = adopt_native_dsn(native_baseline(ROTATED), ROTATED_NATIVE.to_string()).unwrap();
    assert_eq!(adopted.dsn, ROTATED_NATIVE);
}

#[test]
fn native_dsn_that_turns_a_pin_is_refused() {
    let native = ROTATED_NATIVE.replacen("(rotate 270) ", "", 1);
    let error = adopt_native_dsn(native_baseline(ROTATED), native)
        .unwrap_err()
        .to_string();
    assert!(error.contains("pin rotation"), "{error}");
}

/// KiCad's stock-header boards, whose J1 pad 1 has a 2 mm circle on
/// `B.Cu` under its 1.7 mm square front (#842).
#[test]
fn per_layer_padstacks_are_refused() {
    for (mode, source) in [
        (
            "front_inner_back",
            include_str!("../../tests/fixtures/specctra_padstack_front_inner_back.kicad_pcb"),
        ),
        (
            "custom",
            include_str!("../../tests/fixtures/specctra_padstack_custom.kicad_pcb"),
        ),
    ] {
        let error = export_dsn(Path::new("board.kicad_pcb"), source, &rules())
            .unwrap_err()
            .to_string();
        assert!(
            error.contains(&format!("pad J1-1 has a '{mode}' padstack")),
            "{error}"
        );
    }
}

/// KiCad always writes a mode; a block without one is refused all the same.
#[test]
fn padstack_without_a_mode_is_refused() {
    let source = include_str!("../../tests/fixtures/specctra_padstack_custom.kicad_pcb").replacen(
        "(mode custom)",
        "",
        1,
    );
    let error = export_dsn(Path::new("board.kicad_pcb"), &source, &rules())
        .unwrap_err()
        .to_string();
    assert!(
        error.contains("pad J1-1 has a padstack with no mode"),
        "{error}"
    );
}

/// The same board with no padstack block, as KiCad saves a normal pad.
#[test]
fn stock_header_with_normal_pads_exports() {
    let source = include_str!("../../tests/fixtures/specctra_padstack_normal.kicad_pcb");
    let export = export_dsn(Path::new("board.kicad_pcb"), source, &rules()).unwrap();
    assert_eq!(export.pad_count, 4);
    assert!(
        export.dsn.contains("(rect F.Cu -850 -850 850 850)")
            && export.dsn.contains("(rect B.Cu -850 -850 850 850)"),
        "{}",
        export.dsn
    );
}

/// KiCad's locked-via board, its via given a 1 mm `B.Cu` size under a
/// 0.6 mm front (#842).
#[test]
fn locked_via_with_a_per_layer_padstack_is_refused() {
    let source = include_str!("../../tests/fixtures/specctra_padstack_locked_via.kicad_pcb");
    let error = export_dsn(Path::new("board.kicad_pcb"), source, &rules())
        .unwrap_err()
        .to_string();
    assert!(
        error.contains("locked via at (105, 50) mm has a 'front_inner_back' padstack"),
        "{error}"
    );
}

/// KiCad 10's four-layer copy of the locked two-resistor board, with a
/// stock through-hole header and a locked track on `In1.Cu`. See its README.
const FOUR_LAYERS: &str = include_str!("../../tests/fixtures/specctra_four_layers.kicad_pcb");
const FOUR_LAYERS_NATIVE_FILE: &str =
    include_str!("../../tests/fixtures/specctra_four_layers.native-kicad-10.dsn");

fn four_layers_native() -> String {
    FOUR_LAYERS_NATIVE_FILE.to_string()
}

fn copper_of(table: &str) -> Result<Vec<String>> {
    copper_layers(&parse_sexp(&format!("(kicad_pcb (layers {table}))")).unwrap())
}

#[test]
fn copper_layers_run_top_to_bottom_whatever_the_file_order() {
    assert_eq!(
        copper_of(r#"(0 "F.Cu" signal) (2 "B.Cu" signal) (9 "F.Adhes" user)"#).unwrap(),
        ["F.Cu", "B.Cu"]
    );
    // KiCad 10 writes the table top to bottom, as in the four-layer fixture.
    assert_eq!(
        copper_of(r#"(0 "F.Cu" signal) (4 "In1.Cu" power) (6 "In2.Cu" mixed) (2 "B.Cu" signal)"#)
            .unwrap(),
        ["F.Cu", "In1.Cu", "In2.Cu", "B.Cu"]
    );
    // An order KiCad does not write in the table gives the same stack.
    assert_eq!(
        copper_of(r#"(0 "F.Cu" signal) (2 "B.Cu" signal) (4 "In1.Cu" power) (6 "In2.Cu" mixed)"#)
            .unwrap(),
        ["F.Cu", "In1.Cu", "In2.Cu", "B.Cu"]
    );
    for table in [
        r#"(0 "F.Cu" signal) (6 "In2.Cu" signal) (2 "B.Cu" signal)"#,
        r#"(0 "F.Cu" signal) (4 "In1.Cu" signal) (2 "B.Cu" signal)"#,
        r#"(0 "F.Cu" signal) (4 "In1.Cu" signal) (6 "In2.Cu" signal)"#,
        r#"(0 "F.Cu" signal) (0 "F.Cu" signal) (2 "B.Cu" signal)"#,
        r#"(0 "F.Cu" signal)"#,
        r#"(9 "F.Adhes" user)"#,
    ] {
        let error = copper_of(table).unwrap_err().to_string();
        assert!(
            error.contains("copper layers must be F.Cu, In1.Cu..In<N-2>.Cu and B.Cu"),
            "{table}: {error}"
        );
    }
}

#[test]
fn four_layer_board_exports_every_copper_layer() {
    let export = export_dsn(
        Path::new("board.kicad_pcb"),
        FOUR_LAYERS,
        &native_fixture_rules(),
    )
    .unwrap();
    assert_eq!(export.copper_layer_count, 4);
    let tree = parse_dsn_sexp(&export.dsn).unwrap();
    let identity = dsn_identity(&tree).unwrap();
    assert_eq!(
        identity.layers.values().collect::<Vec<_>>(),
        ["F.Cu", "In1.Cu", "In2.Cu", "B.Cu"]
    );
    // The header's through-hole pads and the via cover all four layers.
    for name in ["In1.Cu", "In2.Cu"] {
        assert!(
            export
                .dsn
                .contains(&format!("(rect {name} -850 -850 850 850)")),
            "{}",
            export.dsn
        );
        assert!(
            export.dsn.contains(&format!("(circle {name} 600)")),
            "{}",
            export.dsn
        );
    }
    assert!(
        export
            .dsn
            .contains("(path In1.Cu 250 105000 -50000 105000 -55000)"),
        "{}",
        export.dsn
    );
    let manifest: serde_json::Value = serde_json::from_str(&export.manifest).unwrap();
    assert_eq!(manifest["supported_profile"]["copper_layers"], 4);
    assert_eq!(manifest["supported_profile"]["locked_track_count"], 2);
    assert_eq!(manifest["supported_profile"]["locked_via_count"], 1);
    assert_eq!(
        manifest["layers"]
            .as_array()
            .unwrap()
            .iter()
            .map(|layer| (
                layer["index"].as_u64().unwrap(),
                layer["kicad_name"].as_str().unwrap()
            ))
            .collect::<Vec<_>>(),
        [(0, "F.Cu"), (1, "In1.Cu"), (2, "In2.Cu"), (3, "B.Cu")]
    );
    let via = manifest["padstacks"]
        .as_array()
        .unwrap()
        .iter()
        .find(|padstack| padstack["purpose"] == "via")
        .unwrap();
    assert_eq!(
        via["layers"],
        serde_json::json!(["F.Cu", "In1.Cu", "In2.Cu", "B.Cu"])
    );
}

/// KiCad's own export of the same board passes the semantic comparison
/// layer by layer, pad by pad: both paths describe one routing job.
#[test]
fn four_layer_native_dsn_matches_the_rust_export() {
    let baseline = native_baseline(FOUR_LAYERS);
    let adopted = adopt_native_dsn(baseline, four_layers_native()).unwrap();
    assert_eq!(adopted.dsn, FOUR_LAYERS_NATIVE_FILE);
    let manifest: serde_json::Value = serde_json::from_str(&adopted.manifest).unwrap();
    assert!(manifest["padstacks"]
        .as_array()
        .unwrap()
        .iter()
        .any(|padstack| padstack["name"] == "Via[0-3]_600:300_um"));
}

#[test]
fn four_layer_through_hole_pad_must_cover_every_copper_layer() {
    let two_layer_pad = FOUR_LAYERS.replacen(
        r#"(layers "*.Cu" "*.Mask")"#,
        r#"(layers "F.Cu" "B.Cu" "*.Mask")"#,
        1,
    );
    assert_ne!(two_layer_pad, FOUR_LAYERS);
    let error = export_dsn(
        Path::new("board.kicad_pcb"),
        &two_layer_pad,
        &native_fixture_rules(),
    )
    .unwrap_err()
    .to_string();
    assert!(
        error.contains("unsupported through-hole pad J1-1: copper layers are [F.Cu, B.Cu]"),
        "{error}"
    );
    let prep_error = prepare_export(
        Path::new("board.kicad_pcb"),
        &two_layer_pad,
        &native_fixture_rules(),
    )
    .err()
    .expect("partial-layer through-hole pad must be refused before export")
    .to_string();
    assert!(
        prep_error.contains("unsupported through-hole pad J1-1: copper layers are [F.Cu, B.Cu]"),
        "{prep_error}"
    );
    // KiCad saves a pad on every copper layer as `*.Cu`; a list in full
    // is accepted in the ordinal order of pad lists or top to bottom.
    for list in [
        r#""F.Cu" "B.Cu" "In1.Cu" "In2.Cu""#,
        r#""F.Cu" "In1.Cu" "In2.Cu" "B.Cu""#,
    ] {
        let listed = FOUR_LAYERS.replacen(
            r#"(layers "*.Cu" "*.Mask")"#,
            &format!(r#"(layers {list} "*.Mask")"#),
            1,
        );
        assert_ne!(listed, FOUR_LAYERS);
        export_dsn(
            Path::new("board.kicad_pcb"),
            &listed,
            &native_fixture_rules(),
        )
        .unwrap_or_else(|error| panic!("{list}: {error}"));
    }
    // Every layer named, but one of them twice, or one missing.
    for list in [
        r#""F.Cu" "B.Cu" "In1.Cu" "In2.Cu" "In2.Cu""#,
        r#""F.Cu" "B.Cu" "In1.Cu" "In1.Cu""#,
    ] {
        let repeated = FOUR_LAYERS.replacen(
            r#"(layers "*.Cu" "*.Mask")"#,
            &format!(r#"(layers {list} "*.Mask")"#),
            1,
        );
        let error = export_dsn(
            Path::new("board.kicad_pcb"),
            &repeated,
            &native_fixture_rules(),
        )
        .unwrap_err()
        .to_string();
        assert!(
            error.contains("unsupported through-hole pad J1-1: copper layers are"),
            "{list}: {error}"
        );
    }
}

#[test]
fn four_layer_locked_via_must_be_a_through_via() {
    let blind_span = FOUR_LAYERS.replacen(
        "(drill 0.3)\n\t\t(layers \"F.Cu\" \"B.Cu\")",
        "(drill 0.3)\n\t\t(layers \"F.Cu\" \"In1.Cu\")",
        1,
    );
    assert_ne!(blind_span, FOUR_LAYERS);
    let error = export_dsn(
        Path::new("board.kicad_pcb"),
        &blind_span,
        &native_fixture_rules(),
    )
    .unwrap_err()
    .to_string();
    assert!(
        error.contains("unsupported locked via layer span [F.Cu, In1.Cu]; expected [F.Cu, B.Cu]"),
        "{error}"
    );
}

/// Freerouting 2.3.0 routed KiCad's native four-layer export on F.Cu,
/// In1.Cu and In2.Cu with two vias; the strict planner accepts the session, inner
/// layers included.
#[test]
fn freerouting_routes_four_layers_and_the_planner_accepts_it() {
    let temp = tempfile::tempdir().unwrap();
    let board = temp.path().join("specctra_four_layers.kicad_pcb");
    std::fs::write(&board, FOUR_LAYERS).unwrap();
    let baseline = export_dsn(&board, FOUR_LAYERS, &native_fixture_rules()).unwrap();
    let adopted = adopt_native_dsn(baseline, four_layers_native()).unwrap();
    let ses = include_str!(
        "../../tests/fixtures/specctra_four_layers.native-kicad-10.freerouting-2.3.0.ses"
    );
    let plan = crate::specctra_ses::parse_import_plan(&board, FOUR_LAYERS, &adopted.manifest, ses)
        .unwrap();
    let layers = plan
        .tracks
        .iter()
        .map(|track| track.layer.as_str())
        .collect::<BTreeSet<_>>();
    assert_eq!(layers, BTreeSet::from(["F.Cu", "In1.Cu", "In2.Cu"]));
    assert_eq!(plan.vias.len(), 2);

    // A session that names a layer the manifest does not map is refused.
    let stray = ses.replacen("(path In2.Cu", "(path In3.Cu", 1);
    assert_ne!(stray, ses);
    let error =
        crate::specctra_ses::parse_import_plan(&board, FOUR_LAYERS, &adopted.manifest, &stray)
            .unwrap_err()
            .to_string();
    assert!(
        error.contains("SES path uses unknown layer 'In3.Cu'"),
        "{error}"
    );
}

/// The manifest's layer mappings are the count it declares, top to bottom.
#[test]
fn manifest_layer_mappings_must_match_its_copper_layer_count() {
    let temp = tempfile::tempdir().unwrap();
    let board = temp.path().join("specctra_four_layers.kicad_pcb");
    std::fs::write(&board, FOUR_LAYERS).unwrap();
    let export = export_dsn(&board, FOUR_LAYERS, &native_fixture_rules()).unwrap();
    let ses = include_str!(
        "../../tests/fixtures/specctra_four_layers.native-kicad-10.freerouting-2.3.0.ses"
    );
    let adopted = adopt_native_dsn(export, four_layers_native()).unwrap();
    let mut manifest: serde_json::Value = serde_json::from_str(&adopted.manifest).unwrap();
    let mapping = "manifest layer mappings do not match its";
    let profile = "manifest does not describe the supported first routing profile";
    type ManifestEdit = fn(&mut serde_json::Value);
    let cases: [(&str, &str, ManifestEdit); 4] = [
        ("count", mapping, |m| {
            m["supported_profile"]["copper_layers"] = 2.into()
        }),
        // Three layers mapped as F.Cu, In1.Cu, B.Cu: only the count is wrong.
        ("odd", profile, |m| {
            m["supported_profile"]["copper_layers"] = 3.into();
            m["layers"].as_array_mut().unwrap().remove(2);
            m["layers"][2]["index"] = 2.into();
        }),
        // B.Cu unmapped: the session never routes on it.
        ("dropped", mapping, |m| {
            m["layers"].as_array_mut().unwrap().remove(3);
        }),
        ("swapped", mapping, |m| {
            m["layers"][1]["kicad_name"] = "In2.Cu".into();
            m["layers"][2]["kicad_name"] = "In1.Cu".into();
        }),
    ];
    for (name, expected, edit) in cases {
        let mut edited = manifest.clone();
        edit(&mut edited);
        let error =
            crate::specctra_ses::parse_import_plan(&board, FOUR_LAYERS, &edited.to_string(), ses)
                .unwrap_err()
                .to_string();
        assert!(error.contains(expected), "{name}: {error}");
    }
    manifest["schema_version"] = 2.into();
    crate::specctra_ses::parse_import_plan(&board, FOUR_LAYERS, &manifest.to_string(), ses)
        .unwrap();
}

/// Two stock pin headers whose pads share 1.7 mm copper but not a drill
/// (1.0 and 0.8 mm). See its README.
const PADSTACK_DRILLS: &str =
    include_str!("../../tests/fixtures/specctra_padstack_drills.kicad_pcb");
const PADSTACK_DRILLS_NATIVE_FILE: &str =
    include_str!("../../tests/fixtures/specctra_padstack_drills.native-kicad-10.dsn");

fn padstack_drills_native() -> String {
    PADSTACK_DRILLS_NATIVE_FILE.to_string()
}

/// KiCad names both headers' pads after their copper alone: J1's 1.0 mm
/// and J2's 0.8 mm holes share one native padstack per shape. Adoption
/// refuses before routing and names the pads, instead of a manifest that
/// the importer refuses once the board is routed.
#[test]
fn native_padstacks_merged_across_drills_are_refused_before_routing() {
    let error = adopt_native_dsn(native_baseline(PADSTACK_DRILLS), padstack_drills_native())
        .unwrap_err()
        .to_string();
    assert!(
        error.contains("KiCad native DSN gives one padstack 'Rect[A]Pad_1700.000000x1700.000000_um' to pads that the Rust export keeps apart"),
        "{error}"
    );
    assert!(
        error.contains("J2-1 (drill 800 um)") && error.contains("J1-1 (drill 1000 um)"),
        "{error}"
    );
}

/// The same pads in the Rust export: one padstack per shape and drill,
/// and Freerouting 2.3.0's session for that DSN (0 unrouted) imports.
#[test]
fn rust_export_keeps_drills_apart_and_its_session_imports() {
    let temp = tempfile::tempdir().unwrap();
    let board = temp.path().join("specctra_padstack_drills.kicad_pcb");
    std::fs::write(&board, PADSTACK_DRILLS).unwrap();
    let export = export_dsn(&board, PADSTACK_DRILLS, &native_fixture_rules()).unwrap();
    let manifest: Manifest = serde_json::from_str(&export.manifest).unwrap();
    let through = manifest
        .padstacks
        .iter()
        .filter(|padstack| padstack.purpose == "pad")
        .filter_map(|padstack| {
            padstack
                .drill_um
                .map(|drill| (padstack.shape, padstack.size_x_um, drill))
        })
        .collect::<BTreeSet<_>>();
    assert_eq!(
        through,
        BTreeSet::from([
            (PadShape::Circle, 1700, 800),
            (PadShape::Circle, 1700, 1000),
            (PadShape::Rect, 1700, 800),
            (PadShape::Rect, 1700, 1000),
        ])
    );
    let names = manifest
        .padstacks
        .iter()
        .map(|padstack| padstack.name.as_str())
        .collect::<BTreeSet<_>>();
    assert_eq!(names.len(), manifest.padstacks.len());
    let ses = include_str!("../../tests/fixtures/specctra_padstack_drills.freerouting-2.3.0.ses");
    let plan =
        crate::specctra_ses::parse_import_plan(&board, PADSTACK_DRILLS, &export.manifest, ses)
            .unwrap();
    assert!(!plan.tracks.is_empty());
}

/// The four-layer board with a GND plane on In1.Cu, a VCC plane on In2.Cu
/// and a GND pour on F.Cu and B.Cu, filled by KiCad. See its README.
const ZONES: &str = include_str!("../../tests/fixtures/specctra_zones.kicad_pcb");
const ZONES_NATIVE_FILE: &str =
    include_str!("../../tests/fixtures/specctra_zones.native-kicad-10.dsn");
const ZONES_SES: &str =
    include_str!("../../tests/fixtures/specctra_zones.native-kicad-10.freerouting-2.3.0.ses");

fn zones_native() -> String {
    ZONES_NATIVE_FILE.to_string()
}

fn zone_counts(manifest: &serde_json::Value) -> Vec<(String, String, u64)> {
    manifest["zones"]
        .as_array()
        .unwrap()
        .iter()
        .map(|zone| {
            (
                zone["layer"].as_str().unwrap().to_string(),
                zone["net"].as_str().unwrap().to_string(),
                zone["count"].as_u64().unwrap(),
            )
        })
        .collect()
}

#[test]
fn copper_zones_export_as_planes_on_each_layer_they_cover() {
    let export = native_baseline(ZONES);
    assert_eq!(export.copper_zone_count, 3);
    let identity = dsn_identity(&parse_dsn_sexp(&export.dsn).unwrap()).unwrap();
    let square = vec![
        (81000, -31000),
        (129000, -31000),
        (129000, -69000),
        (81000, -69000),
        (81000, -31000),
    ];
    let pour = vec![
        (82000, -60000),
        (95000, -60000),
        (95000, -68000),
        (82000, -68000),
        (82000, -60000),
    ];
    assert_eq!(
        identity.planes,
        [
            ("GND".to_string(), "B.Cu".to_string(), pour.clone()),
            ("GND".to_string(), "F.Cu".to_string(), pour),
            ("GND".to_string(), "In1.Cu".to_string(), square.clone()),
            ("VCC".to_string(), "In2.Cu".to_string(), square),
        ]
    );
    let manifest: serde_json::Value = serde_json::from_str(&export.manifest).unwrap();
    assert_eq!(manifest["schema_version"], MANIFEST_SCHEMA_VERSION);
    assert_eq!(manifest["supported_profile"]["copper_zones"], true);
    let count = |layer: &str, net: &str| (layer.to_string(), net.to_string(), 1);
    assert_eq!(
        zone_counts(&manifest),
        [
            count("B.Cu", "GND"),
            count("F.Cu", "GND"),
            count("In1.Cu", "GND"),
            count("In2.Cu", "VCC"),
        ]
    );
    // A board without zones writes neither planes nor an inventory.
    let plain = native_baseline(FOUR_LAYERS);
    assert_eq!(plain.copper_zone_count, 0);
    assert!(!plain.dsn.contains("(plane "), "{}", plain.dsn);
    let manifest: serde_json::Value = serde_json::from_str(&plain.manifest).unwrap();
    assert_eq!(manifest["supported_profile"]["copper_zones"], false);
    assert!(manifest.get("zones").is_none(), "{manifest}");
}

/// KiCad's own export writes the same planes, in another order.
#[test]
fn zone_native_dsn_matches_the_rust_export() {
    let adopted = adopt_native_dsn(native_baseline(ZONES), zones_native()).unwrap();
    assert_eq!(adopted.dsn, zones_native());
    let manifest: serde_json::Value = serde_json::from_str(&adopted.manifest).unwrap();
    assert_eq!(zone_counts(&manifest).len(), 4);

    let moved = zones_native().replacen("95000 -60000", "95000 -61000", 1);
    assert_ne!(moved, zones_native());
    let renetted = zones_native().replacen("(plane VCC", "(plane GND", 1);
    let mut dropped = zones_native();
    let start = dropped.find("    (plane VCC").unwrap();
    let end = start + dropped[start..].find("))\n").unwrap() + 3;
    dropped.replace_range(start..end, "");
    for (name, native) in [
        ("moved", moved),
        ("renetted", renetted),
        ("dropped", dropped),
    ] {
        let error = adopt_native_dsn(native_baseline(ZONES), native)
            .unwrap_err()
            .to_string();
        assert!(
            error.contains("native DSN changed the copper-zone planes"),
            "{name}: {error}"
        );
    }
}

/// Freerouting 2.3.0 reaches the planes through vias; the planner takes
/// the session and carries the zone inventory to the import's response.
#[test]
fn freerouting_routes_to_the_planes_and_the_planner_keeps_the_zones() {
    let temp = tempfile::tempdir().unwrap();
    let board = temp.path().join("specctra_zones.kicad_pcb");
    std::fs::write(&board, ZONES).unwrap();
    let baseline = export_dsn(&board, ZONES, &native_fixture_rules()).unwrap();
    let adopted = adopt_native_dsn(baseline, zones_native()).unwrap();
    let plan = crate::specctra_ses::parse_import_plan(&board, ZONES, &adopted.manifest, ZONES_SES)
        .unwrap();
    assert!(!plan.vias.is_empty());
    assert_eq!(
        plan.zones
            .iter()
            .map(|zone| (zone.layer.as_str(), zone.net.as_str(), zone.count))
            .collect::<Vec<_>>(),
        [
            ("B.Cu", "GND", 1),
            ("F.Cu", "GND", 1),
            ("In1.Cu", "GND", 1),
            ("In2.Cu", "VCC", 1)
        ]
    );
}

/// The manifest's zone inventory is the live board's, from schema 3.
#[test]
fn manifest_zone_inventory_must_match_the_live_board() {
    let temp = tempfile::tempdir().unwrap();
    let board = temp.path().join("specctra_zones.kicad_pcb");
    std::fs::write(&board, ZONES).unwrap();
    let adopted = adopt_native_dsn(
        export_dsn(&board, ZONES, &native_fixture_rules()).unwrap(),
        zones_native(),
    )
    .unwrap();
    let manifest: serde_json::Value = serde_json::from_str(&adopted.manifest).unwrap();
    let inventory = "manifest copper-zone inventory does not match the live board snapshot";
    let flag = "manifest copper-zone flag does not match its zone inventory";
    type ManifestEdit = fn(&mut serde_json::Value);
    let cases: [(&str, &str, ManifestEdit); 6] = [
        ("count", inventory, |m| m["zones"][0]["count"] = 2.into()),
        ("net", inventory, |m| m["zones"][3]["net"] = "GND".into()),
        ("layer", inventory, |m| {
            m["zones"][2]["layer"] = "In2.Cu".into()
        }),
        ("dropped", inventory, |m| {
            m["zones"].as_array_mut().unwrap().remove(1);
        }),
        ("unflagged", flag, |m| {
            m["supported_profile"]["copper_zones"] = false.into()
        }),
        ("schema 2", flag, |m| m["schema_version"] = 2.into()),
    ];
    for (name, expected, edit) in cases {
        let mut edited = manifest.clone();
        edit(&mut edited);
        let error =
            crate::specctra_ses::parse_import_plan(&board, ZONES, &edited.to_string(), ZONES_SES)
                .unwrap_err()
                .to_string();
        assert!(error.contains(expected), "{name}: {error}");
    }
    // A zone-free manifest against a board with zones.
    let mut emptied = manifest.clone();
    emptied.as_object_mut().unwrap().remove("zones");
    emptied["supported_profile"]["copper_zones"] = false.into();
    let error =
        crate::specctra_ses::parse_import_plan(&board, ZONES, &emptied.to_string(), ZONES_SES)
            .unwrap_err()
            .to_string();
    assert!(error.contains(inventory), "{error}");
}

/// What the planes cannot say exactly is refused, not approximated.
#[test]
fn zones_the_planes_cannot_describe_are_refused() {
    let plane = "(layer \"In1.Cu\")\n\t\t(uuid";
    let outline = "(polygon\n\t\t\t(pts\n\t\t\t\t(xy 81 31)";
    let cases = [
        (
            "netted rule area",
            ZONES.replacen("(name \"gnd_plane\")", "(name \"gnd_plane\") (keepout (tracks not_allowed) (vias allowed))", 1),
            "rule area 'gnd_plane' cannot belong to net 'GND'",
        ),
        (
            "no net",
            ZONES.replacen("(net \"VCC\")\n\t\t(layer \"In2.Cu\")", "(net \"\")\n\t\t(layer \"In2.Cu\")", 1),
            "copper zone 'vcc_plane' has no net",
        ),
        (
            "cutout",
            ZONES.replacen(
                outline,
                "(polygon (pts (xy 100 40) (xy 110 40) (xy 110 45)))\n\t\t(polygon\n\t\t\t(pts\n\t\t\t\t(xy 81 31)",
                1,
            ),
            "copper zone 'gnd_plane' on net 'GND' has 2 outlines",
        ),
        (
            "arc",
            ZONES.replacen(
                "(xy 81 31) (xy 129 31)",
                "(xy 81 31) (arc (start 100 31) (mid 105 30) (end 110 31)) (xy 129 31)",
                1,
            ),
            "outline has a 'arc' point",
        ),
        (
            "not copper",
            ZONES.replacen(plane, "(layer \"F.SilkS\")\n\t\t(uuid", 1),
            "copper zone 'gnd_plane' is on 'F.SilkS', which is not a copper layer of the board",
        ),
        (
            "padless net",
            ZONES.replacen("(net \"VCC\")\n\t\t(layer \"In2.Cu\")", "(net \"SPARE\")\n\t\t(layer \"In2.Cu\")", 1),
            "copper zone on net 'SPARE', which has no pad",
        ),
        (
            "footprint zone",
            ZONES.replacen(
                "(property \"Reference\" \"J1\"",
                "(zone (net \"GND\") (layer \"F.Cu\") (polygon (pts (xy 0 0) (xy 1 0) (xy 1 1))))\n\t\t(property \"Reference\" \"J1\"",
                1,
            ),
            "footprint J1 contains a zone",
        ),
    ];
    for (name, board, expected) in cases {
        assert_ne!(board, ZONES, "{name}");
        let error = export_dsn(
            Path::new("board.kicad_pcb"),
            &board,
            &native_fixture_rules(),
        )
        .unwrap_err()
        .to_string();
        assert!(error.contains(expected), "{name}: {error}");
    }
    // `*.Cu` covers every copper layer, top to bottom.
    let all = ZONES.replacen(plane, "(layers \"*.Cu\")\n\t\t(uuid", 1);
    let export = export_dsn(Path::new("board.kicad_pcb"), &all, &native_fixture_rules()).unwrap();
    let manifest: serde_json::Value = serde_json::from_str(&export.manifest).unwrap();
    assert_eq!(
        zone_counts(&manifest)
            .iter()
            .filter(|(_, net, _)| net == "GND")
            .map(|(layer, _, count)| (layer.as_str(), *count))
            .collect::<Vec<_>>(),
        [("B.Cu", 2), ("F.Cu", 2), ("In1.Cu", 1), ("In2.Cu", 1)]
    );
}

/// A copper layer the board marks `power` is a DSN `power` layer, where
/// Freerouting lays no track, as KiCad 10.0.8 writes it (`mixed` stays
/// `signal`); the native DSN must agree.
#[test]
fn power_layers_export_as_power_layers() {
    let board = ZONES
        .replacen("(4 \"In1.Cu\" signal)", "(4 \"In1.Cu\" power)", 1)
        .replacen("(6 \"In2.Cu\" signal)", "(6 \"In2.Cu\" mixed)", 1);
    assert_ne!(board, ZONES);
    let export = native_baseline(&board);
    let identity = dsn_identity(&parse_dsn_sexp(&export.dsn).unwrap()).unwrap();
    assert_eq!(
        identity.layer_types.values().collect::<Vec<_>>(),
        ["signal", "power", "signal", "signal"]
    );
    let power_native = zones_native().replacen(
        "(layer In1.Cu\n      (type signal)",
        "(layer In1.Cu\n      (type power)",
        1,
    );
    assert_ne!(power_native, zones_native());
    adopt_native_dsn(native_baseline(&board), power_native.clone()).unwrap();
    for (name, baseline, native) in [
        (
            "signal written as power",
            native_baseline(ZONES),
            power_native,
        ),
        (
            "power written as signal",
            native_baseline(&board),
            zones_native(),
        ),
    ] {
        let error = adopt_native_dsn(baseline, native).unwrap_err().to_string();
        assert!(
            error.contains("native DSN changed a copper layer's signal or power type"),
            "{name}: {error}"
        );
    }
}

const BOTTOM_SIDE: &str = include_str!("../../tests/fixtures/specctra_bottom_side.kicad_pcb");
const BOTTOM_SIDE_NATIVE_FILE: &str =
    include_str!("../../tests/fixtures/specctra_bottom_side.native-kicad-10.dsn");
const BOTTOM_SIDE_SES: &str =
    include_str!("../../tests/fixtures/specctra_bottom_side.native-kicad-10.freerouting-2.3.0.ses");

fn bottom_side_native() -> String {
    BOTTOM_SIDE_NATIVE_FILE.to_string()
}

fn bottom_side_board() -> (tempfile::TempDir, std::path::PathBuf, ExportBundle) {
    let temp = tempfile::tempdir().unwrap();
    let board = temp.path().join("specctra_bottom_side.kicad_pcb");
    std::fs::write(&board, BOTTOM_SIDE).unwrap();
    let baseline = export_dsn(&board, BOTTOM_SIDE, &native_fixture_rules()).unwrap();
    let adopted = adopt_native_dsn(baseline, bottom_side_native()).unwrap();
    (temp, board, adopted)
}

/// A bottom footprint is written as KiCad writes it: its front image,
/// SMD pads on F.Cu, placed `back` at KiCad's rotation plus 180, a pin's
/// own turn mirrored. KiCad's native DSN of the same board is adopted
/// unchanged, so both paths give the router the same placement (#430).
#[test]
fn bottom_footprints_are_written_as_kicads_native_export_writes_them() {
    let export = native_baseline(BOTTOM_SIDE);
    for place in [
        "(place R1 100000 -50000 front 0)",
        "(place R2 110000 -50000 back 270)",
        "(place U1 112000 -56000 back 330)",
        "(place R3 104000 -56000 back 315)",
        "(place J1 120000 -45000 back 180)",
    ] {
        assert!(export.dsn.contains(place), "{place}\n{}", export.dsn);
    }
    assert_eq!(export.component_side, "both");
    let native = bottom_side_native();
    let adopted = adopt_native_dsn(export, native.clone()).unwrap();
    assert_eq!(adopted.dsn, BOTTOM_SIDE_NATIVE_FILE);
    let manifest: serde_json::Value = serde_json::from_str(&adopted.manifest).unwrap();
    assert_eq!(manifest["supported_profile"]["component_side"], "both");
    let sides = manifest["components"]
        .as_array()
        .unwrap()
        .iter()
        .map(|component| {
            (
                component["reference"].as_str().unwrap(),
                component["side"].as_str().unwrap(),
                component["rotation_degrees"].as_f64().unwrap(),
            )
        })
        .collect::<Vec<_>>();
    assert_eq!(
        sides,
        [
            ("J1", "back", 180.0),
            ("R1", "front", 0.0),
            ("R2", "back", 270.0),
            ("R3", "back", 315.0),
            ("U1", "back", 330.0),
        ]
    );
}

/// KiCad's front image of the bottom SOT-23W: pin 3 alone on the right,
/// pins 1 and 2 above and below on the left. A mirrored image would put
/// them on the other side, and the native DSN would be refused.
#[test]
fn a_bottom_footprints_image_is_the_unflipped_front_one() {
    let export = native_baseline(BOTTOM_SIDE);
    let rotations = pin_rotations(&export.dsn);
    assert_eq!(rotations[&("R3".to_string(), "2".to_string())], "90");
    for pin in ["1 -1200 950)", "2 -1200 -950)", "3 1200 0)"] {
        assert!(export.dsn.contains(pin), "{pin}\n{}", export.dsn);
    }
    let mirrored = bottom_side_native().replacen("3 1200 0)", "3 -1200 0)", 1);
    let error = adopt_native_dsn(native_baseline(BOTTOM_SIDE), mirrored)
        .unwrap_err()
        .to_string();
    assert!(
        error.contains("native DSN changed pin position for component 'U1' pin '3'"),
        "{error}"
    );
}

/// An SMD pad of a bottom footprint is on B.Cu; one on F.Cu, the other
/// side's copper, is refused rather than moved.
#[test]
fn a_bottom_footprints_smd_pad_must_be_on_b_cu() {
    let wrong_side = BOTTOM_SIDE.replacen(
        r#"(layers "B.Cu" "B.Mask" "B.Paste")"#,
        r#"(layers "F.Cu" "B.Mask" "B.Paste")"#,
        1,
    );
    assert_ne!(wrong_side, BOTTOM_SIDE);
    let error = export_dsn(
        Path::new("board.kicad_pcb"),
        &wrong_side,
        &native_fixture_rules(),
    )
    .unwrap_err()
    .to_string();
    assert!(
        error.contains("unsupported SMD pad") && error.contains("copper layers are [F.Cu]"),
        "{error}"
    );
    // A front footprint's SMD pad on B.Cu is still refused.
    let front_on_back = FOUR_LAYERS.replacen(
        r#"(layers "F.Cu" "F.Mask" "F.Paste")"#,
        r#"(layers "B.Cu" "F.Mask" "F.Paste")"#,
        1,
    );
    let error = export_dsn(
        Path::new("board.kicad_pcb"),
        &front_on_back,
        &native_fixture_rules(),
    )
    .unwrap_err()
    .to_string();
    assert!(error.contains("copper layers are [B.Cu]"), "{error}");
}

/// Freerouting 2.3.0 keeps every bottom placement and routes the bottom
/// pads on B.Cu; the strict planner accepts the session.
#[test]
fn freerouting_routes_bottom_pads_and_the_planner_accepts_it() {
    let (_temp, board, adopted) = bottom_side_board();
    let plan = crate::specctra_ses::parse_import_plan(
        &board,
        BOTTOM_SIDE,
        &adopted.manifest,
        BOTTOM_SIDE_SES,
    )
    .unwrap();
    let back = plan
        .tracks
        .iter()
        .filter(|track| track.layer == "B.Cu")
        .count();
    assert!(back > 0, "{:?}", plan.tracks);
    assert_eq!(plan.vias.len(), 3);
}

/// The SES must keep each footprint's side: a bottom footprint the
/// session puts on the front is refused.
#[test]
fn a_session_that_turns_a_footprint_over_is_refused() {
    let (_temp, board, adopted) = bottom_side_board();
    let flipped = BOTTOM_SIDE_SES.replacen(
        "(place R2 1100000 -500000 back 270)",
        "(place R2 1100000 -500000 front 270)",
        1,
    );
    assert_ne!(flipped, BOTTOM_SIDE_SES);
    let error =
        crate::specctra_ses::parse_import_plan(&board, BOTTOM_SIDE, &adopted.manifest, &flipped)
            .unwrap_err()
            .to_string();
    assert!(
        error.contains("SES changes placement of component 'R2'"),
        "{error}"
    );
}

/// The manifest's sides are checked against the live board and its
/// profile against its components, whichever DSN was routed (#72).
#[test]
fn manifest_sides_must_match_the_board_and_the_profile() {
    let (_temp, board, adopted) = bottom_side_board();
    let manifest: serde_json::Value = serde_json::from_str(&adopted.manifest).unwrap();
    let plan = |manifest: &serde_json::Value| {
        crate::specctra_ses::parse_import_plan(
            &board,
            BOTTOM_SIDE,
            &manifest.to_string(),
            BOTTOM_SIDE_SES,
        )
        .unwrap_err()
        .to_string()
    };

    // R2 recorded on the front, the session too: the board says back.
    let mut front_r2 = manifest.clone();
    let r2 = front_r2["components"]
        .as_array_mut()
        .unwrap()
        .iter_mut()
        .find(|component| component["reference"] == "R2")
        .unwrap();
    r2["side"] = "front".into();
    let error = plan(&front_r2);
    assert!(
        error.contains(
            "routing manifest places R2 on the front side, the live board on the back side"
        ),
        "{error}"
    );

    // A profile that says front while components are on the back.
    let mut front_profile = manifest.clone();
    front_profile["supported_profile"]["component_side"] = "front".into();
    let error = plan(&front_profile);
    assert!(
        error.contains("manifest profile side 'front' does not match its components (both)"),
        "{error}"
    );

    let mut unknown = manifest.clone();
    unknown["supported_profile"]["component_side"] = "top".into();
    let error = plan(&unknown);
    assert!(
        error.contains("manifest does not describe the supported first routing profile"),
        "{error}"
    );
}

/// The profile side is derived from the footprints.
#[test]
fn the_profile_side_is_derived_from_the_footprints() {
    assert_eq!(native_baseline(FOUR_LAYERS).component_side, "front");
    assert_eq!(native_baseline(BOTTOM_SIDE).component_side, "both");
}

/// KiCad's stock TO-252-2 as U1 on the front and TO-263-2 as U2 flipped
/// to the bottom: each has four unnumbered paste-aperture pads over its
/// tab (#458). See its README.
const PASTE_PADS: &str = include_str!("../../tests/fixtures/specctra_paste_pads.kicad_pcb");
const PASTE_PADS_NATIVE_FILE: &str =
    include_str!("../../tests/fixtures/specctra_paste_pads.native-kicad-10.dsn");
const PASTE_PADS_SES: &str =
    include_str!("../../tests/fixtures/specctra_paste_pads.native-kicad-10.freerouting-2.5.0.ses");

fn paste_pads_native() -> String {
    PASTE_PADS_NATIVE_FILE.to_string()
}

fn non_copper_pads(manifest: &serde_json::Value, reference: &str) -> Vec<(String, String)> {
    manifest["components"]
        .as_array()
        .unwrap()
        .iter()
        .find(|component| component["reference"] == reference)
        .unwrap()["non_copper_pads"]
        .as_array()
        .unwrap()
        .iter()
        .map(|pad| {
            (
                pad["pad_number"].as_str().unwrap().to_string(),
                pad["layers"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|layer| layer.as_str().unwrap())
                    .collect::<Vec<_>>()
                    .join(" "),
            )
        })
        .collect()
}

/// A pad on no copper layer is left out of the DSN image, as KiCad's
/// own export does, and listed in the manifest with its layers: front
/// apertures on F.Paste, bottom ones on B.Paste. KiCad's native DSN of
/// the same board is adopted unchanged (#458).
#[test]
fn paste_apertures_stay_out_of_the_dsn_and_in_the_manifest() {
    let export = native_baseline(PASTE_PADS);
    let pins = |image: &str| {
        let start = export.dsn.find(&format!("(image {image}")).unwrap();
        let body = &export.dsn[start..];
        let end = body.find("\n    (image ").unwrap_or(body.len());
        body[..end].matches("(pin ").count()
    };
    assert_eq!(pins("copperpour_image_U1"), 3);
    assert_eq!(pins("copperpour_image_U2"), 3);
    let manifest: serde_json::Value = serde_json::from_str(&export.manifest).unwrap();
    assert_eq!(manifest["schema_version"], MANIFEST_SCHEMA_VERSION);
    let front = vec![(String::new(), "F.Paste".to_string()); 4];
    let back = vec![(String::new(), "B.Paste".to_string()); 4];
    assert_eq!(non_copper_pads(&manifest, "U1"), front);
    assert_eq!(non_copper_pads(&manifest, "U2"), back);
    // A footprint without such pads writes no list.
    let r1 = manifest["components"]
        .as_array()
        .unwrap()
        .iter()
        .find(|component| component["reference"] == "R1")
        .unwrap();
    assert!(r1.get("non_copper_pads").is_none(), "{r1}");
    let native = paste_pads_native();
    let adopted = adopt_native_dsn(export, native.clone()).unwrap();
    assert_eq!(adopted.dsn, native);
}

/// The live board's identities carry the same pads on no copper layer,
/// so the importer compares them with the manifest (#458).
#[test]
fn live_identities_list_the_pads_on_no_copper_layer() {
    let live = board_pin_identities(PASTE_PADS).unwrap();
    let u1 = live
        .values()
        .find(|footprint| footprint.pins.iter().any(|pin| pin.dsn_pin == "U1-2"))
        .unwrap();
    assert_eq!(u1.pins.len(), 3);
    assert_eq!(u1.non_copper_pads.len(), 4);
    assert!(u1
        .non_copper_pads
        .iter()
        .all(|pad| pad.pad_number.is_empty() && pad.layers == ["F.Paste"]));
}

/// Freerouting routes KiCad's native DSN, which has no aperture pads,
/// and the strict planner accepts the session against the manifest.
#[test]
fn a_session_for_a_board_with_paste_apertures_is_accepted() {
    let temp = tempfile::tempdir().unwrap();
    let board = temp.path().join("specctra_paste_pads.kicad_pcb");
    std::fs::write(&board, PASTE_PADS).unwrap();
    let baseline = export_dsn(&board, PASTE_PADS, &native_fixture_rules()).unwrap();
    let adopted = adopt_native_dsn(baseline, paste_pads_native()).unwrap();
    let plan = crate::specctra_ses::parse_import_plan(
        &board,
        PASTE_PADS,
        &adopted.manifest,
        PASTE_PADS_SES,
    )
    .unwrap();
    assert!(!plan.tracks.is_empty());
    assert_eq!(plan.vias.len(), 2);
}

/// Only a pad with no copper at all is set aside: an unnumbered pad
/// with copper is still refused.
#[test]
fn an_unnumbered_pad_with_copper_is_still_refused() {
    let with_copper =
        PASTE_PADS.replacen(r#"(layers "F.Paste")"#, r#"(layers "F.Cu" "F.Paste")"#, 1);
    assert_ne!(with_copper, PASTE_PADS);
    let error = export_dsn(
        Path::new("board.kicad_pcb"),
        &with_copper,
        &native_fixture_rules(),
    )
    .unwrap_err()
    .to_string();
    assert!(
        error.contains("footprint contains an unnumbered pad"),
        "{error}"
    );
}

/// Only an `smd` pad is set aside: a drilled pad on no copper layer
/// (a mounting hole with its copper removed) is still refused.
#[test]
fn a_drilled_pad_on_no_copper_layer_is_still_refused() {
    for pad_type in ["np_thru_hole", "thru_hole"] {
        let drilled = PASTE_PADS.replacen(
            r#"(pad "" smd roundrect"#,
            &format!(r#"(pad "" {pad_type} roundrect"#),
            1,
        );
        assert_ne!(drilled, PASTE_PADS);
        let error = export_dsn(
            Path::new("board.kicad_pcb"),
            &drilled,
            &native_fixture_rules(),
        )
        .unwrap_err()
        .to_string();
        assert!(
            error.contains("footprint contains an unnumbered pad"),
            "{pad_type}: {error}"
        );
    }
}

/// Two pads on no copper layer that share a UUID are refused at export,
/// as the import side refuses them in the manifest.
#[test]
fn a_pad_on_no_copper_layer_with_a_repeated_uuid_is_refused() {
    let repeated = PASTE_PADS.replacen(
        "aed7a5d2-2a7b-418d-86bb-2b666374d871",
        "a6cc1fb6-c476-4559-8318-c7e768ea2507",
        1,
    );
    assert_ne!(repeated, PASTE_PADS);
    let error = export_dsn(
        Path::new("board.kicad_pcb"),
        &repeated,
        &native_fixture_rules(),
    )
    .unwrap_err()
    .to_string();
    assert!(
        error.contains(
            "pad on no copper layer of U1 repeats the UUID a6cc1fb6-c476-4559-8318-c7e768ea2507"
        ),
        "{error}"
    );
}
const NATIVE_CIRCLE: &str = include_str!("../../tests/fixtures/specctra_native_circle.kicad_pcb");
const NATIVE_CIRCLE_DSN: &str =
    include_str!("../../tests/fixtures/specctra_native_circle.native-kicad-10.dsn");
const NATIVE_CIRCLE_SES: &str = include_str!(
    "../../tests/fixtures/specctra_native_circle.native-kicad-10.freerouting-2.5.0.ses"
);

#[test]
fn native_profile_snapshot_identity_matches_existing_rust_semantics() {
    for source in [
        include_str!("../../tests/fixtures/specctra_two_resistors.kicad_pcb"),
        ROUNDED_OUTLINE,
        D_OUTLINE,
        ROTATED,
        FOUR_LAYERS,
        ZONES,
        BOTTOM_SIDE,
        OVAL_PADS,
        PASTE_PADS,
        RULE_AREAS,
    ] {
        let model = prepare_export(Path::new("board.kicad_pcb"), source, &stock_rules()).unwrap();
        let rust = model.export_rust().unwrap();
        let expected = dsn_identity(&parse_dsn_sexp(&rust.dsn).unwrap()).unwrap();
        assert_eq!(model.identity().unwrap(), expected);
    }
}

#[test]
fn native_profile_circle_is_adopted_without_running_rust_outline_and_its_session_is_bound() {
    let temp = tempfile::tempdir().unwrap();
    let board = temp.path().join("board.kicad_pcb");
    std::fs::write(&board, NATIVE_CIRCLE).unwrap();
    let model = prepare_export(&board, NATIVE_CIRCLE, &native_fixture_rules()).unwrap();
    assert!(model
        .export_rust()
        .unwrap_err()
        .to_string()
        .contains("gr_circle"));
    let native = model.adopt_native(NATIVE_CIRCLE_DSN.to_string()).unwrap();
    assert_eq!(native.dsn, NATIVE_CIRCLE_DSN);
    let manifest: serde_json::Value = serde_json::from_str(&native.manifest).unwrap();
    assert!(manifest["supported_profile"]["outline"]
        .as_str()
        .unwrap()
        .contains("KiCad-native"));
    let plan = crate::specctra_ses::parse_import_plan(
        &board,
        NATIVE_CIRCLE,
        &native.manifest,
        NATIVE_CIRCLE_SES,
    )
    .unwrap();
    assert!(!plan.tracks.is_empty());
    let stale = format!("{NATIVE_CIRCLE}\n");
    assert!(crate::specctra_ses::parse_import_plan(
        &board,
        &stale,
        &native.manifest,
        NATIVE_CIRCLE_SES
    )
    .is_err());
    let changed = NATIVE_CIRCLE_DSN.replace("(place R1 100000 -50000", "(place R1 101000 -50000");
    assert_ne!(changed, NATIVE_CIRCLE_DSN);
    assert!(model.adopt_native(changed).is_err());
}

const RULE_AREAS: &str = include_str!("../../tests/fixtures/specctra_rule_areas.kicad_pcb");
const RULE_AREAS_NATIVE_FILE: &str =
    include_str!("../../tests/fixtures/specctra_rule_areas.native-kicad-10.dsn");
const RULE_AREAS_SES: &str =
    include_str!("../../tests/fixtures/specctra_rule_areas.native-kicad-10.freerouting-2.5.0.ses");

#[test]
fn rule_areas_export_as_keepouts_on_each_layer_they_cover() {
    let export = native_baseline(RULE_AREAS);
    assert_eq!(export.rule_area_count, 3);
    let identity = dsn_identity(&parse_dsn_sexp(&export.dsn).unwrap()).unwrap();
    let antenna = vec![
        (113_000, -44_000),
        (117_000, -44_000),
        (117_000, -52_000),
        (113_000, -52_000),
        (113_000, -44_000),
    ];
    let wire_block = vec![
        (106_000, -52_000),
        (109_000, -52_000),
        (109_000, -57_000),
        (106_000, -57_000),
        (106_000, -52_000),
    ];
    let via_block = vec![
        (96_000, -44_000),
        (99_000, -44_000),
        (99_000, -48_000),
        (96_000, -48_000),
        (96_000, -44_000),
    ];
    assert_eq!(
        identity.keepouts,
        [
            (RuleAreaKind::Keepout, "B.Cu".to_string(), antenna.clone()),
            (RuleAreaKind::Keepout, "F.Cu".to_string(), antenna),
            (RuleAreaKind::WireKeepout, "F.Cu".to_string(), wire_block),
            (
                RuleAreaKind::ViaKeepout,
                "B.Cu".to_string(),
                via_block.clone()
            ),
            (RuleAreaKind::ViaKeepout, "F.Cu".to_string(), via_block),
        ]
    );
    let manifest: serde_json::Value = serde_json::from_str(&export.manifest).unwrap();
    assert_eq!(manifest["schema_version"], MANIFEST_SCHEMA_VERSION);
    assert_eq!(manifest["supported_profile"]["rule_areas"], true);
    assert_eq!(manifest["rule_areas"].as_array().unwrap().len(), 3);

    let plain = native_baseline(FOUR_LAYERS);
    assert_eq!(plain.rule_area_count, 0);
    assert!(!plain.dsn.contains("keepout"), "{}", plain.dsn);
    let manifest: serde_json::Value = serde_json::from_str(&plain.manifest).unwrap();
    assert_eq!(manifest["supported_profile"]["rule_areas"], false);
    assert!(manifest.get("rule_areas").is_none(), "{manifest}");
}

#[test]
fn rule_area_native_dsn_matches_the_rust_export() {
    let native_dsn = RULE_AREAS_NATIVE_FILE.to_string();
    let adopted = adopt_native_dsn(native_baseline(RULE_AREAS), native_dsn.clone()).unwrap();
    assert_eq!(adopted.dsn, native_dsn);

    let moved = native_dsn.replacen("117000 -52000", "117000 -53000", 1);
    assert_ne!(moved, native_dsn);
    let retyped = native_dsn.replacen("(wire_keepout ", "(keepout ", 1);
    assert_ne!(retyped, native_dsn);
    let mut dropped = native_dsn.clone();
    let start = dropped.find("    (wire_keepout ").unwrap();
    let end = start + dropped[start..].find("))\n").unwrap() + 3;
    dropped.replace_range(start..end, "");
    let extra = native_dsn.replacen(
        "    (via ",
        "    (keepout \"\" (polygon In1.Cu 0 85000 -35000 86000 -35000 86000 -36000 85000 -35000))\n    (via ",
        1,
    );
    for (name, native) in [
        ("moved", moved),
        ("retyped", retyped),
        ("dropped", dropped),
        ("extra", extra),
    ] {
        let error = adopt_native_dsn(native_baseline(RULE_AREAS), native)
            .unwrap_err()
            .to_string();
        assert!(
            error.contains("native DSN changed the rule-area keep-outs"),
            "{name}: {error}"
        );
    }
}

#[test]
fn freerouting_routes_around_rule_areas_and_the_planner_accepts_it() {
    let temp = tempfile::tempdir().unwrap();
    let board = temp.path().join("specctra_rule_areas.kicad_pcb");
    std::fs::write(&board, RULE_AREAS).unwrap();
    let baseline = export_dsn(&board, RULE_AREAS, &native_fixture_rules()).unwrap();
    let adopted = adopt_native_dsn(baseline, RULE_AREAS_NATIVE_FILE.to_string()).unwrap();
    let plan = crate::specctra_ses::parse_import_plan(
        &board,
        RULE_AREAS,
        &adopted.manifest,
        RULE_AREAS_SES,
    )
    .unwrap();
    assert!(!plan.tracks.is_empty());
    assert!(!plan.vias.is_empty());

    let manifest: serde_json::Value = serde_json::from_str(&adopted.manifest).unwrap();
    let inventory = "manifest rule-area inventory does not match the live board snapshot";
    let flag = "manifest rule-area flag does not match its rule-area inventory";
    type ManifestEdit = fn(&mut serde_json::Value);
    let cases: [(&str, &str, ManifestEdit); 6] = [
        ("kind", inventory, |m| {
            m["rule_areas"][0]["kind"] = "via_keepout".into()
        }),
        ("layer", inventory, |m| {
            m["rule_areas"][0]["layers"][0] = "In1.Cu".into()
        }),
        ("vertex", inventory, |m| {
            m["rule_areas"][0]["outline_um"][0]["x_um"] = 999999.into()
        }),
        ("dropped", inventory, |m| {
            m["rule_areas"].as_array_mut().unwrap().remove(1);
        }),
        ("unflagged", flag, |m| {
            m["supported_profile"]["rule_areas"] = false.into()
        }),
        ("schema 4", flag, |m| m["schema_version"] = 4.into()),
    ];
    for (name, expected, edit) in cases {
        let mut edited = manifest.clone();
        edit(&mut edited);
        let error = crate::specctra_ses::parse_import_plan(
            &board,
            RULE_AREAS,
            &edited.to_string(),
            RULE_AREAS_SES,
        )
        .unwrap_err()
        .to_string();
        assert!(error.contains(expected), "{name}: {error}");
    }
    let mut emptied = manifest.clone();
    emptied.as_object_mut().unwrap().remove("rule_areas");
    emptied["supported_profile"]["rule_areas"] = false.into();
    let error = crate::specctra_ses::parse_import_plan(
        &board,
        RULE_AREAS,
        &emptied.to_string(),
        RULE_AREAS_SES,
    )
    .unwrap_err()
    .to_string();
    assert!(error.contains(inventory), "{error}");
}

#[test]
fn rule_area_vertices_must_be_exact_whole_micrometres() {
    for (mm, expected_um) in [
        (0.0, 0),
        (0.001, 1),
        (0.009, 9),
        (113.001, 113001),
        (-0.009, -9),
        (-44.029, -44029),
    ] {
        assert_eq!(
            exact_rule_area_um(Some(mm), "rule area x").unwrap(),
            expected_um
        );
    }
    for mm in [113.0006, -44.0004, 0.000001, -0.000001] {
        assert!(exact_rule_area_um(Some(mm), "rule area x")
            .unwrap_err()
            .to_string()
            .contains("whole-micrometre grid"));
    }
    // The review's 113.0006-mm edge must not become 113.001 mm: a
    // 0.25-mm track at x=112.876 would then be accepted with 0.4 um of
    // copper inside the original keep-out. Refuse the source snapshot.
    for (from, to, axis) in [
        ("(xy 113 44)", "(xy 113.0006 44)", "x"),
        ("(xy 113 44)", "(xy 113 44.0004)", "y"),
    ] {
        let source = RULE_AREAS.replacen(from, to, 1);
        assert_ne!(source, RULE_AREAS);
        let error = export_dsn(
            Path::new("board.kicad_pcb"),
            &source,
            &native_fixture_rules(),
        )
        .unwrap_err()
        .to_string();
        assert!(error.contains("whole-micrometre grid"), "{error}");
        assert!(
            error.contains(&format!("rule area 'antenna_keepout' {axis}")),
            "{error}"
        );
        assert!(board_rule_area_inventory(&source).is_err());
    }
    // Millimetre fractions that are whole micrometres remain exact in
    // both the DSN and the reverse manifest, including decimal 0.009.
    let source = RULE_AREAS
        .replace("(xy 113 44)", "(xy 113.009 44)")
        .replace("(xy 113 52)", "(xy 113.009 52)");
    let export = export_dsn(
        Path::new("board.kicad_pcb"),
        &source,
        &native_fixture_rules(),
    )
    .unwrap();
    let manifest: serde_json::Value = serde_json::from_str(&export.manifest).unwrap();
    assert_eq!(manifest["rule_areas"][0]["outline_um"][0]["x_um"], 113009);
    assert!(export.dsn.contains("113009 -44000"), "{}", export.dsn);
}

#[test]
fn rule_areas_that_keepouts_cannot_describe_are_refused() {
    let cases = [
        (
            "allows tracks and vias (pour-only)",
            RULE_AREAS.replacen(
                "(tracks not_allowed)\n\t\t\t(vias not_allowed)",
                "(tracks allowed)\n\t\t\t(vias allowed)",
                1,
            ),
            "allows both tracks and vias",
        ),
        (
            "placement enabled",
            RULE_AREAS.replacen("(placement\n\t\t\t(enabled no)", "(placement\n\t\t\t(enabled yes)", 1),
            "enables room placement",
        ),
        (
            "missing vias flag",
            RULE_AREAS.replacen("(vias not_allowed)", "", 1),
            "missing keepout 'vias' rule",
        ),
        (
            "cutout polygon",
            RULE_AREAS.replacen(
                "(polygon\n\t\t\t(pts\n\t\t\t\t(xy 113 44)",
                "(polygon (pts (xy 114 45) (xy 115 45) (xy 115 46)))\n\t\t(polygon\n\t\t\t(pts\n\t\t\t\t(xy 113 44)",
                1,
            ),
            "has 2 outlines",
        ),
        (
            "arc edge",
            RULE_AREAS.replacen(
                "(xy 113 44) (xy 117 44)",
                "(xy 113 44) (arc (start 114 44) (mid 115 43) (end 116 44)) (xy 117 44)",
                1,
            ),
            "outline has a 'arc' point",
        ),
        (
            "non-copper layer",
            RULE_AREAS.replacen(
                "(layers \"F.Cu\" \"B.Cu\")\n\t\t(uuid",
                "(layers \"F.Cu\" \"F.SilkS\")\n\t\t(uuid",
                1,
            ),
            "is on 'F.SilkS', which is not a copper layer of the board",
        ),
        (
            "footprint rule area",
            RULE_AREAS.replacen(
                "(property \"Reference\" \"J1\"",
                "(zone (layer \"F.Cu\") (keepout (tracks not_allowed) (vias allowed)) (polygon (pts (xy 0 0) (xy 1 0) (xy 1 1))))\n\t\t(property \"Reference\" \"J1\"",
                1,
            ),
            "footprint J1 contains a zone",
        ),
    ];
    for (name, board, expected) in cases {
        assert_ne!(board, RULE_AREAS, "{name}");
        let error = export_dsn(
            Path::new("board.kicad_pcb"),
            &board,
            &native_fixture_rules(),
        )
        .unwrap_err()
        .to_string();
        assert!(error.contains(expected), "{name}: {error}");
    }
    // `*.Cu` on a rule area expands across every copper layer in stack order.
    let all_cu = RULE_AREAS.replacen(
        "(zone\n\t\t(layer \"F.Cu\")",
        "(zone\n\t\t(layers \"*.Cu\")",
        1,
    );
    assert_ne!(all_cu, RULE_AREAS);
    let export = export_dsn(
        Path::new("board.kicad_pcb"),
        &all_cu,
        &native_fixture_rules(),
    )
    .unwrap();
    let manifest: serde_json::Value = serde_json::from_str(&export.manifest).unwrap();
    let wire = manifest["rule_areas"]
        .as_array()
        .unwrap()
        .iter()
        .find(|area| area["kind"] == "wire_keepout")
        .unwrap();
    assert_eq!(
        wire["layers"],
        serde_json::json!(["F.Cu", "In1.Cu", "In2.Cu", "B.Cu"])
    );
}
