use super::inventory::{Inventory, Part, Pin, Point};
use super::poses::MIN_WIRE;
use crate::tools::sch_connectivity::{
    kicad_schematic_mm, redraw_net_graph, schematic_point, NetRoot, DRAWN_ATTACHMENT_TOLERANCE,
};
use copperpour_sexp::geometry::point_on_segment;
use copperpour_sexp::schematic::{
    extract_junctions, extract_labels, extract_no_connects, Label, Wire,
};
use copperpour_sexp::SexpNode;
use std::collections::{BTreeMap, BTreeSet};

const EPSILON: f64 = 1e-5;

pub(super) fn coincides(a: Point, b: Point) -> bool {
    schematic_point(a.0, a.1) == schematic_point(b.0, b.1)
}

pub(super) fn endpoints(w: &Wire) -> [Point; 2] {
    [(w.x1, w.y1), (w.x2, w.y2)]
}

pub(super) fn length([a, b]: [Point; 2]) -> f64 {
    (b.0 - a.0).hypot(b.1 - a.1)
}

pub(super) fn on_wire((x, y): Point, [(x1, y1), (x2, y2)]: [Point; 2]) -> bool {
    let [x, y, x1, y1, x2, y2] = [x, y, x1, y1, x2, y2].map(kicad_schematic_mm);
    point_on_segment(x, y, x1, y1, x2, y2, DRAWN_ATTACHMENT_TOLERANCE)
}

pub(super) enum Terminal<'s, 'a> {
    Pin(&'s Part<'a>, &'s Pin),
    Label(&'s Label),
}

impl Terminal<'_, '_> {
    pub fn tip(&self) -> Point {
        match self {
            Self::Pin(_, pin) => pin.tip,
            Self::Label(label) => (label.x, label.y),
        }
    }
}

#[derive(Default)]
pub(super) struct Island<'s, 'a> {
    pub terminals: Vec<Terminal<'s, 'a>>,
    pub wires: Vec<&'s Wire>,
    pub junctions: Vec<Point>,
}

impl Island<'_, '_> {
    pub fn reaches_within(&self, start: Point, end: Point, limit: f64) -> bool {
        let mut edges = BTreeMap::<_, Vec<_>>::new();
        for wire in &self.wires {
            let segment @ [first, last] = endpoints(wire);
            let mut points = vec![first, last];
            // Unlike the connectivity index, this walk must prove a physical
            // route. KiCad does not attach a label offset 0.001 mm from a span.
            points.extend(
                self.junctions
                    .iter()
                    .copied()
                    .chain(self.terminals.iter().filter_map(|terminal| match terminal {
                        Terminal::Label(_) => Some(terminal.tip()),
                        Terminal::Pin(..) => None,
                    }))
                    .filter(|&p| on_wire(p, segment)),
            );
            let saved = |(x, y)| (kicad_schematic_mm(x), kicad_schematic_mm(y));
            let first = saved(first);
            let mut points: Vec<_> = points.into_iter().map(saved).collect();
            points.sort_by(|&a, &b| length([first, a]).total_cmp(&length([first, b])));
            points.dedup_by(|a, b| coincides(*a, *b));
            for adjacent in points.windows(2) {
                let [a, b] = [adjacent[0], adjacent[1]];
                let distance = length([a, b]);
                let (a, b) = (schematic_point(a.0, a.1), schematic_point(b.0, b.1));
                edges.entry(a).or_default().push((b, distance));
                edges.entry(b).or_default().push((a, distance));
            }
        }
        let start = schematic_point(start.0, start.1);
        let finish = schematic_point(end.0, end.1);
        let mut pending = BTreeMap::from([(start, 0.0)]);
        let mut seen = BTreeSet::new();
        while let Some((&point, &distance)) = pending.iter().min_by(|a, b| f64::total_cmp(a.1, b.1))
        {
            if point == finish {
                return true;
            }
            seen.insert(point);
            pending.remove(&point);
            for &(next, leg) in edges.get(&point).into_iter().flatten() {
                let d = distance + leg;
                if d > limit + EPSILON {
                    continue;
                }
                if !seen.contains(&next) {
                    pending
                        .entry(next)
                        .and_modify(|known| *known = known.min(d))
                        .or_insert(d);
                }
            }
        }
        false
    }
}

pub(super) struct Topology<'s, 'a> {
    pub islands: BTreeMap<NetRoot, Island<'s, 'a>>,
    pub no_connects: Vec<Point>,
}

impl<'s, 'a> Topology<'s, 'a> {
    pub fn new(tree: &SexpNode, inventory: &'s Inventory<'a>, labels: &'s [Label]) -> Self {
        let mut drawn = redraw_net_graph(tree, &inventory.wires, labels);
        let mut islands: BTreeMap<NetRoot, Island<'s, 'a>> = BTreeMap::new();
        for part in inventory.parts.values() {
            for pin in &part.pins {
                islands
                    .entry(pin.drawn)
                    .or_default()
                    .terminals
                    .push(Terminal::Pin(part, pin));
            }
        }
        for label in labels {
            let id = drawn.root_at(label.x, label.y);
            islands
                .entry(id)
                .or_default()
                .terminals
                .push(Terminal::Label(label));
        }
        for wire in &inventory.wires {
            let id = drawn.root_at(wire.x1, wire.y1);
            islands.entry(id).or_default().wires.push(wire);
        }
        for (x, y) in extract_junctions(tree) {
            let id = drawn.root_at(x, y);
            islands.entry(id).or_default().junctions.push((x, y));
        }
        Self {
            islands,
            no_connects: extract_no_connects(tree),
        }
    }

    /// The power pin must have no other attachment to redraw. Same-name
    /// islands elsewhere on the sheet do not belong to this lead.
    /// Reject power outputs (including flags) here because this lookup also
    /// authorizes moving a lead without passing through rail removal checks.
    pub fn private_supply(&self, pin: &Pin) -> Option<(&'s Part<'a>, &'s Pin)> {
        let island = self.islands.get(&pin.drawn)?;
        if island.terminals.len() != 2 || !island.junctions.is_empty() {
            return None;
        }
        let [Terminal::Pin(p, a), Terminal::Pin(q, b)] = island.terminals.as_slice() else {
            return None;
        };
        let (supply, supply_pin) = if std::ptr::eq(*a, pin) {
            (*q, *b)
        } else if std::ptr::eq(*b, pin) {
            (*p, *a)
        } else {
            return None;
        };
        if supply.lib.find("power").is_none()
            || supply_pin.lib.electrical_type != "power_in"
            || supply.pins.len() != 1
            || self
                .no_connects
                .iter()
                .any(|m| coincides(*m, pin.tip) || coincides(*m, supply_pin.tip))
        {
            return None;
        }
        match island.wires.as_slice() {
            [] if coincides(pin.tip, supply_pin.tip) => Some((supply, supply_pin)),
            [wire]
                if endpoints(wire).iter().any(|p| coincides(*p, pin.tip))
                    && endpoints(wire)
                        .iter()
                        .any(|p| coincides(*p, supply_pin.tip)) =>
            {
                Some((supply, supply_pin))
            }
            _ => None,
        }
    }
}

/// Only a minimum-length, unbranched lead at an actual supply symbol can be
/// discounted. The other end may be an ordinary pin or the first rail tee.
pub(super) fn mandatory_stubs(tree: &SexpNode, inventory: &Inventory<'_>) -> BTreeSet<String> {
    let labels = extract_labels(tree);
    let topology = Topology::new(tree, inventory, &labels);
    let mut mandatory = BTreeSet::new();
    for part in inventory
        .parts
        .values()
        .filter(|p| p.lib.find("power").is_some())
    {
        let [pin] = part.pins.as_slice() else {
            continue;
        };
        if pin.lib.electrical_type != "power_in" {
            continue;
        }
        let Some(island) = topology.islands.get(&pin.drawn) else {
            continue;
        };
        if island
            .terminals
            .iter()
            .filter(|t| coincides(t.tip(), pin.tip))
            .count()
            != 1
            || island.junctions.iter().any(|p| coincides(*p, pin.tip))
            || topology.no_connects.iter().any(|p| coincides(*p, pin.tip))
        {
            continue;
        }
        let mut leads = island
            .wires
            .iter()
            .filter(|w| on_wire(pin.tip, endpoints(w)));
        let (Some(wire), None) = (leads.next(), leads.next()) else {
            continue;
        };
        let segment = endpoints(wire);
        if (length(segment) - MIN_WIRE).abs() > EPSILON
            || (segment[0].0 != segment[1].0 && segment[0].1 != segment[1].1)
            || !segment.iter().any(|p| coincides(*p, pin.tip))
        {
            continue;
        }
        let end = if coincides(segment[0], pin.tip) {
            segment[1]
        } else {
            segment[0]
        };
        let ordinary_pin = island.terminals.iter().any(|t| {
            matches!(t, Terminal::Pin(p, _) if p.lib.find("power").is_none())
                && coincides(t.tip(), end)
        });
        let tee = island.junctions.iter().any(|p| coincides(*p, end))
            && island
                .wires
                .iter()
                .filter(|w| on_wire(end, endpoints(w)))
                .count()
                >= 3;
        if ordinary_pin || tee {
            mandatory.extend(wire.uuid.iter().cloned());
        }
    }
    mandatory
}
