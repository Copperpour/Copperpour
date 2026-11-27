use super::inventory::{Part, Pin, Point, Role};
use copperpour_sexp::geometry::{rotate_about, round6, PinTransform};
use copperpour_sexp::schematic::{parse_at, pin_endpoint, pin_outward_direction};
use copperpour_sexp::SexpNode;
use serde::Serialize;

pub(super) const GRID: f64 = 1.27;
pub(super) const MIN_WIRE: f64 = 2.54;
pub(super) const CONNECTOR_GAP: f64 = 7.62;
pub(super) const MAX_CONNECTOR_RUN: f64 = 17.78;

#[derive(Clone, Copy, Debug, PartialEq, Serialize)]
pub(super) struct Pose {
    pub x: f64,
    pub y: f64,
    pub rotation: f64,
}

impl Pose {
    pub fn of(part: &Part<'_>) -> Self {
        Self {
            x: part.inst.x,
            y: part.inst.y,
            rotation: part.inst.rotation,
        }
    }

    pub fn transform(self, part: &Part<'_>) -> PinTransform {
        PinTransform {
            comp_x: self.x,
            comp_y: self.y,
            rotation_deg: self.rotation,
            ..part.inst.pin_transform()
        }
    }

    pub fn tip(self, part: &Part<'_>, pin: &Pin) -> Point {
        pin_endpoint(&pin.lib, self.transform(part))
    }

    pub fn with_tip(part: &Part<'_>, pin: &Pin, at: Point, rotation: f64) -> Self {
        let origin = Self {
            x: 0.0,
            y: 0.0,
            rotation,
        };
        let offset = origin.tip(part, pin);
        Self {
            x: round6(at.0 - offset.0),
            y: round6(at.1 - offset.1),
            rotation,
        }
    }

    pub fn on_grid(self, part: &Part<'_>) -> bool {
        part.pins.iter().all(|pin| {
            let (x, y) = self.tip(part, pin);
            [x, y]
                .into_iter()
                .all(|v| (v - (v / GRID).round() * GRID).abs() < 1e-6)
        })
    }

    pub fn relocate(self, part: &Part<'_>) -> Result<SexpNode, String> {
        let mut node = part.node.clone();
        let old = Self::of(part);
        if self == old {
            return Ok(node);
        }
        set_at(&mut node, (self.x, self.y, self.rotation))
            .ok_or_else(|| format!("{} has no editable pose", part.inst.reference))?;
        let rotation = self.rotation - old.rotation;
        let mut fields_changed = false;
        if let SexpNode::List(children) = &mut node {
            for field in children
                .iter_mut()
                .filter(|n| n.head() == Some("property") && n.find("at").is_some())
            {
                let (x, y, angle) = parse_at(field).ok_or_else(|| {
                    format!("{} has an unreadable property pose", part.inst.reference)
                })?;
                let reflected = part.inst.mirror_x ^ part.inst.mirror_y;
                let (x, y) = rotate_about(
                    x - old.x,
                    y - old.y,
                    old.x,
                    old.y,
                    if reflected { -rotation } else { rotation },
                );
                set_at(
                    field,
                    (
                        x + self.x - old.x,
                        y + self.y - old.y,
                        (angle + rotation).rem_euclid(180.0),
                    ),
                );
                fields_changed = true;
            }
            if fields_changed {
                children.retain(|n| n.head() != Some("fields_autoplaced"));
            }
        }
        Ok(node)
    }
}

pub(super) fn set_at(node: &mut SexpNode, (x, y, rotation): (f64, f64, f64)) -> Option<()> {
    let has_rotation = node.head() != Some("no_connect");
    let SexpNode::List(children) = node else {
        return None;
    };
    let SexpNode::List(at) = children.iter_mut().find(|n| n.head() == Some("at"))? else {
        return None;
    };
    at.truncate(1);
    at.extend(
        [x, y]
            .into_iter()
            .chain(has_rotation.then_some(rotation))
            .map(|n| SexpNode::Atom(round6(n).to_string())),
    );
    Some(())
}

pub(super) fn direction(degrees: f64) -> Point {
    match degrees.rem_euclid(360.0) {
        0.0 => (1.0, 0.0),
        90.0 => (0.0, -1.0),
        180.0 => (-1.0, 0.0),
        270.0 => (0.0, 1.0),
        _ => unreachable!("pin directions are snapped to right angles"),
    }
}

pub(super) fn step(at: Point, along: Point, mm: f64) -> Point {
    (round6(at.0 + along.0 * mm), round6(at.1 + along.1 * mm))
}

pub(super) fn span(a: Point, b: Point) -> f64 {
    (a.0 - b.0).abs() + (a.1 - b.1).abs()
}

/// Preferred poses are generated from pin directions, not the part reference
/// or the page coordinates of a fixture.
pub(super) fn local_poses(
    anchor: &Part<'_>,
    part: &Part<'_>,
    role: Role<'_>,
    rail_tap: Option<Point>,
) -> Vec<Pose> {
    let selected = match role {
        Role::Series { near, serves } | Role::Shunt { near, serves, .. } => (near, serves),
        Role::Feedback { input, .. } => input,
        Role::Bypass { supply, serves } => (supply, serves),
    };
    let (Some(near), Some(served)) = (part.pin(selected.0), anchor.pin(selected.1)) else {
        return Vec::new();
    };
    let outward = direction(served.outward);
    let sideways = (-outward.1, outward.0);
    let old = Pose::of(part);
    let rotations = [0.0, 90.0, 180.0, 270.0].map(|turn| (old.rotation + turn).rem_euclid(360.0));
    let input_left = anchor
        .pins
        .iter()
        .any(|p| p.lib.electrical_type == "input" && p.outward == 180.0);
    let mut poses = Vec::new();
    for offset in [0_i32, 1, -1, 2, -2, 3, -3, 4, -4, 5, -5, 6, -6] {
        for distance in 2..=9 {
            let (at, facing) = match role {
                Role::Series { .. } if offset == 0 => (
                    step(served.tip, outward, f64::from(distance) * MIN_WIRE),
                    (served.outward + 180.0).rem_euclid(360.0),
                ),
                Role::Series { .. } => continue,
                Role::Shunt { ground, .. } => {
                    let along = step(served.tip, outward, f64::from(distance) * MIN_WIRE);
                    let along = if ground {
                        along
                    } else {
                        step(along, (0.0, 1.0), 2.0 * MIN_WIRE)
                    };
                    (
                        step(along, sideways, f64::from(offset) * MIN_WIRE),
                        if ground { 90.0 } else { 270.0 },
                    )
                }
                Role::Feedback { output, .. } => {
                    let Some(output) = anchor.pin(output.1) else {
                        continue;
                    };
                    let right = output.tip.0 > served.tip.0;
                    let side = if offset < 0 { -1.0 } else { 1.0 };
                    (
                        (
                            round6(served.tip.0 + f64::from(offset) * GRID),
                            round6(served.tip.1 + side * f64::from(distance) * MIN_WIRE),
                        ),
                        if right { 180.0 } else { 0.0 },
                    )
                }
                Role::Bypass { .. } if rail_tap.is_some() => {
                    if offset == 0 {
                        continue;
                    }
                    let side = if input_left { 1.0 } else { -1.0 };
                    let tap = rail_tap.expect("matched an existing tee");
                    let branch = step(
                        tap,
                        (side * f64::from(offset.signum()), 0.0),
                        f64::from(distance) * MIN_WIRE,
                    );
                    (
                        step(branch, (0.0, 1.0), f64::from(offset.abs()) * MIN_WIRE),
                        90.0,
                    )
                }
                Role::Bypass { .. } if offset != 0 || outward.0 < 0.0 => {
                    let side = if input_left { 1.0 } else { -1.0 };
                    if outward.1 != 0.0 {
                        let lead = step(served.tip, outward, f64::from(distance) * MIN_WIRE);
                        (step(lead, (side, 0.0), f64::from(offset) * MIN_WIRE), 90.0)
                    } else {
                        let lead = step(served.tip, outward, f64::from(distance) * MIN_WIRE);
                        (step(lead, (0.0, 1.0), f64::from(offset) * MIN_WIRE), 90.0)
                    }
                }
                Role::Bypass { .. } => continue,
            };
            for rotation in rotations {
                let pose = Pose::with_tip(part, near, at, rotation);
                let actual = pin_outward_direction(&near.lib, pose.transform(part));
                if facing == actual && pose.on_grid(part) {
                    poses.push(pose);
                }
            }
        }
    }
    if let Some(tap) = rail_tap {
        poses.sort_by(|a, b| span(a.tip(part, near), tap).total_cmp(&span(b.tip(part, near), tap)));
    }
    poses
}
