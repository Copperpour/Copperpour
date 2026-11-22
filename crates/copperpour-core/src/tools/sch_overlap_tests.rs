use super::request;
use crate::{mcp::handler::McpHandler, tools::ServerConfig};
use copperpour_schematic_editor as cse;
use copperpour_sexp::{
    schematic::{LabelKind, SymbolBounds},
    text_bounds::{text_extents, TextOwner},
};
use serde_json::json;

#[cfg(test)]
async fn served_overlaps(source: &[u8]) -> serde_json::Value {
    let result = call_overlap(source, json!({})).await;
    assert_ne!(result["isError"], json!(true), "{result}");
    let body: serde_json::Value =
        serde_json::from_str(result["content"][0]["text"].as_str().unwrap()).unwrap();
    assert_eq!(
        body["overlap_count"],
        body["overlaps"].as_array().unwrap().len(),
        "{body}"
    );
    body
}

async fn call_overlap(source: &[u8], mut arguments: serde_json::Value) -> serde_json::Value {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("sheet.kicad_sch");
    std::fs::write(&path, source).unwrap();
    arguments["schematic"] = json!(path);
    let handler = McpHandler::new(ServerConfig::default()).await.unwrap();
    let loaded = request(
        &handler,
        "tools/call",
        json!({"name": "load_toolset", "arguments": {"name": "sch_readability"}}),
    )
    .await;
    assert_ne!(loaded["isError"], true, "{loaded}");
    let result = request(
        &handler,
        "tools/call",
        json!({"name": "check_schematic_overlaps", "arguments": arguments}),
    )
    .await;
    assert_eq!(std::fs::read(path).unwrap(), source);
    result
}

#[cfg(test)]
mod placement_overlap_tests {
    use super::*;

    /// Retained graphic and pin nodes from KiCad 10's stock Device:R.
    const DEVICE_R: &str = r#"		(symbol "Device:R"
			(symbol "R_0_1"
				(rectangle
					(start -1.016 -2.54)
					(end 1.016 2.54)
					(stroke (width 0.254) (type default))
					(fill (type none))
				)
			)
			(symbol "R_1_1"
				(pin passive line
					(at 0 3.81 270)
					(length 1.27)
					(name "" (effects (font (size 1.27 1.27))))
					(number "1" (effects (font (size 1.27 1.27))))
				)
				(pin passive line
					(at 0 -3.81 90)
					(length 1.27)
					(name "" (effects (font (size 1.27 1.27))))
					(number "2" (effects (font (size 1.27 1.27))))
				)
			)
		)
"#;

    fn schematic(placements: &[(&str, f64, f64)]) -> String {
        let instances = placements
            .iter()
            .map(|(reference, x, y)| {
                format!(
                    "\t(symbol\n\t\t(lib_id \"Device:R\")\n\t\t(at {x} {y} 0)\n\t\t(unit 1)\n\t\t(uuid \"{reference}\")\n\t\t(property \"Reference\" \"{reference}\")\n\t\t(property \"Value\" \"10k\")\n\t)\n"
                )
            })
            .collect::<String>();
        format!(
            "(kicad_sch\n\t(version 20260206)\n\t(generator \"eeschema\")\n\t(lib_symbols\n{DEVICE_R}\t)\n{instances})\n"
        )
    }

    async fn overlaps(source: &str) -> serde_json::Value {
        served_overlaps(source.as_bytes()).await
    }

    #[tokio::test]
    async fn distinct_origins_with_intersecting_symbol_bodies_are_reported() {
        let result = overlaps(&schematic(&[
            ("R1", 100.0, 50.0),
            ("R2", 101.0, 50.0),
            ("R3", 110.0, 50.0),
        ]))
        .await;

        assert_eq!(result["bounds_resolved"], 3);
        assert_eq!(result["bounds_unresolved"], json!([]));
        assert_eq!(result["overlap_count"], 1);
        assert_eq!(result["overlaps"][0]["a"], "R1");
        assert_eq!(result["overlaps"][0]["b"], "R2");
        assert_eq!(result["overlaps"][0]["detection"], "symbol_geometry");
        assert!(result["overlaps"][0]["overlap_x_mm"].as_f64().unwrap() > 1.0);
    }

    #[tokio::test]
    async fn symbols_whose_bounds_only_touch_are_not_collisions() {
        let result = overlaps(&schematic(&[("R1", 100.0, 50.0), ("R2", 102.032, 50.0)])).await;

        assert_eq!(result["overlap_count"], 0, "{result}");
    }

    /// #745 through the served dispatch, on KiCad's own serialization. The
    /// three pin-tip contacts (R10 and U1 VOUT under `+3V3`, R11 over `GND`)
    /// are connections in KiCad's netlist. R12/R13 share body area, `#PWR03`
    /// covers R14's pin, and `#PWR05` sits partway along U1 VIN, which KiCad
    /// leaves unconnected.
    #[tokio::test]
    async fn the_served_dispatch_passes_pin_tip_contact_and_reports_collisions() {
        let body = served_overlaps(include_bytes!(
            "../../tests/fixtures/symbol_pin_contact_kicad10.kicad_sch"
        ))
        .await;

        assert_eq!(body["bounds_resolved"], 11, "{body}");
        let mut pairs = body["overlaps"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|overlap| overlap["type"] == "component_overlap")
            .map(|overlap| {
                assert_eq!(overlap["detection"], "symbol_geometry");
                let mut pair = [
                    overlap["a"].as_str().unwrap(),
                    overlap["b"].as_str().unwrap(),
                ];
                pair.sort_unstable();
                pair
            })
            .collect::<Vec<_>>();
        pairs.sort_unstable();
        assert_eq!(
            pairs,
            [["#PWR03", "R14"], ["#PWR05", "U1"], ["R12", "R13"]],
            "{body}"
        );
        // The rest is text (#111): R12 and R13 overlap, so their fields cross
        // each other and each other's bodies, and the power symbols on U1's
        // pins meet its Value and body.
        let side = |s: &serde_json::Value| {
            let name = s["reference"].as_str().or(s["net"].as_str()).unwrap();
            match s["field"].as_str() {
                Some(field) => format!("{name}.{field}"),
                None => format!("{name} body"),
            }
        };
        let mut texts = body["overlaps"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|overlap| overlap["type"] == "text_overlap")
            .map(|overlap| {
                let mut pair = [side(&overlap["a"]), side(&overlap["b"])];
                pair.sort();
                pair
            })
            .collect::<Vec<_>>();
        texts.sort();
        let expected = [
            ["#PWR04 body", "U1.Value"],
            ["#PWR04.Value", "U1.Value"],
            ["#PWR05.Value", "U1 body"],
            ["R12 body", "R13.Value"],
            ["R12.Reference", "R13 body"],
            ["R12.Reference", "R13.Reference"],
            ["R12.Reference", "R13.Value"],
            ["R12.Value", "R13 body"],
            ["R12.Value", "R13.Value"],
        ]
        .map(|pair| pair.map(String::from));
        assert_eq!(texts, expected, "{body}");
        assert_eq!(body["overlap_count"], 12, "{body}");
    }

    /// #747 on KiCad's own `multichannel` demo sheet, where C12 shifted over
    /// IC2's second unit overlaps its power pin. The extents are the file's
    /// placements plus their library geometry, and the overlap is their
    /// intersection; the old response said `0.7620000000000005` and
    /// `228.60000000000002`.
    #[tokio::test]
    async fn kicad_demo_overlap_reports_file_precision() {
        let sheet = include_str!("../../tests/fixtures/multichannel_channel_strip.kicad_sch")
            .replace("(at 208.28 67.31 0)", "(at 214.63 67.31 0)");
        let result = overlaps(&sheet).await;
        let noisy = crate::tools::noisy_numbers(&result);
        assert!(noisy.is_empty(), "reported {noisy:?}");

        let overlap = &result["overlaps"][0];
        assert_eq!(
            (&overlap["a"], &overlap["b"]),
            (&json!("C12"), &json!("IC2"))
        );
        assert_eq!(overlap["overlap_x_mm"], json!(7.62));
        assert_eq!(overlap["overlap_y_mm"], json!(0.762));
        assert_eq!(
            overlap["bounds_a"],
            json!({ "x_min": 214.63, "y_min": 65.278, "x_max": 222.25, "y_max": 69.342 })
        );
        assert_eq!(
            overlap["bounds_b"],
            json!({ "x_min": 208.28, "y_min": 68.58, "x_max": 228.6, "y_max": 88.9 })
        );
    }

    #[tokio::test]
    async fn invalid_overlap_tolerance_is_a_named_argument_error() {
        let result = call_overlap(
            schematic(&[("R1", 100.0, 50.0)]).as_bytes(),
            json!({"tolerance": -0.1}),
        )
        .await;
        assert_eq!(result["isError"], true, "{result}");
        let result: serde_json::Value =
            serde_json::from_str(result["content"][0]["text"].as_str().unwrap()).unwrap();

        assert_eq!(result["error"]["kind"], "invalid_argument");
        assert_eq!(result["error"]["field"], "tolerance");
    }
}

#[cfg(test)]
mod text_overlap_tests {
    use super::*;
    use copperpour_sexp::{parser::parse_sexp, schematic::parse_at};
    use std::collections::HashSet;

    const SHEET: &str = include_str!("../../tests/fixtures/text_overlap_kicad10.kicad_sch");
    const SVG: &str = include_str!("../../tests/fixtures/text_overlap_kicad10.svg");

    /// One string as kicad-cli plots it: the ink of its drawn strokes, along
    /// the text and across it.
    #[derive(Debug)]
    struct Plotted {
        text: String,
        vertical: bool,
        along: (f64, f64),
        across: (f64, f64),
    }

    fn attr<'a>(tag: &'a str, name: &str) -> Option<&'a str> {
        let key = format!(" {name}=\"");
        let rest = &tag[tag.find(&key)? + key.len()..];
        Some(&rest[..rest.find('"')?])
    }

    /// Half the stroke width of the style in force at `offset` in the plot.
    fn half_stroke(offset: usize) -> f64 {
        let prefix = &SVG[..offset];
        let width = &prefix[prefix.rfind("stroke-width:").unwrap() + 13..];
        width
            .split(';')
            .next()
            .unwrap()
            .trim()
            .parse::<f64>()
            .unwrap()
            / 2.0
    }

    /// A path's points: kicad-cli writes `M x y L x y` for strokes and
    /// `M x,y x,y` for shapes.
    fn path_points(d: &str) -> Vec<(f64, f64)> {
        let numbers = d
            .split(|c: char| c.is_whitespace() || c == ',' || c.is_ascii_alphabetic())
            .filter_map(|n| n.parse::<f64>().ok())
            .collect::<Vec<_>>();
        numbers.chunks(2).map(|p| (p[0], p[1])).collect()
    }

    /// Each invisible `<text>` kicad-cli writes, with the `stroked-text`
    /// group that draws it. A repeat of the same string, anchor and turn
    /// would be the same string; this plot has none.
    fn plotted() -> Vec<Plotted> {
        let mut seen = HashSet::new();
        let mut out = Vec::new();
        let mut rest = SVG;
        while let Some(at) = rest.find("<text ") {
            let before = &rest[..at];
            rest = &rest[at..];
            let tag_end = rest.find('>').unwrap();
            let tag = rest[..tag_end].replace('\n', " ");
            let close = rest.find("</text>").unwrap();
            let text = rest[tag_end + 1..close]
                .replace("&lt;", "<")
                .replace("&gt;", ">")
                .replace("&quot;", "\"")
                .replace("&apos;", "'")
                .replace("&amp;", "&");
            rest = &rest[close..];
            if text.is_empty() {
                continue;
            }
            // The strokes: every point of the group's paths, widened by half
            // the stroke width its style sets.
            let group = &rest[rest.find("<g class=\"stroked-text\">").unwrap()..];
            let group = &group[..group.find("</g>").unwrap()];
            let half = half_stroke(SVG.len() - rest.len());
            let points = group
                .split("d=\"")
                .skip(1)
                .flat_map(|d| path_points(&d[..d.find('"').unwrap()]))
                .collect::<Vec<_>>();
            let span = |axis: fn(&(f64, f64)) -> f64| {
                let values = points.iter().map(axis);
                (
                    values.clone().fold(f64::INFINITY, f64::min) - half,
                    values.fold(f64::NEG_INFINITY, f64::max) + half,
                )
            };
            // A turned string sits in the `rotate(a cx cy)` group opened just
            // before it.
            let turn = before
                .rfind("<g ")
                .map(|g| &before[g..])
                .filter(|g| !g.contains("</g>"))
                .and_then(|g| attr(g, "transform"))
                .and_then(|t| t.strip_prefix("rotate("))
                .map(|t| {
                    t.trim_end_matches(')')
                        .split_whitespace()
                        .map(String::from)
                        .collect::<Vec<_>>()
                });
            if !seen.insert((text.clone(), tag.clone(), turn.clone())) {
                continue;
            }
            out.push(match turn.as_deref() {
                None => Plotted {
                    text,
                    vertical: false,
                    along: span(|p| p.0),
                    across: span(|p| p.1),
                },
                Some([a, _, _]) if a == "-90.000000" => Plotted {
                    text,
                    vertical: true,
                    along: span(|p| p.1),
                    across: span(|p| p.0),
                },
                Some(other) => {
                    panic!("a turn kicad-cli does not write for upright text: {other:?}")
                }
            });
        }
        out
    }

    /// kicad-cli 10.0.6 as the oracle, as #111 asks: every label and visible
    /// field of the fixture is one of the strings its SVG plots, and the
    /// strings it plots beyond them are U1's pin names and numbers. Along the
    /// text and across it, the extent is the ink of KiCad's drawn strokes, to
    /// 0.005 mm.
    #[test]
    fn every_string_sits_where_kicad_cli_plots_it() {
        let tree = parse_sexp(SHEET).unwrap();
        let plotted = plotted();
        let extents = text_extents(&tree);
        let mut used = HashSet::new();
        let near = |(a, b): (f64, f64), (c, d): (f64, f64)| (a - c).abs() + (b - d).abs();
        for e in &extents {
            let r = e.run;
            let split = |p: &Plotted| {
                if p.vertical {
                    ((r.min_y, r.max_y), (r.min_x, r.max_x))
                } else {
                    ((r.min_x, r.max_x), (r.min_y, r.max_y))
                }
            };
            let distance = |p: &Plotted| {
                let (along, across) = split(p);
                near(along, p.along) + near(across, p.across)
            };
            let (index, p) = plotted
                .iter()
                .enumerate()
                .filter(|(_, p)| p.text == e.text)
                .min_by(|(_, p), (_, q)| distance(p).total_cmp(&distance(q)))
                .unwrap_or_else(|| panic!("kicad-cli plots no {:?}", e.text));
            assert!(
                used.insert(index),
                "two extents for one plotted {:?}",
                e.text
            );
            let (along, across) = split(p);
            for (axis, ours, ink) in [("along", along, p.along), ("across", across, p.across)] {
                assert!(
                    (ours.0 - ink.0).abs() < 0.005 && (ours.1 - ink.1).abs() < 0.005,
                    "{:?}: {axis} {ours:?} against the ink {ink:?}",
                    e.text
                );
            }
        }
        assert_eq!(extents.len(), 75);
        for (index, p) in plotted.iter().enumerate() {
            assert!(
                used.contains(&index)
                    || ["+", "-", "1", "2", "3", "5", "6", "7"].contains(&p.text.as_str()),
                "no extent for plotted {p:?}"
            );
        }
    }

    /// The outline of every global label and the flag of every hierarchical
    /// one, as kicad-cli draws them: a closed path at the label's anchor.
    /// A global label's extent is its outline with half its stroke, and what
    /// is compared the outline's own line, each on every edge. A hierarchical
    /// label's extent holds its flag, and what is compared the flag's line,
    /// each meeting it at the anchor.
    #[test]
    fn label_shapes_are_where_kicad_cli_draws_them() {
        let tree = parse_sexp(SHEET).unwrap();
        // Each path with the half stroke width its style sets.
        let mut paths = Vec::new();
        let mut rest = SVG;
        while let Some(at) = rest.find("d=\"M ") {
            let half_pen = half_stroke(SVG.len() - rest.len() + at);
            rest = &rest[at + 3..];
            paths.push((path_points(&rest[..rest.find('"').unwrap()]), half_pen));
        }
        let extents = text_extents(&tree);
        let mut checked = 0;
        for (tag, kind, outline) in [
            ("global_label", LabelKind::GlobalLabel, true),
            ("hierarchical_label", LabelKind::HierarchicalLabel, false),
        ] {
            for node in tree.find_all(tag) {
                let net = node.get(1).and_then(|n| n.as_str()).unwrap();
                let (x, y, _) = parse_at(node).unwrap();
                // An outline starts at its anchor; a flag's path starts at
                // its first template corner, all of them within its reach.
                let (path, half_pen) = paths
                    .iter()
                    .find(|(p, _)| {
                        if outline {
                            (p[0].0 - x).abs() < 1e-3 && (p[0].1 - y).abs() < 1e-3
                        } else {
                            p.len() >= 4
                                && p.iter()
                                    .all(|q| (q.0 - x).abs() < 1.271 && (q.1 - y).abs() < 1.271)
                        }
                    })
                    .unwrap_or_else(|| panic!("no shape drawn at {net}'s anchor"));
                let half_pen = *half_pen;
                let line = SymbolBounds {
                    min_x: path.iter().map(|p| p.0).fold(f64::INFINITY, f64::min),
                    min_y: path.iter().map(|p| p.1).fold(f64::INFINITY, f64::min),
                    max_x: path.iter().map(|p| p.0).fold(f64::NEG_INFINITY, f64::max),
                    max_y: path.iter().map(|p| p.1).fold(f64::NEG_INFINITY, f64::max),
                };
                let drawn = SymbolBounds {
                    min_x: line.min_x - half_pen,
                    min_y: line.min_y - half_pen,
                    max_x: line.max_x + half_pen,
                    max_y: line.max_y + half_pen,
                };
                let extent = extents
                    .iter()
                    .find(|e| {
                        matches!(&e.owner, TextOwner::Label { kind: k, net: n, .. }
                            if *k == kind && n == net)
                    })
                    .unwrap_or_else(|| panic!("no extent for {net}"));
                for (ours, kicad) in [(extent.bounds, drawn), (extent.compared, line)] {
                    let edges = [
                        (ours.min_x, kicad.min_x),
                        (ours.min_y, kicad.min_y),
                        (kicad.max_x, ours.max_x),
                        (kicad.max_y, ours.max_y),
                    ];
                    if outline {
                        let exact = edges.iter().all(|(a, k)| (a - k).abs() < 1e-3);
                        assert!(exact, "{net}: {ours:?} against {kicad:?}");
                    } else {
                        // The flag holds the anchor's side; the text the rest.
                        let holds = edges.iter().all(|(low, high)| *low <= high + 1e-3)
                            && edges.iter().any(|(a, k)| (a - k).abs() < 1e-3);
                        assert!(holds, "{net}: {ours:?} does not hold {kicad:?}");
                    }
                }
                checked += 1;
            }
        }
        assert_eq!(checked, 17);
    }

    /// The fixture's README names each case. Its controls stay clean: the
    /// same pair of labels a gap apart, stock resistors whose Value sits
    /// inside their own body, at each rotation and mirrored `x`, a row of
    /// each label shape, two global labels stacked on the grid, an outline
    /// whose corner meets a body's, two labels end to end whose ink does not
    /// meet, strings with spaces, markup and the micro and ohm signs, and
    /// fields justified right, top and bottom. Two labels of different nets
    /// at one anchor, or within the tolerance, are one collision, which
    /// `label_overlap` already reports; two of one net at one anchor are text
    /// over text.
    #[tokio::test]
    async fn the_fixture_reports_its_collisions_and_nothing_else() {
        let body = served_overlaps(SHEET.as_bytes()).await;
        let side = |s: &serde_json::Value| match s["kind"].as_str().unwrap() {
            "label" => s["net"].as_str().unwrap().to_string(),
            "field" => format!(
                "{}.{}",
                s["reference"].as_str().unwrap(),
                s["field"].as_str().unwrap()
            ),
            _ => format!("{} body", s["reference"].as_str().unwrap()),
        };
        let overlaps = body["overlaps"].as_array().unwrap();
        let (coincident, texts): (Vec<_>, Vec<_>) =
            overlaps.iter().partition(|o| o["type"] == "label_overlap");
        let mut coincident = coincident
            .iter()
            .map(|o| {
                let mut nets = ["net_a", "net_b"].map(|k| o[k].as_str().unwrap());
                nets.sort();
                nets
            })
            .collect::<Vec<_>>();
        coincident.sort();
        assert_eq!(
            coincident,
            [["NEAR_A", "NEAR_B"], ["ONE_A", "ONE_B"]],
            "{body}"
        );
        let mut pairs = texts
            .iter()
            .map(|o| {
                assert_eq!(o["type"], "text_overlap", "{o}");
                let mut pair = [side(&o["a"]), side(&o["b"])];
                pair.sort();
                pair
            })
            .collect::<Vec<_>>();
        pairs.sort();
        let expected = [
            ["#PWR01.Value", "VBUS_SENSE"],
            ["BBBB1", "BBBB2"],
            ["CLK_OUT", "CLK_SRC"],
            ["LONG_NET_NAME_ACROSS", "R3 body"],
            ["LONG_NET_NAME_ACROSS", "R3.Reference"],
            ["LONG_NET_NAME_ACROSS", "R3.Value"],
            ["NRST_CH347", "XO"],
            ["SAME", "SAME"],
            ["SCL_PULLUP", "SDA"],
        ]
        .map(|pair| pair.map(String::from));
        assert_eq!(pairs, expected, "{body}");
        assert_eq!(body["overlap_count"], 11);

        let power = texts
            .iter()
            .flat_map(|o| [&o["a"], &o["b"]])
            .find(|side| side["reference"] == "#PWR01")
            .unwrap();
        assert!(power["symbol_uuid"].is_string(), "{power}");
        let graze = texts
            .iter()
            .find(|o| o["a"]["net"] == "XO" || o["b"]["net"] == "XO")
            .unwrap();
        assert_eq!(graze["a"]["label_type"], "net_label");
        assert!(graze["a"]["uuid"].is_string());
        assert!(graze["overlap_x_mm"].as_f64().unwrap() > 1.0, "{graze}");
        // A global label is reported with its stroke, though it is compared
        // at its line: SDA's outline ends at its anchor, 50.8.
        let sda = texts.iter().find(|o| o["b"]["net"] == "SDA").unwrap();
        assert_eq!(sda["bounds_b"]["x_max"], 50.8762, "{sda}");
        let noisy = crate::tools::noisy_numbers(&body);
        assert!(noisy.is_empty(), "reported {noisy:?}");
    }
}

#[cfg(test)]
mod wire_label_overlap_tests {
    use super::*;
    use serde_json::Value;

    const LABELS: &[u8] = include_bytes!("../../tests/fixtures/wire_over_label_kicad10.kicad_sch");
    const POWER: &[u8] =
        include_bytes!("../../tests/fixtures/wire_over_power_text_kicad10.kicad_sch");
    const VARIANTS: &[u8] =
        include_bytes!("../../tests/fixtures/wire_label_variants_kicad10.kicad_sch");

    fn assert_crossing(
        finding: &Value,
        wire_uuid: &str,
        [x1, y1, x2, y2]: [f64; 4],
        text: Value,
        [left, top, right, bottom]: [f64; 4],
    ) {
        assert_eq!(finding["type"], "wire_label_overlap", "{finding}");
        assert_eq!(
            finding["a"],
            json!({"kind": "wire", "uuid": wire_uuid, "x1": x1, "y1": y1, "x2": x2, "y2": y2})
        );
        assert_eq!(finding["b"], text);
        // The extents bound the ink measured in the KiCad SVG, not the outline.
        let bounds = &finding["bounds_b"];
        for (coordinate, ink, direction) in [
            ("x_min", left, -1.0),
            ("y_min", top, -1.0),
            ("x_max", right, 1.0),
            ("y_max", bottom, 1.0),
        ] {
            let margin = direction * (bounds[coordinate].as_f64().unwrap() - ink);
            assert!((0.0..0.5).contains(&margin), "{coordinate}: {finding}");
        }
    }

    fn label(label_type: &str, net: &str, uuid: &str) -> Value {
        json!({"kind": "label", "label_type": label_type, "net": net, "uuid": uuid, "text": net})
    }

    #[tokio::test]
    async fn local_and_global_text_crossings_exclude_attachments_and_outline_only_crossings() {
        let body = served_overlaps(LABELS).await;
        assert_eq!(body["overlap_count"], 2, "{body}");
        let findings = body["overlaps"].as_array().unwrap();
        let by_net = |net| {
            findings
                .iter()
                .find(|o| o["b"]["net"] == net)
                .unwrap_or_else(|| panic!("missing {net}: {body}"))
        };
        assert_crossing(
            by_net("SIGNAL_INPUT"),
            "ddfda1f2-03eb-4aa5-bbb6-cc27920289a9",
            [55.88, 45.72, 55.88, 49.53],
            label(
                "net_label",
                "SIGNAL_INPUT",
                "c54b569e-d996-4a07-94d1-782cdcd5bfc0",
            ),
            [51.14, 49.02, 63.78, 50.41],
        );
        assert_crossing(
            by_net("GLOBAL_SIGNAL"),
            "97e6a672-ef03-40f3-aafd-fef9c1657218",
            [104.14, 48.26, 104.14, 53.34],
            label(
                "global_label",
                "GLOBAL_SIGNAL",
                "b51d4554-2452-493d-9845-46cc0c95feba",
            ),
            [103.37, 50.20, 117.52, 51.59],
        );
    }

    #[tokio::test]
    async fn reported_text_bounds_use_kicads_six_decimal_precision() {
        let body = served_overlaps(LABELS).await;
        let noisy = crate::tools::noisy_numbers(&body);
        assert!(noisy.is_empty(), "reported {noisy:?}");
    }

    #[tokio::test]
    async fn a_zero_length_wire_inside_the_run_does_not_cross_it() {
        let mut schematic =
            cse::Schematic::from_source("source.kicad_sch", std::str::from_utf8(LABELS).unwrap())
                .unwrap();
        let degenerate = schematic.add_wire(55.88, 49.53, 55.88, 49.53).uuid.clone();
        let body = served_overlaps(schematic.to_source().as_bytes()).await;
        assert!(
            body["overlaps"]
                .as_array()
                .unwrap()
                .iter()
                .all(|o| o["a"]["uuid"] != degenerate),
            "{body}"
        );
    }

    #[tokio::test]
    async fn a_horizontal_baseline_and_tee_do_not_cross_local_label_descenders() {
        let body = served_overlaps(VARIANTS).await;
        let descenders = body["overlaps"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|o| o["b"]["net"] == "VCC(3V3)")
            .collect::<Vec<_>>();
        assert_eq!(descenders.len(), 1, "{body}");
        assert_crossing(
            descenders[0],
            "35ac8957-7fc8-485d-b354-8088d9439b24",
            [55.88, 135.89, 55.88, 138.43],
            label(
                "net_label",
                "VCC(3V3)",
                "f2297762-bc6a-40f0-9ffe-ec0c2a19264e",
            ),
            [51.02, 137.74, 59.43, 139.67],
        );
    }

    #[tokio::test]
    async fn a_vertical_baseline_and_tee_do_not_cross_local_label_subscripts() {
        let body = served_overlaps(VARIANTS).await;
        assert!(
            body["overlaps"]
                .as_array()
                .unwrap()
                .iter()
                .all(|o| o["b"]["net"] != "V_{gs}"),
            "{body}"
        );
    }

    #[tokio::test]
    async fn hierarchical_label_text_crossing_is_reported_but_its_attachment_is_not() {
        let body = served_overlaps(POWER).await;
        let labels = body["overlaps"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|o| o["b"]["kind"] == "label")
            .collect::<Vec<_>>();
        assert_eq!(labels.len(), 1, "{body}");
        assert_crossing(
            labels[0],
            "b8b54e8d-2371-44ae-89e5-460d81aeee1e",
            [80.01, 48.26, 80.01, 53.34],
            label(
                "hierarchical_label",
                "SIG_H",
                "e98278f5-aba7-46f7-a1af-b49c2e5ab2a5",
            ),
            [78.00, 50.11, 82.84, 51.50],
        );
    }

    #[tokio::test]
    async fn power_value_crossing_is_reported_but_its_pin_attachment_and_near_miss_are_not() {
        let body = served_overlaps(POWER).await;
        let other = body["overlaps"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|o| o["b"]["kind"] != "label")
            .collect::<Vec<_>>();
        assert_eq!(other.len(), 1, "{body}");
        assert_crossing(
            other[0],
            "a1dd2242-097c-42f2-81f7-2f296aeebff6",
            [45.72, 46.99, 55.88, 46.99],
            json!({
                "kind": "field", "reference": "#PWR01", "unit": 1,
                "symbol_uuid": "bb8234ac-3887-4cae-8062-7d496f3f5909", "field": "Value", "text": "+3V3"
            }),
            [48.56, 46.55, 53.10, 47.82],
        );
    }

    #[tokio::test]
    async fn rotated_labels_and_a_wire_through_its_own_global_label_are_reported() {
        let body = served_overlaps(VARIANTS).await;
        let labels = body["overlaps"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|o| {
                o["b"]["kind"] == "label"
                    && !matches!(o["b"]["net"].as_str(), Some("VCC(3V3)" | "V_{gs}"))
            })
            .collect::<Vec<_>>();
        assert_eq!(labels.len(), 5, "{body}");
        for (net, kind, label_uuid, wire_uuid, endpoints, ink) in [
            (
                "TURN_UP",
                "net_label",
                "bae663da-893d-4904-9a4c-f2a25ec4d841",
                "6f5e0796-b502-45f8-a08a-663172b19511",
                [48.26, 45.72, 52.07, 45.72],
                [49.02, 42.48, 50.41, 50.58],
            ),
            (
                "FACE_LEFT",
                "global_label",
                "c340cad7-9684-4f01-a2d5-663445cd509f",
                "b3ca56be-e631-4da5-a24d-14f1a9e462e0",
                [97.79, 48.26, 97.79, 53.34],
                [90.58, 50.20, 99.95, 51.59],
            ),
            (
                "TURN_DOWN",
                "hierarchical_label",
                "070a95f8-ff53-4367-a192-5b3d85858119",
                "f8437346-f86c-4eb7-985f-597716c18e2f",
                [149.86, 54.61, 154.94, 54.61],
                [151.71, 52.66, 153.10, 63.49],
            ),
            (
                "THROUGH",
                "global_label",
                "b80124a0-1183-436b-9774-f7190bb60476",
                "671d5905-e0bd-449c-a6dd-7a91d3ba48af",
                [203.2, 50.8, 215.9, 50.8],
                [204.85, 50.20, 213.26, 51.47],
            ),
            (
                "VERT_GLOBAL",
                "global_label",
                "abf9fd68-9ac3-40b3-b6ea-a3a1f496a46a",
                "6f6f1ffa-3b4a-4aaa-9f2b-bbb3baa8fe96",
                [200.66, 96.52, 205.74, 96.52],
                [202.60, 87.73, 203.99, 99.95],
            ),
        ] {
            let finding = labels
                .iter()
                .find(|o| o["b"]["net"] == net)
                .unwrap_or_else(|| panic!("missing {net}: {body}"));
            assert_crossing(
                finding,
                wire_uuid,
                endpoints,
                label(kind, net, label_uuid),
                ink,
            );
        }
    }

    #[tokio::test]
    async fn only_visible_power_value_text_counts_as_label_text() {
        let body = served_overlaps(VARIANTS).await;
        let findings = body["overlaps"].as_array().unwrap();
        let label_fields = findings
            .iter()
            .filter(|o| o["b"]["kind"] == "field" && o["type"] == "wire_label_overlap")
            .collect::<Vec<_>>();
        assert_eq!(label_fields.len(), 1, "{body}");
        assert_crossing(
            label_fields[0],
            "26ff5974-14a7-474a-8459-76984cb68503",
            [99.06, 96.52, 110.49, 96.52],
            json!({
                "kind": "field", "reference": "#PWR01", "unit": 1,
                "symbol_uuid": "884fbc50-49a6-434d-8886-79c88cd4cbad", "field": "Value", "text": "+3V3"
            }),
            [103.17, 95.83, 107.71, 97.10],
        );

        let resistor_fields = findings
            .iter()
            .filter(|o| o["type"] == "wire_field_overlap")
            .collect::<Vec<_>>();
        assert_eq!(resistor_fields.len(), 2, "{body}");
        for (wire, field) in [
            ("92409db3-f11f-4c1f-9db7-37239e7bc06d", "Reference"),
            ("9678b691-6f2d-47e6-bf4c-bd29e527c04e", "Value"),
        ] {
            assert!(
                resistor_fields.iter().any(|o| {
                    o["b"]["reference"] == "R1"
                        && o["a"]["uuid"] == wire
                        && o["b"]["field"] == field
                }),
                "{body}"
            );
        }
    }
}

#[cfg(test)]
#[path = "sch_wire_field_overlap_tests.rs"]
mod wire_field_overlap_tests;

#[cfg(test)]
mod wire_overlap_tests {
    //! `wire_overlap` against `tests/fixtures/wire_through_symbol_kicad10.kicad_sch`;
    //! its README names each case.
    use super::*;

    const SHEET: &[u8] =
        include_bytes!("../../tests/fixtures/wire_through_symbol_kicad10.kicad_sch");

    /// The wire through the crystal, the wire across `R1`, the wire from
    /// `X1`'s pin tip into its body and the wire from the `GND` symbol's tip into
    /// its own drawing are the only `wire_overlap` findings:
    /// not the wires on `R2`'s pin tips, along `Y2`'s top edge, ending on `Y3`'s
    /// edge, beside `R4`, leaving `X1`'s pin tip outward past which its drawing
    /// reaches, or leaving the pin tips of the stock `D1` LED, whose emission
    /// arrows reach past its left tip, outward or square to the pin.
    #[tokio::test]
    async fn only_the_wires_through_a_body_report_wire_overlap() {
        let body = served_overlaps(SHEET).await;
        let mut found = body["overlaps"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|o| o["type"] == "wire_overlap")
            .map(|o| {
                assert_eq!(o["a"]["kind"], "wire", "{o}");
                assert_eq!(o["b"]["kind"], "symbol_body", "{o}");
                assert!(o["b"]["symbol_uuid"].is_string(), "{o}");
                assert!(o["bounds_b"]["x_min"].is_number(), "{o}");
                (
                    o["a"]["uuid"].as_str().unwrap().to_string(),
                    o["b"]["reference"].as_str().unwrap().to_string(),
                )
            })
            .collect::<Vec<_>>();
        found.sort();
        let expected = [
            ("6ca73cc5-a9c5-4da6-b1ad-116b66c7ac4d", "#PWR01"),
            ("7f228b0f-7e2f-4061-a35f-d496b115f231", "R1"),
            ("c3143ef8-21cb-4f0b-a9c8-fe7b58052256", "Y1"),
            ("ff399ce4-c445-46a7-b92c-8ec0839aee42", "X1"),
        ]
        .map(|(uuid, reference)| (uuid.to_string(), reference.to_string()));
        assert_eq!(found, expected, "{body}");

        let mut fields = body["overlaps"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|o| o["type"] == "wire_field_overlap")
            .map(|o| {
                (
                    o["a"]["uuid"].as_str().unwrap(),
                    o["b"]["reference"].as_str().unwrap(),
                    o["b"]["field"].as_str().unwrap(),
                )
            })
            .collect::<Vec<_>>();
        fields.sort_unstable();
        assert_eq!(
            fields,
            [
                ("7f228b0f-7e2f-4061-a35f-d496b115f231", "R1", "Reference"),
                ("7f228b0f-7e2f-4061-a35f-d496b115f231", "R1", "Value"),
                ("c2c87694-21b8-4679-9498-fb356e6e4738", "R4", "Reference"),
                ("e389c34b-85fd-453a-a53c-bcb6f48b1c1c", "X1", "Reference"),
            ],
            "{body}"
        );
        assert_eq!(body["overlap_count"], found.len() + fields.len(), "{body}");

        let through = body["overlaps"]
            .as_array()
            .unwrap()
            .iter()
            .find(|o| o["b"]["reference"] == "Y1")
            .unwrap();
        let wire = &through["a"];
        assert_eq!(
            (wire["x1"].as_f64(), wire["y1"].as_f64()),
            (Some(46.99), Some(50.8))
        );
        assert_eq!(
            (wire["x2"].as_f64(), wire["y2"].as_f64()),
            (Some(54.61), Some(50.8))
        );
    }
}
