//! `get_placement_overview`: what a placement looks like, as numbers first.
//!
//! A model reads names and distances reliably and estimates them from a
//! picture only roughly, so the default answer is a compact report: airwire
//! (ratsnest) length overall, per net and per footprint, the longest
//! airwires with the pads at either end, how often airwires of different
//! nets cross, and, for each net group (a netclass, or a group the caller
//! names), where it sits on the board, which other parts sit inside that
//! region and how close other nets' pads come. The picture is optional: an
//! SVG of the outline, courtyards, pads and airwires, rasterized in-process
//! and returned as an image block.
//!
//! Airwires are each net's minimum spanning tree over its pads, the shortest
//! set of straight connections that joins them, which is what KiCad's
//! ratsnest draws for an unrouted net. Routed copper is not considered: this
//! describes the placement, not the routing.

use super::live_or_saved_board;
use crate::mcp::protocol::CallToolResult;
use crate::tool;
use crate::tools::pcb_routing::wildcard_matches;
use crate::tools::{get_path, ToolContext, ToolDef};
use copperpour_sexp::board::{
    board_outline_shape, footprint_courtyards, FootprintCourtyard, PadSite, PcbConnectivityIndex,
};
use serde_json::json;
use std::collections::{BTreeMap, BTreeSet, HashMap};

/// Nets with more pads than this are left out of the airwires by default: a
/// rail reaching dozens of pads draws a web that hides everything else and is
/// poured or routed as a plane anyway.
const DEFAULT_MAX_NET_PADS: usize = 40;
const DEFAULT_TOP: usize = 10;
const MAX_TOP: usize = 50;
/// More airwires than this and the crossing count is skipped, not slowed.
const MAX_CROSSING_EDGES: usize = 4000;
const GROUP_COLORS: [&str; 6] = [
    "#d62728", "#1f77b4", "#2ca02c", "#9467bd", "#ff7f0e", "#8c564b",
];

pub(super) fn tool() -> ToolDef {
    tool!(
        "get_placement_overview",
        "Describe a board's footprint placement as numbers: airwire (ratsnest) length \
         in total, per net and per footprint, the longest airwires with the pads at either \
         end, how many airwires of different nets cross, and for each net group (the \
         project's netclasses, or net_groups you pass, e.g. a high-voltage group) its \
         region, the other parts inside that region, and the closest pads of other nets. \
         Airwires are each net's minimum spanning tree over its pads; routing is ignored. \
         Ground nets and nets with more than max_net_pads pads are left out of the airwires \
         and listed. Reads the board live from KiCad when it is open there, else the saved \
         file. image: true also returns a picture of outline, courtyards, pads and airwires.",
        json!({
            "type": "object",
            "properties": {
                "board": { "type": "string", "description": "Path to .kicad_pcb file" },
                "net_groups": {
                    "type": "object",
                    "additionalProperties": { "type": "array", "items": { "type": "string" } },
                    "description": "Group name to net names or KiCad wildcard patterns, e.g. {\"HV\": [\"HV\", \"D_*\"]}. Default: the project's netclasses other than Default."
                },
                "skip_nets": {
                    "type": "array", "items": { "type": "string" },
                    "description": "Net names or wildcard patterns left out of the airwires. Default: ground nets (GND*, *GND, VSS*, 0V)."
                },
                "max_net_pads": { "type": "integer", "minimum": 2, "description": "Nets with more pads are left out of the airwires. Default 40." },
                "top": { "type": "integer", "minimum": 1, "maximum": 50, "description": "Entries per ranked list. Default 10." },
                "image": { "type": "boolean", "description": "Also return the placement as an image block. Default false." },
                "width_px": { "type": "integer", "minimum": 200, "maximum": 4096, "description": "Image width in pixels. Default 1600." }
            },
            "required": ["board"]
        }),
        |args, ctx| async move { handle_placement_overview(args, ctx).await }
    )
}

/// One airwire: two pads of one net, by index into the net's deduplicated pads.
struct Airwire<'a> {
    net: &'a str,
    a: &'a PadSite,
    b: &'a PadSite,
    length: f64,
}

async fn handle_placement_overview(
    args: &serde_json::Value,
    ctx: &ToolContext,
) -> anyhow::Result<CallToolResult> {
    let board = get_path(args, "board")?;
    let (content, source) = match live_or_saved_board(ctx, &board).await? {
        Ok(read) => read,
        Err(result) => return Ok(result),
    };
    let tree = copperpour_sexp::parse_sexp(&content)?;
    let index = PcbConnectivityIndex::build(&tree);
    let courtyards = footprint_courtyards(&tree).items;
    let outline = board_outline_shape(&tree).bbox();

    let top = args["top"]
        .as_u64()
        .map_or(DEFAULT_TOP, |n| (n as usize).clamp(1, MAX_TOP));
    let max_net_pads = args["max_net_pads"]
        .as_u64()
        .map_or(DEFAULT_MAX_NET_PADS, |n| (n as usize).max(2));
    let skip_patterns: Option<Vec<String>> = args["skip_nets"].as_array().map(|items| {
        items
            .iter()
            .filter_map(|v| v.as_str().map(str::to_string))
            .collect()
    });
    let skipped_by_rule = |net: &str| match &skip_patterns {
        Some(patterns) => patterns.iter().any(|p| names_net(p, net)),
        None => super::is_ground_like_net(net),
    };

    // ── Net groups ──────────────────────────────────────────────────────────
    let (group_patterns, groups_source) = match args["net_groups"].as_object() {
        Some(map) => (
            map.iter()
                .map(|(name, patterns)| {
                    let patterns = patterns
                        .as_array()
                        .map(|items| {
                            items
                                .iter()
                                .filter_map(|v| v.as_str().map(str::to_string))
                                .collect()
                        })
                        .unwrap_or_default();
                    (name.clone(), patterns)
                })
                .collect::<Vec<(String, Vec<String>)>>(),
            "argument",
        ),
        None => (project_netclass_patterns(&board), "netclasses"),
    };
    let nets = index.nets();
    let group_of_net: HashMap<&str, usize> = nets
        .iter()
        .filter_map(|net| {
            group_patterns
                .iter()
                .position(|(_, patterns)| patterns.iter().any(|p| names_net(p, net)))
                .map(|group| (*net, group))
        })
        .collect();

    // ── Airwires ────────────────────────────────────────────────────────────
    let mut airwires: Vec<Airwire> = Vec::new();
    let mut per_net: Vec<(&str, usize, f64)> = Vec::new();
    let mut skipped_nets: Vec<serde_json::Value> = Vec::new();
    for net in &nets {
        let pads = unique_positions(index.pads_of_net(net));
        if pads.len() < 2 {
            continue;
        }
        if skipped_by_rule(net) || pads.len() > max_net_pads {
            skipped_nets.push(json!({ "net": net, "pads": pads.len() }));
            continue;
        }
        let mut length = 0.0;
        for (a, b) in minimum_spanning_tree(&pads) {
            let wire = Airwire {
                net,
                a: pads[a],
                b: pads[b],
                length: distance(pads[a].at, pads[b].at),
            };
            length += wire.length;
            airwires.push(wire);
        }
        per_net.push((net, pads.len(), length));
    }
    let total: f64 = airwires.iter().map(|w| w.length).sum();

    let mut longest: Vec<&Airwire> = airwires.iter().collect();
    longest.sort_by(|x, y| y.length.total_cmp(&x.length));
    let longest: Vec<serde_json::Value> = longest
        .iter()
        .take(top)
        .map(|w| {
            json!({
                "net": w.net,
                "from": pad_name(w.a),
                "to": pad_name(w.b),
                "length_mm": round2(w.length),
            })
        })
        .collect();

    per_net.sort_by(|x, y| y.2.total_cmp(&x.2));
    let longest_nets: Vec<serde_json::Value> = per_net
        .iter()
        .take(top)
        .map(
            |(net, pads, length)| json!({ "net": net, "pads": pads, "length_mm": round2(*length) }),
        )
        .collect();

    // A part whose airwires reach far is usually the one in the wrong place.
    let mut per_footprint: BTreeMap<&str, (usize, f64)> = BTreeMap::new();
    for wire in airwires.iter().filter(|w| w.a.reference != w.b.reference) {
        for reference in [&wire.a.reference, &wire.b.reference] {
            let entry = per_footprint.entry(reference.as_str()).or_default();
            entry.0 += 1;
            entry.1 += wire.length;
        }
    }
    let mut by_footprint: Vec<(&str, usize, f64)> = per_footprint
        .into_iter()
        .map(|(reference, (count, length))| (reference, count, length))
        .collect();
    by_footprint.sort_by(|x, y| y.2.total_cmp(&x.2));
    let farthest_footprints: Vec<serde_json::Value> = by_footprint
        .iter()
        .take(top)
        .map(|(reference, count, length)| {
            json!({
                "reference": reference,
                "airwires": count,
                "length_mm": round2(*length),
                "mean_mm": round2(length / *count as f64),
            })
        })
        .collect();

    let crossings = (airwires.len() <= MAX_CROSSING_EDGES).then(|| count_crossings(&airwires));

    // Parts whose courtyard centre is off the board: still parked, or lost.
    let board_area = outline
        .map(|(x0, y0, x1, y1)| (x1 - x0) * (y1 - y0))
        .filter(|a| *a > 0.0);
    let outside: Vec<&str> = match outline {
        Some((x0, y0, x1, y1)) => courtyards
            .iter()
            .filter_map(|c| {
                let (cx, cy) = ((c.bbox.0 + c.bbox.2) / 2.0, (c.bbox.1 + c.bbox.3) / 2.0);
                let off = cx < x0 || cx > x1 || cy < y0 || cy > y1;
                off.then_some(c.reference.as_deref().unwrap_or("?"))
            })
            .collect(),
        None => Vec::new(),
    };

    // ── Groups ──────────────────────────────────────────────────────────────
    let groups: Vec<serde_json::Value> = group_patterns
        .iter()
        .enumerate()
        .map(|(group, (name, patterns))| {
            describe_group(
                group,
                name,
                patterns,
                &nets,
                &index,
                &group_of_net,
                &courtyards,
                board_area,
                top,
            )
        })
        .collect();

    let mut report = json!({
        "source": source,
        "footprints": courtyards.len(),
        "pads": nets.iter().map(|net| index.pads_of_net(net).len()).sum::<usize>(),
        "outline_bbox": outline.map(bbox_json),
        "outside_outline_count": outside.len(),
        "outside_outline": outside.iter().take(top).collect::<Vec<_>>(),
        "airwires": {
            "total_length_mm": round2(total),
            "count": airwires.len(),
            "nets": per_net.len(),
            "crossings": crossings,
            "longest": longest,
            "longest_nets": longest_nets,
            "farthest_footprints": farthest_footprints,
            "skipped_nets": skipped_nets,
        },
        "groups_source": groups_source,
        "groups": groups,
    });
    if crossings.is_none() {
        report["airwires"]["crossings_note"] =
            json!(format!("not counted above {MAX_CROSSING_EDGES} airwires"));
    }

    let mut result = CallToolResult::json(&report);
    if args["image"].as_bool().unwrap_or(false) {
        let width_px = args["width_px"]
            .as_u64()
            .map_or(1600, |w| w.clamp(200, 4096)) as u32;
        let svg = render_svg(
            &courtyards,
            outline,
            &index,
            &nets,
            &airwires,
            &group_of_net,
        );
        match copperpour_render::svg_to_png(svg.as_bytes(), width_px) {
            Ok(rendered) => {
                use base64::Engine as _;
                let data = base64::engine::general_purpose::STANDARD.encode(&rendered.png);
                result
                    .content
                    .extend(CallToolResult::image(data, "image/png").content);
            }
            Err(error) => {
                report["image_error"] = json!(error.to_string());
                result = CallToolResult::json(&report);
            }
        }
    }
    Ok(result)
}

/// Whether a group or skip pattern names `net`. KiCad names a net local to
/// a sheet by its path (`/Output stage/G_A`), so a pattern also matches the
/// name after the last `/`: `G_A` finds that net, as a caller would expect.
fn names_net(pattern: &str, net: &str) -> bool {
    wildcard_matches(pattern, net)
        || net
            .rsplit_once('/')
            .is_some_and(|(_, short)| wildcard_matches(pattern, short))
}

/// The project's netclasses other than `Default`, each with its patterns.
/// Empty when there is no readable `.kicad_pro` or it assigns no nets.
fn project_netclass_patterns(board: &std::path::Path) -> Vec<(String, Vec<String>)> {
    let Ok(text) = std::fs::read_to_string(board.with_extension("kicad_pro")) else {
        return Vec::new();
    };
    let Ok(settings) = serde_json::from_str::<serde_json::Value>(&text) else {
        return Vec::new();
    };
    let mut by_class: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for entry in settings["net_settings"]["netclass_patterns"]
        .as_array()
        .into_iter()
        .flatten()
    {
        let (Some(class), Some(pattern)) = (entry["netclass"].as_str(), entry["pattern"].as_str())
        else {
            continue;
        };
        if class != "Default" {
            by_class
                .entry(class.to_string())
                .or_default()
                .push(pattern.to_string());
        }
    }
    by_class.into_iter().collect()
}

#[allow(clippy::too_many_arguments)]
fn describe_group(
    group: usize,
    name: &str,
    patterns: &[String],
    nets: &[&str],
    index: &PcbConnectivityIndex,
    group_of_net: &HashMap<&str, usize>,
    courtyards: &[FootprintCourtyard],
    board_area: Option<f64>,
    top: usize,
) -> serde_json::Value {
    let in_group = |net: &str| group_of_net.get(net) == Some(&group);
    let member_nets: Vec<&str> = nets.iter().copied().filter(|net| in_group(net)).collect();
    let group_pads: Vec<&PadSite> = member_nets
        .iter()
        .flat_map(|net| index.pads_of_net(net))
        .collect();
    let members: BTreeSet<&str> = group_pads.iter().map(|p| p.reference.as_str()).collect();

    let region = group_pads
        .iter()
        .fold(None, |acc: Option<(f64, f64, f64, f64)>, pad| {
            let (x, y) = pad.at;
            Some(match acc {
                None => (x, y, x, y),
                Some((x0, y0, x1, y1)) => (x0.min(x), y0.min(y), x1.max(x), y1.max(y)),
            })
        });

    // Parts with no pad on the group's nets whose courtyard centre lies in
    // the group's region: what a separation would have to move.
    let inside: Vec<&str> = match region {
        Some((x0, y0, x1, y1)) => courtyards
            .iter()
            .filter_map(|c| {
                let reference = c.reference.as_deref()?;
                if members.contains(reference) {
                    return None;
                }
                let (cx, cy) = ((c.bbox.0 + c.bbox.2) / 2.0, (c.bbox.1 + c.bbox.3) / 2.0);
                (cx >= x0 && cx <= x1 && cy >= y0 && cy <= y1).then_some(reference)
            })
            .collect(),
        None => Vec::new(),
    };

    // The closest pads of other nets on other parts. A part that carries both
    // (a MOSFET's gate beside its drain) is spaced by its own footprint.
    let mut closest: Vec<(f64, &PadSite, &PadSite, &str)> = Vec::new();
    for other_net in nets.iter().copied().filter(|net| !in_group(net)) {
        for other in index.pads_of_net(other_net) {
            for pad in &group_pads {
                if pad.reference == other.reference {
                    continue;
                }
                closest.push((distance(pad.at, other.at), pad, other, other_net));
            }
        }
    }
    closest.sort_by(|x, y| x.0.total_cmp(&y.0));
    let mut seen_pairs: BTreeSet<(String, String)> = BTreeSet::new();
    let closest: Vec<serde_json::Value> = closest
        .into_iter()
        .filter(|(_, pad, other, _)| seen_pairs.insert((pad_name(pad), pad_name(other))))
        .take(top.min(5))
        .map(|(d, pad, other, other_net)| {
            json!({
                "distance_mm": round2(d),
                "pad": pad_name(pad),
                "net": index.net_of_pad(&pad.reference, &pad.pad_number),
                "other_pad": pad_name(other),
                "other_net": other_net,
            })
        })
        .collect();

    // How much of the board the group spans: near 1 means it is spread over
    // all of it rather than kept to one area.
    let region_share = region
        .zip(board_area)
        .map(|((x0, y0, x1, y1), area)| round2(((x1 - x0) * (y1 - y0) / area).min(1.0)));
    json!({
        "name": name,
        "patterns": patterns,
        "nets": member_nets,
        "footprints": members,
        "region_bbox": region.map(bbox_json),
        "region_share_of_board": region_share,
        "other_parts_inside_region_count": inside.len(),
        "other_parts_inside_region": inside.iter().take(top).collect::<Vec<_>>(),
        "closest_other_net_pads": closest,
    })
}

/// A net's pads with repeated positions dropped: multi-hole pads and stacked
/// pads share a spot and would only add zero-length airwires.
fn unique_positions(pads: &[PadSite]) -> Vec<&PadSite> {
    let mut seen: Vec<(f64, f64)> = Vec::new();
    let mut out = Vec::new();
    for pad in pads {
        if seen.iter().any(|p| distance(*p, pad.at) < 1e-6) {
            continue;
        }
        seen.push(pad.at);
        out.push(pad);
    }
    out
}

/// Prim's algorithm on the complete graph of `pads`: the edges, as index
/// pairs, of the shortest tree joining them.
fn minimum_spanning_tree(pads: &[&PadSite]) -> Vec<(usize, usize)> {
    let n = pads.len();
    let mut in_tree = vec![false; n];
    let mut best = vec![(f64::INFINITY, 0usize); n];
    let mut edges = Vec::with_capacity(n.saturating_sub(1));
    in_tree[0] = true;
    for j in 1..n {
        best[j] = (distance(pads[0].at, pads[j].at), 0);
    }
    for _ in 1..n {
        let Some(next) = (0..n)
            .filter(|&j| !in_tree[j])
            .min_by(|&a, &b| best[a].0.total_cmp(&best[b].0))
        else {
            break;
        };
        in_tree[next] = true;
        edges.push((best[next].1, next));
        for j in 0..n {
            if !in_tree[j] {
                let d = distance(pads[next].at, pads[j].at);
                if d < best[j].0 {
                    best[j] = (d, next);
                }
            }
        }
    }
    edges
}

/// Pairs of airwires of different nets that cross. Touching at an end is not
/// a crossing.
fn count_crossings(airwires: &[Airwire]) -> usize {
    let mut count = 0;
    for (i, a) in airwires.iter().enumerate() {
        for b in &airwires[i + 1..] {
            if a.net != b.net && segments_cross(a.a.at, a.b.at, b.a.at, b.b.at) {
                count += 1;
            }
        }
    }
    count
}

fn segments_cross(p1: (f64, f64), p2: (f64, f64), q1: (f64, f64), q2: (f64, f64)) -> bool {
    let orient = |a: (f64, f64), b: (f64, f64), c: (f64, f64)| {
        (b.0 - a.0) * (c.1 - a.1) - (b.1 - a.1) * (c.0 - a.0)
    };
    let d1 = orient(q1, q2, p1);
    let d2 = orient(q1, q2, p2);
    let d3 = orient(p1, p2, q1);
    let d4 = orient(p1, p2, q2);
    const EPS: f64 = 1e-9;
    ((d1 > EPS && d2 < -EPS) || (d1 < -EPS && d2 > EPS))
        && ((d3 > EPS && d4 < -EPS) || (d3 < -EPS && d4 > EPS))
}

fn distance(a: (f64, f64), b: (f64, f64)) -> f64 {
    ((a.0 - b.0).powi(2) + (a.1 - b.1).powi(2)).sqrt()
}

fn pad_name(pad: &PadSite) -> String {
    format!("{}.{}", pad.reference, pad.pad_number)
}

fn round2(v: f64) -> f64 {
    (v * 100.0).round() / 100.0
}

fn bbox_json((x0, y0, x1, y1): (f64, f64, f64, f64)) -> serde_json::Value {
    json!({ "x_min": round2(x0), "y_min": round2(y0), "x_max": round2(x1), "y_max": round2(y1) })
}

// ─── Picture ─────────────────────────────────────────────────────────────────

fn render_svg(
    courtyards: &[FootprintCourtyard],
    outline: Option<(f64, f64, f64, f64)>,
    index: &PcbConnectivityIndex,
    nets: &[&str],
    airwires: &[Airwire],
    group_of_net: &HashMap<&str, usize>,
) -> String {
    let extent = courtyards
        .iter()
        .map(|c| c.bbox)
        .chain(outline)
        .reduce(|a, b| (a.0.min(b.0), a.1.min(b.1), a.2.max(b.2), a.3.max(b.3)))
        .unwrap_or((0.0, 0.0, 10.0, 10.0));
    let margin = 2.0;
    let (x0, y0) = (extent.0 - margin, extent.1 - margin);
    let (w, h) = (
        (extent.2 - extent.0 + 2.0 * margin).max(1.0),
        (extent.3 - extent.1 + 2.0 * margin).max(1.0),
    );
    let color = |net: &str| {
        group_of_net
            .get(net)
            .map_or("#777777", |g| GROUP_COLORS[g % GROUP_COLORS.len()])
    };
    let footprint_group: HashMap<&str, usize> = nets
        .iter()
        .filter_map(|net| group_of_net.get(net).map(|g| (*net, *g)))
        .flat_map(|(net, g)| {
            index
                .pads_of_net(net)
                .iter()
                .map(move |p| (p.reference.as_str(), g))
        })
        .collect();

    let mut svg = format!(
        "<svg xmlns=\"http://www.w3.org/2000/svg\" viewBox=\"{x0} {y0} {w} {h}\">\
         <rect x=\"{x0}\" y=\"{y0}\" width=\"{w}\" height=\"{h}\" fill=\"#ffffff\"/>"
    );
    if let Some((ox0, oy0, ox1, oy1)) = outline {
        svg += &format!(
            "<rect x=\"{ox0}\" y=\"{oy0}\" width=\"{}\" height=\"{}\" fill=\"none\" \
             stroke=\"#000000\" stroke-width=\"0.3\"/>",
            ox1 - ox0,
            oy1 - oy0
        );
    }
    for c in courtyards {
        let (bx0, by0, bx1, by1) = c.bbox;
        let fill = c
            .reference
            .as_deref()
            .and_then(|r| footprint_group.get(r))
            .map_or("#4a78c2", |g| GROUP_COLORS[g % GROUP_COLORS.len()]);
        svg += &format!(
            "<rect x=\"{bx0}\" y=\"{by0}\" width=\"{}\" height=\"{}\" fill=\"{fill}\" \
             fill-opacity=\"0.22\" stroke=\"#333333\" stroke-width=\"0.06\"/>",
            (bx1 - bx0).max(0.05),
            (by1 - by0).max(0.05)
        );
    }
    for wire in airwires {
        svg += &format!(
            "<line x1=\"{}\" y1=\"{}\" x2=\"{}\" y2=\"{}\" stroke=\"{}\" stroke-width=\"0.12\"/>",
            wire.a.at.0,
            wire.a.at.1,
            wire.b.at.0,
            wire.b.at.1,
            color(wire.net)
        );
    }
    for net in nets {
        for pad in index.pads_of_net(net) {
            svg += &format!(
                "<circle cx=\"{}\" cy=\"{}\" r=\"0.15\" fill=\"{}\"/>",
                pad.at.0,
                pad.at.1,
                color(net)
            );
        }
    }
    for c in courtyards {
        let Some(reference) = c.reference.as_deref() else {
            continue;
        };
        let (bx0, by0, bx1, by1) = c.bbox;
        let height = ((by1 - by0) * 0.45).clamp(0.5, 1.2);
        let width = stroke_text_width(reference, height);
        let (cx, cy) = ((bx0 + bx1) / 2.0, (by0 + by1) / 2.0);
        svg += &format!(
            "<path d=\"{}\" fill=\"none\" stroke=\"#000000\" stroke-width=\"{}\" \
             stroke-linecap=\"round\" stroke-linejoin=\"round\"/>",
            stroke_text_path(reference, cx - width / 2.0, cy - height / 2.0, height),
            height / 9.0
        );
    }
    svg += "</svg>";
    svg
}

/// A minimal stroke font: polylines on a 4 × 6 grid, y down. The renderer
/// loads no fonts on purpose (output must not depend on the machine), so
/// labels are drawn as paths like KiCad's own plots. Lower case is drawn as
/// upper case; anything else as `?`.
fn glyph(c: char) -> &'static [&'static [(i8, i8)]] {
    const BOX: &[(i8, i8)] = &[(0, 0), (4, 0), (4, 6), (0, 6), (0, 0)];
    match c.to_ascii_uppercase() {
        '0' => &[BOX, &[(4, 0), (0, 6)]],
        '1' => &[&[(1, 1), (2, 0), (2, 6)], &[(1, 6), (3, 6)]],
        '2' => &[&[(0, 0), (4, 0), (4, 3), (0, 3), (0, 6), (4, 6)]],
        '3' => &[&[(0, 0), (4, 0), (4, 6), (0, 6)], &[(0, 3), (4, 3)]],
        '4' => &[&[(0, 0), (0, 3), (4, 3)], &[(4, 0), (4, 6)]],
        '5' | 'S' => &[&[(4, 0), (0, 0), (0, 3), (4, 3), (4, 6), (0, 6)]],
        '6' => &[&[(4, 0), (0, 0), (0, 6), (4, 6), (4, 3), (0, 3)]],
        '7' => &[&[(0, 0), (4, 0), (4, 6)]],
        '8' => &[BOX, &[(0, 3), (4, 3)]],
        '9' => &[&[(4, 3), (0, 3), (0, 0), (4, 0), (4, 6), (0, 6)]],
        'A' => &[&[(0, 6), (0, 2), (2, 0), (4, 2), (4, 6)], &[(0, 3), (4, 3)]],
        'B' => &[
            &[(0, 0), (3, 0), (4, 1), (4, 2), (3, 3), (0, 3)],
            &[(3, 3), (4, 4), (4, 5), (3, 6), (0, 6), (0, 0)],
        ],
        'C' => &[&[(4, 0), (0, 0), (0, 6), (4, 6)]],
        'D' => &[&[(0, 0), (3, 0), (4, 1), (4, 5), (3, 6), (0, 6), (0, 0)]],
        'E' => &[&[(4, 0), (0, 0), (0, 6), (4, 6)], &[(0, 3), (3, 3)]],
        'F' => &[&[(4, 0), (0, 0), (0, 6)], &[(0, 3), (3, 3)]],
        'G' => &[&[(4, 0), (0, 0), (0, 6), (4, 6), (4, 3), (2, 3)]],
        'H' => &[&[(0, 0), (0, 6)], &[(4, 0), (4, 6)], &[(0, 3), (4, 3)]],
        'I' => &[&[(1, 0), (3, 0)], &[(2, 0), (2, 6)], &[(1, 6), (3, 6)]],
        'J' => &[&[(4, 0), (4, 6), (0, 6), (0, 4)]],
        'K' => &[&[(0, 0), (0, 6)], &[(4, 0), (0, 3), (4, 6)]],
        'L' => &[&[(0, 0), (0, 6), (4, 6)]],
        'M' => &[&[(0, 6), (0, 0), (2, 3), (4, 0), (4, 6)]],
        'N' => &[&[(0, 6), (0, 0), (4, 6), (4, 0)]],
        'O' => &[BOX],
        'P' => &[&[(0, 6), (0, 0), (4, 0), (4, 3), (0, 3)]],
        'Q' => &[BOX, &[(2, 4), (4, 6)]],
        'R' => &[&[(0, 6), (0, 0), (4, 0), (4, 3), (0, 3)], &[(1, 3), (4, 6)]],
        'T' => &[&[(0, 0), (4, 0)], &[(2, 0), (2, 6)]],
        'U' => &[&[(0, 0), (0, 6), (4, 6), (4, 0)]],
        'V' => &[&[(0, 0), (2, 6), (4, 0)]],
        'W' => &[&[(0, 0), (1, 6), (2, 3), (3, 6), (4, 0)]],
        'X' => &[&[(0, 0), (4, 6)], &[(4, 0), (0, 6)]],
        'Y' => &[&[(0, 0), (2, 3), (4, 0)], &[(2, 3), (2, 6)]],
        'Z' => &[&[(0, 0), (4, 0), (0, 6), (4, 6)]],
        '#' => &[
            &[(1, 0), (1, 6)],
            &[(3, 0), (3, 6)],
            &[(0, 2), (4, 2)],
            &[(0, 4), (4, 4)],
        ],
        '_' => &[&[(0, 6), (4, 6)]],
        '-' => &[&[(0, 3), (4, 3)]],
        '+' => &[&[(0, 3), (4, 3)], &[(2, 1), (2, 5)]],
        '.' => &[&[(2, 5), (2, 6)]],
        '/' => &[&[(0, 6), (4, 0)]],
        ' ' => &[],
        _ => &[
            &[(0, 1), (1, 0), (3, 0), (4, 1), (4, 2), (2, 3), (2, 4)],
            &[(2, 5), (2, 6)],
        ],
    }
}

/// Glyph advance in grid units: 4 wide plus 2 of spacing.
const ADVANCE: f64 = 6.0;

fn stroke_text_width(text: &str, height: f64) -> f64 {
    let n = text.chars().count() as f64;
    (n * ADVANCE - 2.0).max(0.0) * height / 6.0
}

fn stroke_text_path(text: &str, x: f64, y: f64, height: f64) -> String {
    let unit = height / 6.0;
    let mut d = String::new();
    for (i, c) in text.chars().enumerate() {
        let origin = x + i as f64 * ADVANCE * unit;
        for line in glyph(c) {
            for (k, (gx, gy)) in line.iter().enumerate() {
                let (px, py) = (origin + f64::from(*gx) * unit, y + f64::from(*gy) * unit);
                d += &format!("{}{:.3} {:.3} ", if k == 0 { "M" } else { "L" }, px, py);
            }
        }
    }
    d
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pad(reference: &str, number: &str, x: f64, y: f64) -> PadSite {
        PadSite {
            reference: reference.to_string(),
            pad_number: number.to_string(),
            at: (x, y),
            layer_side: copperpour_sexp::board::Side::Front,
            reaches_both_faces: false,
        }
    }

    #[test]
    fn the_spanning_tree_is_the_shortest_join() {
        // Four pads on a line, given out of order: the tree is the three gaps.
        let pads = [
            pad("A", "1", 0.0, 0.0),
            pad("B", "1", 30.0, 0.0),
            pad("C", "1", 10.0, 0.0),
            pad("D", "1", 20.0, 0.0),
        ];
        let refs: Vec<&PadSite> = pads.iter().collect();
        let edges = minimum_spanning_tree(&refs);
        assert_eq!(edges.len(), 3);
        let total: f64 = edges
            .iter()
            .map(|(a, b)| distance(refs[*a].at, refs[*b].at))
            .sum();
        assert!((total - 30.0).abs() < 1e-9, "{total}");
    }

    #[test]
    fn a_pattern_matches_a_sheet_local_net_by_its_short_name() {
        assert!(names_net("G_A", "/Output stage/G_A"));
        assert!(names_net("G_*", "/Output stage/G_A"));
        assert!(names_net("/Output stage/G_A", "/Output stage/G_A"));
        assert!(!names_net("G_A", "/Output stage/G_AB"));
        assert!(!names_net("A", "/Output stage/G_A"));
    }

    #[test]
    fn repeated_positions_add_no_airwire() {
        let pads = [pad("J1", "1", 1.0, 1.0), pad("J1", "1", 1.0, 1.0)];
        assert_eq!(unique_positions(&pads).len(), 1);
    }

    #[test]
    fn crossing_needs_a_proper_intersection() {
        assert!(segments_cross(
            (0.0, 0.0),
            (2.0, 2.0),
            (0.0, 2.0),
            (2.0, 0.0)
        ));
        // Sharing an end, or merely touching, is not a crossing.
        assert!(!segments_cross(
            (0.0, 0.0),
            (2.0, 2.0),
            (2.0, 2.0),
            (4.0, 0.0)
        ));
        assert!(!segments_cross(
            (0.0, 0.0),
            (1.0, 0.0),
            (2.0, 0.0),
            (3.0, 0.0)
        ));
    }

    #[test]
    fn every_reference_character_has_strokes_or_falls_back() {
        for c in "R105#PWR01Q7C_3V3+-./".chars() {
            assert!(!glyph(c).is_empty(), "{c}");
        }
        assert_eq!(glyph('r'), glyph('R'));
        let path = stroke_text_path("R1", 0.0, 0.0, 6.0);
        assert!(path.starts_with("M0.000 6.000 L0.000 0.000"), "{path}");
    }
}
