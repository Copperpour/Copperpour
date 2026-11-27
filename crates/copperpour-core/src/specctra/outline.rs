//! Build the Rust DSN boundary from the board outline and arc tolerance.

use super::{
    point_um, point_um_f64, DEFAULT_OUTLINE_ARC_ERROR_UM, I64_UPPER_EXCLUSIVE,
    MAX_OUTLINE_ARC_SEGMENTS, UM_PER_MM,
};
use anyhow::{bail, Context, Result};
use copperpour_sexp::SexpNode;
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

pub(super) fn outline_arc_error_um(board_path: &Path, tree: &SexpNode) -> Result<f64> {
    if !tree
        .children()
        .unwrap_or(&[])
        .iter()
        .any(|node| node.head() == Some("gr_arc") && node.find_str("layer") == Some("Edge.Cuts"))
    {
        return Ok(DEFAULT_OUTLINE_ARC_ERROR_UM);
    }
    let project_path = board_path.with_extension("kicad_pro");
    let bytes = match std::fs::read(&project_path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(DEFAULT_OUTLINE_ARC_ERROR_UM);
        }
        Err(error) => {
            return Err(error).with_context(|| format!("read project {}", project_path.display()));
        }
    };
    let project: serde_json::Value = serde_json::from_slice(&bytes)
        .with_context(|| format!("parse project {}", project_path.display()))?;
    let Some(value) = project.pointer("/board/design_settings/rules/max_error") else {
        return Ok(DEFAULT_OUTLINE_ARC_ERROR_UM);
    };
    let error_mm = value.as_f64().with_context(|| {
        format!(
            "project {} board.design_settings.rules.max_error is not a number",
            project_path.display()
        )
    })?;
    let error_um = error_mm * UM_PER_MM;
    if !error_um.is_finite() || error_um <= 0.0 {
        bail!(
            "project {} board.design_settings.rules.max_error must be a positive finite number",
            project_path.display()
        );
    }
    Ok(error_um)
}

pub(super) fn simple_closed_outline(tree: &SexpNode, max_error_um: f64) -> Result<Vec<(i64, i64)>> {
    let mut edges: Vec<Vec<(i64, i64)>> = Vec::new();
    for node in tree.children().unwrap_or(&[]) {
        if node.find_str("layer") != Some("Edge.Cuts") {
            continue;
        }
        match node.head() {
            Some("gr_line") => {
                let start = point_um(node, "start").context("Edge.Cuts line has no start")?;
                let end = point_um(node, "end").context("Edge.Cuts line has no end")?;
                if start == end {
                    bail!("unsupported outline: zero-length Edge.Cuts line");
                }
                edges.push(vec![start, end]);
            }
            Some("gr_arc") => {
                let start = point_um_f64(node, "start").context("Edge.Cuts arc has no start")?;
                let mid = point_um_f64(node, "mid").context("Edge.Cuts arc has no mid")?;
                let end = point_um_f64(node, "end").context("Edge.Cuts arc has no end")?;
                edges.push(outline_arc_points(start, mid, end, max_error_um)?);
            }
            Some(tag) if tag.starts_with("gr_") => {
                bail!("unsupported first routing profile: Edge.Cuts '{tag}' is not a line or circular arc")
            }
            _ => {}
        }
    }
    // Two edges are enough for a D-shaped board (one line, one arc). A single
    // edge fails the degree check below, and the enclosed-area check after the
    // walk refuses two edges that only retrace each other.
    if edges.is_empty() {
        bail!("unsupported outline: no Edge.Cuts lines or arcs");
    }

    // Stable edge order makes the chosen direction deterministic even when
    // KiCad serializes the outline primitives in a different order.
    edges.sort_by(|left, right| {
        let canonical = |points: &[(i64, i64)]| {
            let forward = points.to_vec();
            let reverse = points.iter().rev().copied().collect::<Vec<_>>();
            if forward <= reverse {
                forward
            } else {
                reverse
            }
        };
        canonical(left).cmp(&canonical(right))
    });

    let mut adjacency: BTreeMap<(i64, i64), Vec<usize>> = BTreeMap::new();
    for (index, points) in edges.iter().enumerate() {
        let start = *points.first().context("outline edge has no points")?;
        let end = *points.last().context("outline edge has no points")?;
        adjacency.entry(start).or_default().push(index);
        adjacency.entry(end).or_default().push(index);
    }
    for (point, incident_edges) in &mut adjacency {
        incident_edges.sort_unstable();
        if incident_edges.len() != 2 {
            bail!(
                "unsupported outline: vertex ({}, {}) has degree {}, expected 2",
                point.0,
                point.1,
                incident_edges.len()
            );
        }
    }

    let start = *adjacency.keys().next().context("outline has no vertices")?;
    let mut ordered = vec![start];
    let mut current = start;
    let mut used_edges = BTreeSet::new();
    loop {
        let edge_index = adjacency[&current]
            .iter()
            .copied()
            .find(|index| !used_edges.contains(index))
            .context("unsupported outline: Edge.Cuts is not one closed loop")?;
        used_edges.insert(edge_index);
        let points = &edges[edge_index];
        let directed = if points.first() == Some(&current) {
            points.clone()
        } else if points.last() == Some(&current) {
            points.iter().rev().copied().collect()
        } else {
            bail!("unsupported outline: Edge.Cuts edge is disconnected");
        };
        ordered.extend(directed.into_iter().skip(1));
        current = *ordered.last().context("outline has no points")?;
        if current == start {
            break;
        }
    }
    if used_edges.len() != edges.len() {
        bail!("unsupported outline: Edge.Cuts contains more than one loop");
    }
    // Two straight lines between the same two points, or one arc drawn twice,
    // walk back over themselves: the loop is closed but encloses nothing.
    if edges.len() == 2 {
        let doubled_area: i128 = ordered
            .windows(2)
            .map(|edge| {
                i128::from(edge[0].0) * i128::from(edge[1].1)
                    - i128::from(edge[1].0) * i128::from(edge[0].1)
            })
            .sum();
        if doubled_area == 0 {
            bail!("unsupported outline: two Edge.Cuts edges enclose no area");
        }
    }
    Ok(ordered)
}

/// Flatten a KiCad three-point arc into chords that stay within the board's
/// `max_error`. A 0.707 µm allowance covers rounding the board's input points
/// to the exporter’s micrometre coordinate grid.
pub(super) fn outline_arc_points(
    start: (f64, f64),
    mid: (f64, f64),
    end: (f64, f64),
    max_error_um: f64,
) -> Result<Vec<(i64, i64)>> {
    use std::f64::consts::{PI, TAU};

    if start == end || start == mid || mid == end {
        bail!("unsupported outline: Edge.Cuts arc has coincident points");
    }
    let (bx, by) = (mid.0 - start.0, mid.1 - start.1);
    let (cx, cy) = (end.0 - start.0, end.1 - start.1);
    let cross = bx * cy - by * cx;
    let span = bx.hypot(by).max(cx.hypot(cy)).max((cx - bx).hypot(cy - by));
    if !cross.is_finite() || cross.abs() <= f64::EPSILON * span * span * 16.0 {
        bail!("unsupported outline: Edge.Cuts arc points are collinear");
    }

    // Circumcentre relative to start; using local coordinates avoids loss of
    // precision for boards whose origin is far from the outline.
    let b2 = bx * bx + by * by;
    let c2 = cx * cx + cy * cy;
    let ux = (cy * b2 - by * c2) / (2.0 * cross);
    let uy = (bx * c2 - cx * b2) / (2.0 * cross);
    let radius = ux.hypot(uy);
    if !radius.is_finite() || radius <= 0.0 {
        bail!("unsupported outline: Edge.Cuts arc has no finite radius");
    }

    let angle = |x: f64, y: f64| (y - uy).atan2(x - ux);
    let start_angle = angle(0.0, 0.0);
    let to_mid = (angle(bx, by) - start_angle).rem_euclid(TAU);
    let to_end = (angle(cx, cy) - start_angle).rem_euclid(TAU);
    let sweep = if to_mid <= to_end {
        to_end
    } else {
        to_end - TAU
    };
    let chord_error_um = max_error_um - std::f64::consts::FRAC_1_SQRT_2;
    if !chord_error_um.is_finite() || chord_error_um <= 0.0 {
        bail!("unsupported outline: board max_error is smaller than the DSN coordinate resolution");
    }
    let max_step = if chord_error_um >= radius {
        PI
    } else {
        2.0 * (1.0 - chord_error_um / radius).acos()
    };
    if !max_step.is_finite() || max_step <= 0.0 {
        bail!("unsupported outline: board max_error cannot represent an Edge.Cuts arc");
    }
    let segment_count = (sweep.abs() / max_step).ceil().max(1.0) as usize;
    if segment_count > MAX_OUTLINE_ARC_SEGMENTS {
        bail!(
            "unsupported outline: Edge.Cuts arc needs more than {MAX_OUTLINE_ARC_SEGMENTS} segments"
        );
    }

    let mut points = Vec::with_capacity(segment_count + 1);
    let start_point = (round_um_i64(start.0)?, round_um_i64(start.1)?);
    let end_point = (round_um_i64(end.0)?, round_um_i64(end.1)?);
    points.push(start_point);
    for index in 1..segment_count {
        let theta = start_angle + sweep * index as f64 / segment_count as f64;
        let point = (
            round_um_i64(start.0 + ux + radius * theta.cos())?,
            round_um_i64(start.1 + uy + radius * theta.sin())?,
        );
        if points.last() != Some(&point) {
            points.push(point);
        }
    }
    if points.last() != Some(&end_point) {
        points.push(end_point);
    }
    if points.len() < 2 {
        bail!("unsupported outline: Edge.Cuts arc collapsed at DSN resolution");
    }
    Ok(points)
}

fn round_um_i64(value_um: f64) -> Result<i64> {
    let rounded = value_um.round();
    // `i64::MAX as f64` rounds up to 2^63, so the upper bound is exclusive.
    if !rounded.is_finite() || rounded < i64::MIN as f64 || rounded >= I64_UPPER_EXCLUSIVE {
        bail!("unsupported outline: flattened Edge.Cuts arc exceeds coordinate range");
    }
    Ok(rounded as i64)
}

pub(super) fn closed_outline(outline: &[(i64, i64)]) -> Vec<(i64, i64)> {
    outline.iter().chain(outline.first()).copied().collect()
}
