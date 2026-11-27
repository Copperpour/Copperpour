//! One shared answer to "what is attached at (x, y)".
//!
//! The connectivity tools each used to carry their own tolerance and their own
//! set of items that count as a connection, so they disagreed on real sheets: a
//! wire ending on a hierarchical sheet pin was terminated for `find_orphan_items`
//! and floating for `validate_wire_connections`, and two pins placed directly on
//! each other were connected for the first and unconnected for
//! `validate_component_connections`. The net graph was a fourth answer again,
//! burying its attach tolerance in the function body and knowing nothing about
//! sheet pins. That tolerance is now a property of the [`WireIndex`] the seeder
//! is handed rather than a literal — the value every production caller uses is
//! still [`COINCIDENT_TOLERANCE`], so this is structure, not a behaviour change.
//!
//! One invariant is worth stating, because the two layers are not identical:
//! coincidence is a **superset** of the graph. A point is coincident within the
//! index's tolerance, while [`NetGraph`] joins nodes on exact [`pt_key`]
//! equality plus the attach step. So a wire ending 5 µm off a sheet pin is
//! *terminated* for the validators yet does not reach that pin's net. That is
//! the safe direction and it is deliberate: leniency suppresses a false
//! "floating end" from a rounding artefact, while strictness keeps the graph
//! from inventing a net connection KiCad's own netlister would not make.
//! KiCad snaps to grid, so exact agreement is the normal case.
//!
//! [`ConnectivityIndex`] is built once per tree and holds every item that can
//! terminate a point — wires, wire endpoints, verified wire-side bus-entry
//! corners, labels, pins with the reference that owns them, sheet pins,
//! junctions and no-connects — under a single tolerance, and the tools express
//! policy over it. Bus geometry is indexed for attachment decisions but is
//! intentionally absent from [`NetGraph`]: a bus is a bundle, not one electrical
//! net. One [`seed_net_graph`] is
//! the only definition of the graph, so the ten read-only tools that want net
//! names cannot drift from the three that ask about attachment.

use copperpour_sexp::{
    geometry::{point_on_segment, points_coincident, PinTransform},
    schematic::{
        extract_bus_entries, extract_junctions, extract_no_connects, extract_sheet_pins,
        extract_symbol_instances, find_lib_symbol, pin_endpoint, wire_end_needs_junction, Label,
        LabelKind, LibPin, SymbolInstance, Wire,
    },
    SexpNode,
};
use std::cell::OnceCell;
use std::cmp::Reverse;
use std::collections::{BTreeSet, HashMap, HashSet};

#[cfg(test)]
#[path = "sch_power_driver_tests.rs"]
mod power_driver_tests;

/// The coincidence tolerance the connectivity tools have always used, in mm.
/// `find_orphan_items` takes its own as an argument; the rest use this.
pub(crate) const COINCIDENT_TOLERANCE: f64 = 0.01;

// Less than one KiCad schematic internal unit (0.0001 mm).
pub(super) const DRAWN_ATTACHMENT_TOLERANCE: f64 = 0.00001;
const DRAWN_UNITS_PER_MM: f64 = 10_000.0;

// ─── Spatial indices ──────────────────────────────────────────────────────────

fn bucket(value: f64, tolerance: f64) -> i64 {
    (value / tolerance).floor() as i64
}

/// Points bucketed at the coincidence tolerance, so a lookup probes nine cells
/// instead of scanning every point. `points_coincident` compares an L∞ box of
/// side `tol`, which the 3×3 neighbourhood covers exactly.
///
/// Each point keeps the position it was built at, so a caller holding the slice
/// the index was built from can recover the item itself and not only its
/// coordinates — see [`ConnectivityIndex::point_detail`].
struct PointIndex {
    tol: f64,
    buckets: HashMap<(i64, i64), Vec<IndexedPoint>>,
}

/// An indexed point: where it is, and the position it was built at.
type IndexedPoint = (f64, f64, usize);

impl PointIndex {
    fn build(points: impl IntoIterator<Item = (f64, f64)>, tol: f64) -> Self {
        let mut index = PointIndex {
            tol,
            buckets: HashMap::new(),
        };
        for (ordinal, (x, y)) in points.into_iter().enumerate() {
            let key = index.cell(x, y);
            index.buckets.entry(key).or_default().push((x, y, ordinal));
        }
        index
    }

    fn cell(&self, x: f64, y: f64) -> (i64, i64) {
        ((x / self.tol).floor() as i64, (y / self.tol).floor() as i64)
    }

    /// Every indexed point coinciding with `(x, y)`, as `(x, y, build ordinal)`.
    /// Yielded in bucket order, which is not build order: two coincident points
    /// can fall in different cells, so a caller that reports them must sort on
    /// the ordinal rather than inherit whichever cell was probed first.
    fn hits_at(&self, x: f64, y: f64) -> impl Iterator<Item = IndexedPoint> + '_ {
        let (cx, cy) = self.cell(x, y);
        (-1..=1)
            .flat_map(move |dx| (-1..=1).map(move |dy| (cx + dx, cy + dy)))
            .filter_map(move |key| self.buckets.get(&key))
            .flatten()
            .copied()
            .filter(move |(px, py, _)| points_coincident(x, y, *px, *py, self.tol))
    }

    /// How many indexed points coincide with `(x, y)`.
    fn count_at(&self, x: f64, y: f64) -> usize {
        self.hits_at(x, y).count()
    }

    fn contains(&self, x: f64, y: f64) -> bool {
        self.hits_at(x, y).next().is_some()
    }
}

/// Wires bucketed by the coordinate they hold constant: a horizontal wire can
/// only be met in its own row, a vertical one in its own column. Mirrors
/// `point_on_segment`, which answers `false` for anything diagonal.
pub(super) struct WireIndex<'a> {
    tol: f64,
    normalize: fn(f64) -> f64,
    rows: HashMap<i64, Vec<&'a Wire>>,
    columns: HashMap<i64, Vec<&'a Wire>>,
}

impl<'a> WireIndex<'a> {
    fn tolerance(&self) -> f64 {
        self.tol
    }

    pub(super) fn build(wires: &'a [Wire], tol: f64) -> Self {
        Self::with_coordinates(wires, tol, std::convert::identity)
    }

    fn with_coordinates(wires: &'a [Wire], tol: f64, normalize: fn(f64) -> f64) -> Self {
        let mut index = WireIndex {
            tol,
            normalize,
            rows: HashMap::new(),
            columns: HashMap::new(),
        };
        for wire in wires {
            let [x1, y1, x2, y2] = [wire.x1, wire.y1, wire.x2, wire.y2].map(normalize);
            if (x1 - x2).abs() < tol {
                index.columns.entry(bucket(x1, tol)).or_default().push(wire);
            } else if (y1 - y2).abs() < tol {
                index.rows.entry(bucket(y1, tol)).or_default().push(wire);
            }
        }
        index
    }

    /// Every wire that could pass through `(x, y)`.
    fn candidates(&self, x: f64, y: f64) -> impl Iterator<Item = &&'a Wire> {
        let cell_x = bucket(x, self.tol);
        let cell_y = bucket(y, self.tol);
        (-1..=1).flat_map(move |delta| {
            let column = cell_x
                .checked_add(delta)
                .and_then(|cell| self.columns.get(&cell));
            let row = cell_y
                .checked_add(delta)
                .and_then(|cell| self.rows.get(&cell));
            column
                .into_iter()
                .flatten()
                .chain(row.into_iter().flatten())
        })
    }

    /// Every wire that actually passes through `(x, y)`.
    fn hits(&self, x: f64, y: f64) -> impl Iterator<Item = &&'a Wire> {
        let [x, y] = [x, y].map(self.normalize);
        self.candidates(x, y).filter(move |wire| {
            let [x1, y1, x2, y2] = [wire.x1, wire.y1, wire.x2, wire.y2].map(self.normalize);
            point_on_segment(x, y, x1, y1, x2, y2, self.tol)
        })
    }

    pub(super) fn interior_hits(
        &self,
        x: f64,
        y: f64,
        point_key: fn(f64, f64) -> NetRoot,
    ) -> impl Iterator<Item = &'a Wire> + '_ {
        let p = point_key(x, y);
        self.hits(x, y).copied().filter(move |w| {
            let a = point_key(w.x1, w.y1);
            let b = point_key(w.x2, w.y2);
            a.min(b) < p
                && p < a.max(b)
                && ((a.0 == b.0 && a.0 == p.0) || (a.1 == b.1 && a.1 == p.1))
        })
    }

    /// Lies anywhere on a wire, endpoints included.
    fn covers(&self, x: f64, y: f64) -> bool {
        self.hits(x, y).next().is_some()
    }

    /// Lies on the interior of a wire, where eeschema joins it only through a
    /// junction or a label.
    fn covers_interior(&self, x: f64, y: f64) -> bool {
        self.hits(x, y).any(|wire| {
            !points_coincident(x, y, wire.x1, wire.y1, self.tol)
                && !points_coincident(x, y, wire.x2, wire.y2, self.tol)
        })
    }
}

// ─── Union-find net graph ─────────────────────────────────────────────────────

/// A graph node's identity: coordinates quantized to 1 µm.
///
/// Deliberately *not* the index's tolerance. This is an equality key, and a
/// bucket boundary would separate two points closer together than two points it
/// keeps — so widening it does not make near-misses union, it only makes which
/// ones union depend on where the grid falls. Tolerance is applied by
/// [`seed_net_graph`]'s attach step, which is where a near-miss is resolved
/// against a wire it lies on.
pub(crate) fn pt_key(x: f64, y: f64) -> (i64, i64) {
    ((x * 1000.0).round() as i64, (y * 1000.0).round() as i64)
}

/// KiCad's schematic coordinates, not the coarser lookup keys used by the
/// inspection tools. `parseInternalUnits` rounds to 10,000 IU per mm.
pub(super) fn schematic_point(x: f64, y: f64) -> (i64, i64) {
    const IU_PER_MM: f64 = 10_000.0;
    // KiCad uses the decimal 0.7071, not the exact reciprocal square root.
    const LIMIT: f64 = i32::MAX as f64 * (7071.0 / 10_000.0);
    let coordinate = |mm: f64| (mm * IU_PER_MM).clamp(-LIMIT, LIMIT).round() as i64;
    (coordinate(x), coordinate(y))
}

fn schematic_mm((x, y): (i64, i64)) -> (f64, f64) {
    (x as f64 / 10_000.0, y as f64 / 10_000.0)
}

/// KiCad parses the placed origin and the library pin into integer schematic
/// units before applying the symbol transform.
pub(super) fn schematic_pin_endpoint(pin: &LibPin, mut transform: PinTransform) -> (f64, f64) {
    (transform.comp_x, transform.comp_y) =
        schematic_mm(schematic_point(transform.comp_x, transform.comp_y));
    let (x, y) = schematic_mm(schematic_point(pin.local_x, pin.local_y));
    copperpour_sexp::geometry::transform_pin(x, y, transform)
}

/// Electrical contact with the drawn wire rather than a geometric proximity hit.
pub(super) fn schematic_point_on_wire(at: (f64, f64), wire: &Wire) -> bool {
    let p = schematic_point(at.0, at.1);
    let a = schematic_point(wire.x1, wire.y1);
    let b = schematic_point(wire.x2, wire.y2);
    let between = |p: i64, a: i64, b: i64| (a.min(b)..=a.max(b)).contains(&p);
    between(p.0, a.0, b.0)
        && between(p.1, a.1, b.1)
        && i128::from(p.0 - a.0) * i128::from(b.1 - a.1)
            == i128::from(p.1 - a.1) * i128::from(b.0 - a.0)
}

/// A name a label puts on a point, with the kind of label that put it there.
///
/// The kind is what makes [`NetGraph::name_of_root`] deterministic: one net can
/// carry several names, and which one it is *called* follows KiCad's own
/// precedence rather than whichever the map happened to yield first.
#[derive(Clone)]
struct NetName {
    name: String,
    kind: LabelKind,
    power_local: bool,
}

/// KiCad's same-sheet driver order, from `CONNECTION_SUBGRAPH::PRIORITY`.
/// Sheet pins and ordinary pins are below these ranks and resolved separately.
///
/// The mixed power and label cases have netlist witnesses on 10.0.6 and 10.0.8.
fn driver_priority(driver: &NetName) -> u8 {
    match (driver.kind, driver.power_local) {
        (LabelKind::GlobalLabel, _) => 4,
        (LabelKind::PowerSymbol, false) => 3,
        (LabelKind::PowerSymbol, true) => 2,
        (LabelKind::NetLabel, _) => 1,
        (LabelKind::HierarchicalLabel, _) => 0,
    }
}

/// Whether `a` names the net in preference to `b`. Equal priority is broken on
/// the name, ascending, which is KiCad's own tie-break (two local labels on one
/// net gave `AAA` over `LOCAL_VCC`, and `LOCAL_VCC` over `ZZZ`).
fn names_ahead_of(a: &NetName, b: &NetName) -> bool {
    (Reverse(driver_priority(a)), &a.name) < (Reverse(driver_priority(b)), &b.name)
}

/// A net's identity: the union-find root every point on that net resolves to.
///
/// Distinct from a net *name*, which a net may have several of or none at all.
pub(crate) type NetRoot = (i64, i64);
pub(super) type ScopedNetName = (String, u8);

/// A graph of points joined into nets, and the name each net carries.
///
/// Built only by [`seed_net_graph`], which is why every mutator below is
/// private: once seeding calls [`NetGraph::name_nets`] the graph is frozen, and
/// `root_names` cannot go stale behind a later union. Callers get the two
/// questions they actually ask — [`root_at`](Self::root_at) for a net's
/// identity, [`name_of_root`](Self::name_of_root) for what it is called.
#[derive(Clone)]
pub(crate) struct NetGraph {
    point_key: fn(f64, f64) -> NetRoot,
    pin_endpoint: fn(&LibPin, PinTransform) -> (f64, f64),
    /// Every name at a point, not just the winning one. A rail label written
    /// over the power symbol that names it puts two names on one coordinate,
    /// and dropping the loser would keep it out of
    /// [`merge_named_nets`](Self::merge_named_nets) — so a point carrying
    /// `AAA` and `ZZZ` would never join a disconnected `ZZZ` segment.
    point_nets: HashMap<(i64, i64), Vec<NetName>>,
    parent: HashMap<(i64, i64), NetRoot>,
    /// The winning name per root, resolved once at the end of seeding.
    root_names: HashMap<NetRoot, NetName>,
    /// Every name on each root, sorted. A caller classifying a net — is this a
    /// rail? is it ground? — must read all of them: the winning name can be a
    /// global label that says nothing about the `+3V3` alias beside it.
    root_aliases: HashMap<NetRoot, BTreeSet<String>>,
}

impl NetGraph {
    fn new(point_key: fn(f64, f64) -> NetRoot) -> Self {
        NetGraph {
            point_key,
            pin_endpoint,
            point_nets: HashMap::new(),
            parent: HashMap::new(),
            root_names: HashMap::new(),
            root_aliases: HashMap::new(),
        }
    }

    pub(super) fn into_named_nets(mut self) -> Self {
        self.merge_named_nets();
        self.name_nets();
        self
    }

    pub(super) fn pin_position(&self, pin: &LibPin, transform: PinTransform) -> (f64, f64) {
        (self.pin_endpoint)(pin, transform)
    }

    fn ensure(&mut self, k: (i64, i64)) {
        self.parent.entry(k).or_insert(k);
    }

    fn find(&mut self, k: (i64, i64)) -> NetRoot {
        self.ensure(k);
        let p = self.parent[&k];
        if p == k {
            return k;
        }
        let root = self.find(p);
        self.parent.insert(k, root);
        root
    }

    fn union(&mut self, a: (i64, i64), b: (i64, i64)) {
        let ra = self.find(a);
        let rb = self.find(b);
        if ra != rb {
            self.parent.insert(rb, ra);
        }
    }

    fn add_wire(&mut self, w: &Wire) {
        let a = (self.point_key)(w.x1, w.y1);
        let b = (self.point_key)(w.x2, w.y2);
        self.ensure(a);
        self.ensure(b);
        self.union(a, b);
    }

    fn add_driver(&mut self, x: f64, y: f64, net: &str, kind: LabelKind, power_local: bool) {
        let k = (self.point_key)(x, y);
        self.ensure(k);
        let held = self.point_nets.entry(k).or_default();
        // Kept whole, and ranked only when a name has to be *chosen*: every
        // name at this point still has to reach `merge_named_nets`, or a net
        // joined to this one by the losing name would stay a net of its own.
        if !held
            .iter()
            .any(|name| name.name == net && name.kind == kind && name.power_local == power_local)
        {
            held.push(NetName {
                name: net.to_string(),
                kind,
                power_local,
            });
        }
    }

    /// The graph node a point belongs to. Two points share a root exactly when
    /// they are on one net — through wire and junction geometry, and through
    /// [`merge_named_nets`](Self::merge_named_nets) for the pieces KiCad joins
    /// by name alone.
    ///
    /// This is the comparison a caller wants wherever it would otherwise
    /// compare net *names*: a net carrying two names has one root and would
    /// fail a name comparison against itself.
    pub(crate) fn root_at(&mut self, x: f64, y: f64) -> NetRoot {
        self.find((self.point_key)(x, y))
    }

    /// What the net at `root` is called, or `None` when no label names it.
    ///
    /// A net can carry more than one name — a rail named by a `+3V3` power
    /// symbol that also has a label on it, a local label on a net that also
    /// carries a global one. The answer is the one KiCad's netlister would
    /// choose (see [`driver_priority`]), not whichever the map yielded first:
    /// that was `HashMap` iteration order, so the name moved between runs of
    /// the same binary and the audits reported the losing name as a rail of its
    /// own.
    pub(crate) fn name_of_root(&self, root: NetRoot) -> Option<String> {
        self.root_names.get(&root).map(|name| name.name.clone())
    }

    pub(super) fn scoped_name_of_root(&self, root: NetRoot) -> Option<ScopedNetName> {
        self.root_names.get(&root).map(scoped_name)
    }

    pub(super) fn primary_name_is_global(&self, root: NetRoot) -> bool {
        self.root_names.get(&root).is_some_and(|name| {
            matches!(
                (name.kind, name.power_local),
                (LabelKind::GlobalLabel, _) | (LabelKind::PowerSymbol, false)
            )
        })
    }

    /// The winner and each scoped alias on its net. Roots are local to one
    /// snapshot; comparing these pairs also covers nets without placed pins.
    pub(super) fn named_net_aliases(&mut self) -> BTreeSet<(ScopedNetName, ScopedNetName)> {
        let points: Vec<_> = self.point_nets.keys().copied().collect();
        let mut aliases = BTreeSet::new();
        for point in points {
            let root = self.find(point);
            if let Some(winner) = self.scoped_name_of_root(root) {
                aliases.extend(
                    self.point_nets[&point]
                        .iter()
                        .map(|name| (winner.clone(), scoped_name(name))),
                );
            }
        }
        aliases
    }

    /// Every name on the net at `root`, sorted, empty when nothing names it.
    ///
    /// [`name_of_root`](Self::name_of_root) answers what the net is *called*;
    /// this answers what it is *known as*, which is the question a classifier
    /// asks. A rail whose winning name is the global label `SYS` is still the
    /// `+3V3` rail, and reading only the winner made it neither a rail to
    /// decouple nor a pull-up destination.
    pub(crate) fn aliases_of_root(&self, root: NetRoot) -> BTreeSet<String> {
        self.root_aliases.get(&root).cloned().unwrap_or_default()
    }

    /// Join the points that carry the same name.
    ///
    /// KiCad nets a sheet by name as well as by wire: two segments each
    /// carrying a `SIG` label are one net, and every `GND` power symbol on the
    /// sheet is the same rail. Wires and junctions alone answer for one piece
    /// of copper, so without this a root is a *segment* rather than a net —
    /// and a decoupling capacitor drawn the normal way, on its own stub with
    /// its own `+3V3` symbol, shares no root with the pin it decouples.
    ///
    /// Anchoring each name at the first point that carries it and unioning the
    /// rest into it makes every same-named point one root, whatever order the
    /// map yields them in.
    fn merge_named_nets(&mut self) {
        let mut anchors: HashMap<String, (i64, i64)> = HashMap::new();
        let named: Vec<((i64, i64), Vec<String>)> = self
            .point_nets
            .iter()
            .map(|(k, names)| (*k, names.iter().map(|name| name.name.clone()).collect()))
            .collect();
        for (k, names) in named {
            for name in names {
                match anchors.get(&name) {
                    Some(&anchor) => self.union(anchor, k),
                    None => {
                        anchors.insert(name, k);
                    }
                }
            }
        }
    }

    /// Resolve one winning name per root. Called once, by [`seed_net_graph`],
    /// after the last union — a name is a property of the finished net, so
    /// there is nothing to resolve until the graph stops moving.
    fn name_nets(&mut self) {
        let mut best: HashMap<NetRoot, NetName> = HashMap::new();
        let mut aliases: HashMap<NetRoot, BTreeSet<String>> = HashMap::new();
        for k in self.point_nets.keys().copied().collect::<Vec<_>>() {
            let root = self.find(k);
            for candidate in self.point_nets[&k].clone() {
                aliases
                    .entry(root)
                    .or_default()
                    .insert(candidate.name.clone());
                let wins = match best.get(&root) {
                    Some(held) => names_ahead_of(&candidate, held),
                    None => true,
                };
                if wins {
                    best.insert(root, candidate);
                }
            }
        }
        self.root_names = best;
        self.root_aliases = aliases;
    }

    /// The name of the net at a point — [`root_at`](Self::root_at) then
    /// [`name_of_root`](Self::name_of_root). For reporting a single point only:
    /// a caller asking whether two points are on one net wants their roots, not
    /// their names, since a net can carry more than one.
    pub(crate) fn net_at(&mut self, x: f64, y: f64) -> Option<String> {
        let root = self.root_at(x, y);
        self.name_of_root(root)
    }

    pub(crate) fn points_on_net(&mut self, net: &str) -> Vec<(i64, i64)> {
        // Collect keys first to avoid simultaneous borrow of point_nets and self.find()
        let net_keys: Vec<(i64, i64)> = self
            .point_nets
            .iter()
            .filter(|(_, names)| names.iter().any(|name| name.name == net))
            .map(|(k, _)| *k)
            .collect();
        let net_roots: HashSet<NetRoot> = net_keys.iter().map(|k| self.find(*k)).collect();
        self.points_on_roots(&net_roots)
    }

    /// Every point on any of `roots` — the same walk [`points_on_net`] does,
    /// for a caller that already holds identities rather than a name. Asking
    /// once for several nets also walks the graph once instead of per net.
    ///
    /// [`points_on_net`]: Self::points_on_net
    pub(crate) fn points_on_roots(&mut self, roots: &HashSet<NetRoot>) -> Vec<(i64, i64)> {
        let all_keys: Vec<(i64, i64)> = self.parent.keys().cloned().collect();
        all_keys
            .into_iter()
            .filter(|k| roots.contains(&self.find(*k)))
            .collect()
    }
}

fn scoped_name(name: &NetName) -> (String, u8) {
    let scope = match name.kind {
        LabelKind::PowerSymbol if name.power_local => 1,
        LabelKind::GlobalLabel | LabelKind::PowerSymbol => 0,
        LabelKind::NetLabel => 1,
        LabelKind::HierarchicalLabel => 2,
    };
    (name.name.clone(), scope)
}

/// Seed a graph from the terminals that name and join nets.
///
/// Drawn labels and junction dots connect anywhere along a wire, not only at
/// an endpoint. Power symbols name the node at their pin, which needs an
/// endpoint or junction to reach a wire.
///
/// Sheet pins are only *registered*, never attached mid-span. A hierarchical
/// sheet pin is a pin, and KiCad connects a pin landing mid-wire only through a
/// junction dot (#104) — the rule [`ConnectivityIndex::attaches_pin`] states for
/// symbol pins. A wire that *ends* on a sheet pin is already unioned by
/// `add_wire`, and one that merely passes through its coordinates must not be:
/// a wire routed along a sheet edge would otherwise merge with whatever net
/// that pin carries, inventing a short KiCad does not see. Registering the
/// point is still worth doing, so a query at a sheet pin resolves its net.
fn seed_net_graph(
    tree: &SexpNode,
    wires: &[Wire],
    labels: &[Label],
    on_wire: &WireIndex,
) -> NetGraph {
    seed_drawn_graph(tree, wires, labels, on_wire).into_named_nets()
}

fn seed_drawn_graph(
    tree: &SexpNode,
    wires: &[Wire],
    labels: &[Label],
    on_wire: &WireIndex,
) -> NetGraph {
    seed_drawn_graph_with(tree, wires, labels, pt_key, |graph, x, y| {
        for w in on_wire.hits(x, y) {
            graph.union(pt_key(x, y), pt_key(w.x1, w.y1));
        }
    })
}

fn seed_drawn_graph_with(
    tree: &SexpNode,
    wires: &[Wire],
    labels: &[Label],
    point_key: fn(f64, f64) -> NetRoot,
    attach: impl Fn(&mut NetGraph, f64, f64),
) -> NetGraph {
    let mut graph = NetGraph::new(point_key);
    for wire in wires {
        graph.add_wire(wire);
    }
    let local_power = OnceCell::new();
    for label in labels {
        // Recover the power scope from the placed symbol.
        let power_local = match (label.kind, label.uuid.as_deref()) {
            (LabelKind::PowerSymbol, Some(uuid)) => local_power
                .get_or_init(|| local_power_symbol_uuids(tree))
                .contains(uuid),
            _ => false,
        };
        graph.add_driver(label.x, label.y, &label.net, label.kind, power_local);
        if label.kind != LabelKind::PowerSymbol {
            attach(&mut graph, label.x, label.y);
        }
    }
    for (x, y) in extract_junctions(tree) {
        graph.ensure(point_key(x, y));
        attach(&mut graph, x, y);
    }
    for (x, y) in extract_sheet_pins(tree) {
        graph.ensure(point_key(x, y));
    }
    graph
}

/// Each label paired with the graph root it sits on.
///
/// Two tools read the one relation from opposite ends — `find_shorted_nets`
/// root-first, since more than one name on a root is a short, and
/// `find_single_pin_nets` name-first, pooling the roots one name reaches. The
/// walk lives here so a refinement to which label anchors which root cannot
/// land in only one of them.
pub(crate) fn label_roots<'a>(
    graph: &mut NetGraph,
    labels: &'a [Label],
) -> Vec<((i64, i64), &'a Label)> {
    labels
        .iter()
        .map(|label| (graph.root_at(label.x, label.y), label))
        .collect()
}

/// The net graph for a whole tree, at [`COINCIDENT_TOLERANCE`].
///
/// `labels` must be `extract_all_net_labels` — power symbols name nets too, and
/// a graph built from `extract_labels` alone reports every `power:` rail
/// unconnected.
///
/// This deliberately does not go through [`ConnectivityIndex`]: the graph reads
/// none of the pin geometry an index parses, and this is the entry point ten
/// read-only tools use.
pub(crate) fn net_graph_for(tree: &SexpNode, wires: &[Wire], labels: &[Label]) -> NetGraph {
    seed_net_graph(
        tree,
        wires,
        labels,
        &WireIndex::build(wires, COINCIDENT_TOLERANCE),
    )
}

/// Drawn nodes, before separate wire islands are joined by name. Power
/// symbols are queried as pins, not attached mid-span as labels.
pub(super) fn drawn_net_graph_for(tree: &SexpNode, wires: &[Wire], labels: &[Label]) -> NetGraph {
    seed_drawn_graph(
        tree,
        wires,
        labels,
        &WireIndex::build(wires, COINCIDENT_TOLERANCE),
    )
}

pub(super) fn redraw_point_key(x: f64, y: f64) -> NetRoot {
    let to_iu = |mm| (kicad_schematic_mm(mm) * DRAWN_UNITS_PER_MM).round() as i64;
    (to_iu(x), to_iu(y))
}

pub(super) fn kicad_schematic_mm(mm: f64) -> f64 {
    // SCH_IO_KICAD_SEXPR_PARSER::parseInternalUnits clamps the coordinate
    // to the drawable range before rounding to 100 nm units.
    const LIMIT: f64 = i32::MAX as f64 * 7071.0 / 10_000.0;
    (mm * DRAWN_UNITS_PER_MM).clamp(-LIMIT, LIMIT).round() / DRAWN_UNITS_PER_MM
}

/// Use saved-coordinate equality for redraws without changing legacy queries.
pub(super) fn redraw_net_graph(tree: &SexpNode, wires: &[Wire], labels: &[Label]) -> NetGraph {
    let index = WireIndex::with_coordinates(wires, DRAWN_ATTACHMENT_TOLERANCE, kicad_schematic_mm);
    seed_drawn_graph_with(tree, wires, labels, redraw_point_key, |graph, x, y| {
        for wire in index.hits(x, y) {
            graph.union(redraw_point_key(x, y), redraw_point_key(wire.x1, wire.y1));
        }
    })
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum SymbolPower {
    Ordinary,
    Global,
    Local,
}

impl SymbolPower {
    pub(super) fn from_library(lib: &SexpNode) -> Self {
        match lib.find("power") {
            None => Self::Ordinary,
            Some(power) if power.get(1).and_then(SexpNode::as_str) == Some("local") => Self::Local,
            Some(_) => Self::Global,
        }
    }
}

fn local_power_symbol_uuids(tree: &SexpNode) -> HashSet<String> {
    let libraries = tree
        .find("lib_symbols")
        .map(|node| node.find_all("symbol"))
        .unwrap_or_default();
    extract_symbol_instances(tree)
        .into_iter()
        .filter_map(|instance| {
            let lib = find_lib_symbol(&libraries, &instance)?;
            match SymbolPower::from_library(lib) {
                SymbolPower::Local => instance.uuid,
                _ => None,
            }
        })
        .collect()
}

/// Add node-only drivers from the pins chosen by the caller. A hidden power
/// input on an ordinary symbol names a global net, but is not a drawn mark.
/// Only real power symbols contribute marks for the connector check.
pub(super) fn add_symbol_power_drivers<'a>(
    graph: &mut NetGraph,
    instance: &SymbolInstance,
    power: SymbolPower,
    pins: impl IntoIterator<Item = (&'a LibPin, (f64, f64))>,
    mut drawn_mark: impl FnMut(Label),
) {
    for (pin, (x, y)) in pins {
        if pin.electrical_type != "power_in" {
            continue;
        }
        match power {
            SymbolPower::Ordinary if pin.hidden => {
                let name = super::sch_label_rename::stored_name(&pin.name);
                graph.add_driver(x, y, &name, LabelKind::PowerSymbol, false);
            }
            SymbolPower::Global | SymbolPower::Local if !instance.value.is_empty() => {
                graph.add_driver(
                    x,
                    y,
                    &instance.value,
                    LabelKind::PowerSymbol,
                    power == SymbolPower::Local,
                );
                drawn_mark(Label {
                    kind: LabelKind::PowerSymbol,
                    net: instance.value.clone(),
                    x,
                    y,
                    rotation: instance.rotation,
                    uuid: instance.uuid.clone(),
                });
            }
            _ => {}
        }
    }
}

/// Power drivers name their pin's node without attaching it to a wire that
/// crosses the pin without a junction.
pub(super) fn snapshot_net_graph(tree: &SexpNode, wires: &[Wire]) -> NetGraph {
    snapshot_graph(tree, wires, false)
}

pub(super) fn cleanup_net_graph(tree: &SexpNode, wires: &[Wire]) -> NetGraph {
    snapshot_graph(tree, wires, true)
}

/// Cleanup's electrical graph uses saved schematic units for both keyed
/// endpoints and drawn attachments, including those along diagonal wires.
pub(super) fn exact_drawn_net_graph(tree: &SexpNode, wires: &[Wire], labels: &[Label]) -> NetGraph {
    let mut graph = seed_drawn_graph_with(tree, wires, labels, schematic_point, |graph, x, y| {
        for wire in wires.iter().filter(|w| schematic_point_on_wire((x, y), w)) {
            graph.union(schematic_point(x, y), schematic_point(wire.x1, wire.y1));
        }
    });
    graph.pin_endpoint = schematic_pin_endpoint;
    graph
}

fn snapshot_graph(tree: &SexpNode, wires: &[Wire], exact: bool) -> NetGraph {
    use copperpour_sexp::schematic::extract_labels;

    let labels = extract_labels(tree);
    let mut graph = if exact {
        exact_drawn_net_graph(tree, wires, &labels)
    } else {
        drawn_net_graph_for(tree, wires, &labels)
    };
    let lib_symbols = tree
        .find("lib_symbols")
        .map(|node| node.find_all("symbol"))
        .unwrap_or_default();
    let placed: super::PlacedPinsByReference = if exact {
        super::drawn_pins_for_power_drivers(tree)
            .into_iter()
            .collect::<Option<_>>()
            .unwrap_or_default()
    } else {
        super::placed_pins_for_power_drivers(tree)
    };
    let pin_endpoint = graph.pin_endpoint;
    for (inst, pins) in placed {
        let Some(lib) = find_lib_symbol(&lib_symbols, &inst) else {
            continue;
        };
        let power = match SymbolPower::from_library(lib) {
            // The snapshot historically identified local symbols by UUID.
            SymbolPower::Local if !exact && inst.uuid.is_none() => SymbolPower::Global,
            power => power,
        };
        add_symbol_power_drivers(
            &mut graph,
            &inst,
            power,
            pins.iter()
                .map(|(pin, transform)| (pin, pin_endpoint(pin, *transform))),
            drop,
        );
    }
    graph.into_named_nets()
}

/// Use the run check's saved-coordinate equality, without changing the
/// tolerance of the older connectivity tools.
pub(super) fn wire_run_net_graph(
    tree: &SexpNode,
    wires: &[Wire],
    labels: &[Label],
    point_key: fn(f64, f64) -> NetRoot,
) -> NetGraph {
    let index = WireIndex::build(wires, COINCIDENT_TOLERANCE);
    let mut graph = seed_drawn_graph_with(tree, wires, labels, point_key, |graph, x, y| {
        for w in index.interior_hits(x, y, point_key) {
            graph.union(point_key(x, y), point_key(w.x1, w.y1));
        }
    });
    graph.merge_named_nets();
    graph.name_nets();
    graph
}

// ─── Nets named after a pin ───────────────────────────────────────────────────

/// How KiCad writes a unit after a reference, `U1A` or `U1.1`: the project's
/// `schematic.subpart_id_separator` and `subpart_first_id`
/// (`eeschema/schematic_settings.cpp`). KiCad's defaults, no separator and
/// `A`, stand for a key the project leaves out or sets out of range, and for a
/// sheet no project owns.
#[derive(Clone, Copy)]
pub(crate) struct UnitNotation {
    separator: u32,
    first_id: u32,
}

impl Default for UnitNotation {
    fn default() -> Self {
        UnitNotation {
            separator: 0,
            first_id: u32::from('A'),
        }
    }
}

impl UnitNotation {
    /// The notation of the project that owns `schematic`.
    pub(crate) fn for_schematic(schematic: &std::path::Path) -> Self {
        Self::project_notation(schematic).unwrap_or_default()
    }

    pub(crate) fn project_notation(schematic: &std::path::Path) -> Option<Self> {
        let default = UnitNotation::default();
        let project = crate::tools::resolve_schematic_ownership(schematic)
            .ok()
            .flatten()
            .and_then(|ownership| std::fs::read_to_string(ownership.project_file).ok())
            .and_then(|text| serde_json::from_str::<serde_json::Value>(&text).ok())?;
        let setting = |key: &str, range: std::ops::RangeInclusive<u32>, fallback: u32| {
            project["schematic"][key]
                .as_u64()
                .and_then(|value| u32::try_from(value).ok())
                .filter(|value| range.contains(value))
                .unwrap_or(fallback)
        };
        Some(UnitNotation {
            separator: setting("subpart_id_separator", 0..=126, default.separator),
            first_id: setting(
                "subpart_first_id",
                u32::from('1')..=u32::from('z'),
                default.first_id,
            ),
        })
    }

    /// `SCH_SYMBOL::SubReference`: the library's display name for the unit
    /// when it gives one, else `SCHEMATIC_SETTINGS::SubReference`.
    fn suffix(self, unit: u32, display_name: Option<&str>) -> String {
        let mut suffix = String::new();
        if display_name.is_none() && unit < 1 {
            return suffix;
        }
        if let Some(separator) = char::from_u32(self.separator).filter(|_| self.separator != 0) {
            suffix.push(separator);
        }
        match display_name {
            Some(name) => suffix.push_str(name),
            None if (u32::from('0')..=u32::from('9')).contains(&self.first_id) => {
                suffix.push_str(&unit.to_string());
            }
            None => {
                // `LIB_SYMBOL::LetterSubReference`.
                let mut letters = Vec::new();
                let mut rest = unit;
                loop {
                    let digit = (rest - 1) % 26;
                    letters.extend(char::from_u32(self.first_id + digit));
                    rest = (rest - digit) / 26;
                    if rest == 0 {
                        break;
                    }
                }
                suffix.extend(letters.iter().rev());
            }
        }
        suffix
    }
}

/// `CONNECTION_SUBGRAPH::PRIORITY` for the pins that reach [`PinNetNames`]:
/// a pin of a symbol kept off the board, or whose library reference starts
/// with `#`, names nothing while another pin can; a hidden `power_in` pin of an
/// ordinary symbol is a global power net of its own name.
const NO_PRIORITY: u8 = 0;
const PIN_PRIORITY: u8 = 1;
const GLOBAL_POWER_PIN_PRIORITY: u8 = 6;

/// A pin that drives its net in KiCad's connection graph, as every pin does
/// in KiCad 10.0.8 (`SCH_CONNECTION::IsDriver`), with what its name is made
/// of.
struct PinDriver {
    priority: u8,
    /// The symbol's reference ends with `?`.
    unannotated: bool,
    /// The reference with its unit, as a named pin's net carries it.
    unit_reference: String,
    reference: String,
    name: String,
    number: String,
    /// `SCH_PIN::GetEffectivePadNumber`: the first number a stacked pin
    /// (`[2,5,8]`) stands for, as written; any other number as it is.
    pad: String,
    /// Another pin of the placed unit shows this pin's name under another
    /// number: one that is `no_connect`, and one that is not. KiCad adds the
    /// pad number to the name when one does (`has_multiple`).
    shares_name_with_no_connect_pin: bool,
    shares_name_with_pin: bool,
}

impl PinDriver {
    /// `SCH_PIN::GetDefaultNetName`. `no_connect` is a no-connect flag on the
    /// net, which makes it `unconnected-(…)` whatever reaches it.
    ///
    /// An unannotated symbol's pin is `Err` with the name up to the hash of the
    /// symbol's UUID KiCad writes next (`Net-(R?-1a2b3c4d-Pad1)`), which
    /// `KIID::Hash` takes from the Boost KiCad was built with.
    fn net_name(&self, no_connect: bool) -> Result<String, String> {
        use super::sch_label_rename::stored_name;
        if self.priority == GLOBAL_POWER_PIN_PRIORITY {
            return Ok(stored_name(&self.name));
        }
        let (mut net, has_multiple) = if no_connect {
            (
                "unconnected-(".to_string(),
                self.shares_name_with_no_connect_pin,
            )
        } else {
            ("Net-(".to_string(), self.shares_name_with_pin)
        };
        if self.unannotated {
            net.push_str(&self.reference);
            net.push('-');
            return Err(net);
        }
        if !self.name.is_empty() && self.name != self.number {
            net.push_str(&self.unit_reference);
            net.push('-');
            net.push_str(&stored_name(&self.name));
            if no_connect || has_multiple {
                net.push_str("-Pad");
                net.push_str(&stored_name(&self.pad));
            }
        } else {
            net.push_str(&self.reference);
            net.push_str("-Pad");
            net.push_str(&stored_name(&self.pad));
        }
        net.push(')');
        Ok(net)
    }
}

/// The names KiCad gives the nets on a sheet that no label, power symbol or
/// sheet pin names: after the pin that drives each one
/// (`CONNECTION_SUBGRAPH::ResolveDrivers`), `Net-(R1-Pad2)` or
/// `Net-(U1-VIN)`. A net reaching one such pin is KiCad's `unconnected-(…)`
/// and is left unnamed here, so a pin with no name is a pin on no other. A net
/// KiCad names after a pin of an unannotated symbol is left unnamed too: its
/// name carries a hash this crate does not compute.
///
/// A net whose name comes from a sheet pin is the sheet below's to name, and
/// is left unnamed. Pins a symbol joins inside itself (jumper groups) stay on
/// the nets the graph has.
pub(crate) struct PinNetNames {
    names: HashMap<NetRoot, String>,
}

pub(super) struct NamingPin {
    pub(super) pin: LibPin,
    pub(super) alternate_name: Option<String>,
    pub(super) global_power: bool,
}

impl PinNetNames {
    pub(crate) fn build(tree: &SexpNode, graph: &mut NetGraph, notation: UnitNotation) -> Self {
        use copperpour_sexp::schematic::{
            lib_pin_nodes_for_body_style, parse_lib_pin, placed_pin_electrical_type,
        };
        Self::build_with_pins(tree, graph, notation, false, |_, node, lib, unit| {
            lib_pin_nodes_for_body_style(lib, unit, super::symbol_body_style(node))
                .into_iter()
                .filter_map(|lib_pin| {
                    let pin = parse_lib_pin(lib_pin)?;
                    // Only driver qualification uses the alternate here. The
                    // legacy queries still use base pin types for no-connects.
                    let global_power =
                        pin.hidden && placed_pin_electrical_type(node, lib_pin, &pin) == "power_in";
                    Some(NamingPin {
                        pin,
                        alternate_name: None,
                        global_power,
                    })
                })
                .collect()
        })
    }

    /// `pins_for_symbol` receives the index in the sheet's top-level symbol
    /// list, before skipping any unresolved symbols.
    pub(super) fn build_with_pins(
        tree: &SexpNode,
        graph: &mut NetGraph,
        notation: UnitNotation,
        include_single_pin_names: bool,
        pins_for_symbol: impl Fn(usize, &SexpNode, &SexpNode, u32) -> Vec<NamingPin>,
    ) -> Self {
        use super::symbol_pins::{stacked, Stacked, TILDE_IS_TEXT};
        use copperpour_sexp::schematic::{find_lib_symbol, parse_subsymbol_unit, symbol_instance};
        // KiCad reads a pin named `~` in an older file as unnamed.
        let tilde_is_empty = tree
            .find_str("version")
            .and_then(|version| version.parse::<u32>().ok())
            .is_some_and(|version| version < TILDE_IS_TEXT);
        let lib_symbols = tree
            .find("lib_symbols")
            .map(|node| node.find_all("symbol"))
            .unwrap_or_default();
        let mut drivers: HashMap<NetRoot, Vec<PinDriver>> = HashMap::new();
        for (i, node) in tree.find_all("symbol").into_iter().enumerate() {
            let Some(instance) = symbol_instance(node) else {
                continue;
            };
            let Some(lib) = find_lib_symbol(&lib_symbols, &instance) else {
                continue;
            };
            let off_board = node.find_str("on_board") == Some("no");
            let lib_reference = lib
                .find_all("property")
                .into_iter()
                .find(|property| property.get(1).and_then(SexpNode::as_str) == Some("Reference"))
                .and_then(|property| property.get(2).and_then(SexpNode::as_str))
                .unwrap_or_default();
            let units = lib.find_all("symbol");
            let unit_count = units
                .iter()
                .filter_map(|unit| parse_subsymbol_unit(unit.get(1)?.as_str()?))
                .max()
                .unwrap_or(1);
            let unit_reference = if unit_count > 1 {
                let display_name = units.iter().find_map(|unit| {
                    (unit.get(1)?.as_str().and_then(parse_subsymbol_unit)? == instance.unit)
                        .then(|| unit.find_str("unit_name"))
                        .flatten()
                });
                format!(
                    "{}{}",
                    instance.reference,
                    notation.suffix(instance.unit, display_name)
                )
            } else {
                instance.reference.clone()
            };
            let unannotated = instance.reference.is_empty() || instance.reference.ends_with('?');
            let pins: Vec<_> = pins_for_symbol(i, node, lib, instance.unit)
                .into_iter()
                .map(
                    |NamingPin {
                         mut pin,
                         alternate_name,
                         global_power,
                     }| {
                        for text in [&mut pin.name, &mut pin.number] {
                            if tilde_is_empty && *text == "~" {
                                text.clear();
                            }
                        }
                        let shown_name = alternate_name.unwrap_or_else(|| pin.name.clone());
                        (pin, shown_name, global_power)
                    },
                )
                .collect();
            let t = instance.pin_transform();
            for (pin, shown_name, global_power) in &pins {
                // `SCH_PIN::ConnectionPropagatesTo`: a `no_connect` pin
                // connects to nothing.
                if pin.electrical_type == "no_connect" {
                    continue;
                }
                // `SCH_PIN::IsGlobalPower`. A power symbol's own pin is a label
                // in this graph, and its net is named already.
                let priority = if *global_power {
                    GLOBAL_POWER_PIN_PRIORITY
                } else if off_board || lib_reference.starts_with('#') {
                    NO_PRIORITY
                } else {
                    PIN_PRIORITY
                };
                // KiCad names the net from the library name but detects
                // duplicates using the actual names shown on the sheet.
                let sharing = |no_connect: bool| {
                    pins.iter().any(|(other, other_shown_name, _)| {
                        other_shown_name == shown_name
                            && other.number != pin.number
                            && (other.electrical_type == "no_connect") == no_connect
                    })
                };
                let (x, y) = graph.pin_position(pin, t);
                drivers
                    .entry(graph.root_at(x, y))
                    .or_default()
                    .push(PinDriver {
                        priority,
                        unannotated,
                        unit_reference: unit_reference.clone(),
                        reference: instance.reference.clone(),
                        name: pin.name.clone(),
                        number: pin.number.clone(),
                        pad: match stacked(&pin.number).first() {
                            Some(Stacked::Run(prefix, first, _)) => format!("{prefix}{first}"),
                            Some(Stacked::One(number)) => number.clone(),
                            None => pin.number.clone(),
                        },
                        shares_name_with_no_connect_pin: sharing(true),
                        shares_name_with_pin: sharing(false),
                    });
            }
        }
        let sheet_pin_roots: HashSet<NetRoot> = extract_sheet_pins(tree)
            .into_iter()
            .map(|(x, y)| graph.root_at(x, y))
            .collect();
        let no_connect_roots: HashSet<NetRoot> = extract_no_connects(tree)
            .into_iter()
            .map(|(x, y)| graph.root_at(x, y))
            .collect();
        let names = drivers
            .into_iter()
            .filter(|(root, _)| {
                graph.name_of_root(*root).is_none() && !sheet_pin_roots.contains(root)
            })
            .filter_map(|(root, drivers)| {
                let highest = drivers.iter().map(|driver| driver.priority).max()?;
                // KiCad also uses `unconnected-(…)` for a weak single-pin
                // net with a wire stub. Legacy callers omit these names.
                let weak_single_pin = highest < GLOBAL_POWER_PIN_PRIORITY && drivers.len() == 1;
                if weak_single_pin && !include_single_pin_names {
                    return None;
                }
                let no_connect = weak_single_pin || no_connect_roots.contains(&root);
                // A name up to its hash, which every one follows with `-Pad`,
                // sorts as the whole name does against any name that does not
                // begin with it, and before one that does: that net, which
                // only a reference like `R?-1` can make, stays unnamed too.
                let rank = |name: &Result<String, String>| match name {
                    Ok(name) => (name.contains("-Pad"), name.clone()),
                    Err(head) => (true, head.clone()),
                };
                drivers
                    .iter()
                    .filter(|driver| driver.priority == highest)
                    .map(|driver| driver.net_name(no_connect))
                    .min_by_key(rank)?
                    .ok()
                    .map(|name| (root, name))
            })
            .collect();
        PinNetNames { names }
    }

    /// The name of the net a placed pin at `(x, y)` is on: what a label names
    /// it, as [`NetGraph::net_at`] answers, else KiCad's name for it after a
    /// pin. `None` for a pin KiCad nets alone, and for one a sheet pin's net
    /// reaches.
    pub(crate) fn net_of_pin(
        &self,
        graph: &mut NetGraph,
        pin: &LibPin,
        x: f64,
        y: f64,
    ) -> Option<String> {
        let root = graph.root_at(x, y);
        if let Some(name) = graph.name_of_root(root) {
            return Some(name);
        }
        if pin.electrical_type == "no_connect" {
            return None;
        }
        self.pin_name_of_root(root)
    }

    /// The pin-derived name only. Callers prefer the graph's explicit name.
    pub(crate) fn pin_name_of_root(&self, root: NetRoot) -> Option<String> {
        self.names.get(&root).cloned()
    }
}

// ─── The index ────────────────────────────────────────────────────────────────

/// A pin on the sheet, with the reference designator that owns it. Unit-aware
/// via `placed_pins_by_reference`, so a multi-unit symbol never contributes
/// another unit's pins as phantom connection points (#35).
pub(crate) struct PlacedPin {
    pub(crate) symbol_index: usize,
    pub(crate) symbol_uuid: Option<String>,
    pub(crate) unit: u32,
    pub(crate) is_power_symbol: bool,
    pub(crate) reference: String,
    /// The owning component's value, carried so a caller reporting a pin does
    /// not re-walk the instances this was built from.
    pub(crate) value: String,
    pub(crate) pin: LibPin,
    pub(crate) at: (f64, f64),
}

/// The items sitting on one point, rather than the counts and booleans the
/// attachment predicates ask for. `trace_from_point` is the one tool whose
/// answer *is* that inventory, so it reads this instead of scanning the tree
/// again at a tolerance of its own (#539).
pub(crate) struct PointDetail<'i> {
    /// Component pins at the point, in the order the sheet places them. One
    /// entry per pin, so two pins stacked on a point are both here.
    pub(crate) pins: Vec<&'i PlacedPin>,
    /// Junction dots at the point, as stored in the file.
    pub(crate) junctions: Vec<(f64, f64)>,
}

/// Every item that can terminate a point on one sheet, under one tolerance.
pub(crate) struct ConnectivityIndex<'a> {
    wires: &'a [Wire],
    labels: &'a [Label],
    on_wire: WireIndex<'a>,
    on_bus: WireIndex<'a>,
    bus_entry_wire_points: PointIndex,
    wire_ends: PointIndex,
    label_points: PointIndex,
    pin_points: PointIndex,
    sheet_pin_points: PointIndex,
    junction_points: PointIndex,
    no_connect_points: PointIndex,
    placed_pins: Vec<PlacedPin>,
    /// [`crate::tools::junction_exit_points`], for [`Self::needs_junction`].
    junction_exits: Vec<(f64, f64)>,
}

/// The wire-side end of each bus entry on the sheet.
///
/// An entry terminates a wire only when saved geometry proves exactly one
/// corner lies on a bus. Zero matches are detached; two matches are ambiguous
/// between buses. Neither case licenses a guessed wire side.
pub(crate) fn bus_entry_wire_ends(
    tree: &SexpNode,
    buses: &[Wire],
    tolerance: f64,
) -> Vec<(f64, f64)> {
    bus_entry_wire_ends_on(tree, &WireIndex::build(buses, tolerance))
}

fn bus_entry_wire_ends_on(tree: &SexpNode, on_bus: &WireIndex) -> Vec<(f64, f64)> {
    extract_bus_entries(tree)
        .into_iter()
        .filter_map(|entry| entry.endpoints())
        .filter_map(
            |(at, far)| match (on_bus.covers(at.0, at.1), on_bus.covers(far.0, far.1)) {
                (true, false) => Some(far),
                (false, true) => Some(at),
                _ => None,
            },
        )
        .collect()
}

impl<'a> ConnectivityIndex<'a> {
    /// `labels` should be `extract_all_net_labels`: a power symbol names a net
    /// exactly as a label does, and [`net_graph`](Self::net_graph) needs both.
    /// Its points coincide with the power symbol's own pin, so passing them
    /// changes no coincidence answer — only the names the graph can reach.
    pub(crate) fn build(
        tree: &SexpNode,
        wires: &'a [Wire],
        buses: &'a [Wire],
        labels: &'a [Label],
        tolerance: f64,
    ) -> Self {
        Self::with_placed_pins(
            tree,
            wires,
            buses,
            labels,
            tolerance,
            super::placed_pins_by_reference(tree),
        )
    }

    pub(crate) fn with_placed_pins(
        tree: &SexpNode,
        wires: &'a [Wire],
        buses: &'a [Wire],
        labels: &'a [Label],
        tolerance: f64,
        by_reference: super::PlacedPinsByReference,
    ) -> Self {
        let junctions = extract_junctions(tree);
        let sheet_pins = extract_sheet_pins(tree);
        let on_bus = WireIndex::build(buses, tolerance);
        let bus_entry_wire_points = bus_entry_wire_ends_on(tree, &on_bus);
        let junction_exits = crate::tools::junction_exits(
            &by_reference,
            sheet_pins.iter().copied(),
            bus_entry_wire_points.iter().copied(),
        );
        let power_symbols: HashSet<_> = tree
            .find("lib_symbols")
            .into_iter()
            .flat_map(|lib| lib.find_all("symbol"))
            .filter(|lib| lib.find("power").is_some())
            .filter_map(|lib| lib.get(1)?.as_str())
            .collect();
        let placed_pins: Vec<PlacedPin> = by_reference
            .into_iter()
            .enumerate()
            .flat_map(|(symbol_index, (inst, pins))| {
                let is_power_symbol = power_symbols.contains(inst.lib_symbol_name());
                pins.into_iter().map(move |(pin, transform)| PlacedPin {
                    symbol_index,
                    symbol_uuid: inst.uuid.clone(),
                    unit: inst.unit,
                    is_power_symbol,
                    reference: inst.reference.clone(),
                    value: inst.value.clone(),
                    at: pin_endpoint(&pin, transform),
                    pin,
                })
            })
            .collect();

        ConnectivityIndex {
            wires,
            labels,
            on_wire: WireIndex::build(wires, tolerance),
            on_bus,
            bus_entry_wire_points: PointIndex::build(bus_entry_wire_points, tolerance),
            wire_ends: PointIndex::build(
                wires
                    .iter()
                    .flat_map(|wire| [(wire.x1, wire.y1), (wire.x2, wire.y2)]),
                tolerance,
            ),
            // Power symbols excluded on purpose. `extract_power_symbol_labels`
            // places a pseudo-label *on the symbol's own pin*, so indexing it
            // here would make every power pin see a label at its own position
            // and report itself attached — hiding the unwired GND that is one
            // of the commonest real mistakes on a sheet. A wire arriving at a
            // power symbol is terminated by its pin, which `pin_points` covers.
            label_points: PointIndex::build(
                labels
                    .iter()
                    .filter(|label| label.kind != LabelKind::PowerSymbol)
                    .map(|label| (label.x, label.y)),
                tolerance,
            ),
            // Built from `placed_pins` in order, so a hit's ordinal indexes
            // straight back into that slice — which is what lets
            // `point_detail` report the pin and not just its coordinates.
            pin_points: PointIndex::build(placed_pins.iter().map(|p| p.at), tolerance),
            sheet_pin_points: PointIndex::build(sheet_pins, tolerance),
            junction_points: PointIndex::build(junctions, tolerance),
            no_connect_points: PointIndex::build(extract_no_connects(tree), tolerance),
            placed_pins,
            junction_exits,
        }
    }

    pub(crate) fn placed_pins(&self) -> &[PlacedPin] {
        &self.placed_pins
    }

    pub(crate) fn labels(&self) -> &[Label] {
        self.labels
    }

    pub(crate) fn junctions(&self) -> impl Iterator<Item = (f64, f64)> + '_ {
        self.junction_points
            .buckets
            .values()
            .flatten()
            .map(|&(x, y, _)| (x, y))
    }

    /// How many wire endpoints lie at `(x, y)`. A point that is itself a wire
    /// endpoint counts itself, so two or more means wires meet here.
    pub(crate) fn wire_ends_at(&self, x: f64, y: f64) -> usize {
        self.wire_ends.count_at(x, y)
    }

    pub(crate) fn has_wire_end(&self, x: f64, y: f64) -> bool {
        self.wire_ends.contains(x, y)
    }

    /// Lies anywhere on a wire, endpoints included.
    pub(crate) fn on_wire(&self, x: f64, y: f64) -> bool {
        self.on_wire.covers(x, y)
    }

    /// Lies on a wire's interior. eeschema joins a wire there only through a
    /// junction dot or a label, so a T without either is two nets.
    pub(crate) fn on_wire_interior(&self, x: f64, y: f64) -> bool {
        self.on_wire.covers_interior(x, y)
    }

    /// Lies anywhere on a bus segment, endpoints included. This is geometric
    /// attachment only: buses are bundles, never nodes in [`NetGraph`].
    pub(crate) fn on_bus(&self, x: f64, y: f64) -> bool {
        self.on_bus.covers(x, y)
    }

    /// A bus entry whose opposite corner is proven to touch exactly one bus
    /// has its wire-side endpoint here.
    pub(crate) fn has_bus_entry_wire_end(&self, x: f64, y: f64) -> bool {
        self.bus_entry_wire_points.contains(x, y)
    }

    /// How many wires lie under `(x, y)` — endpoint and interior alike. The
    /// booleans above cannot tell one wire passing from two wires crossing,
    /// and the junction reconciler needs that distinction: a dot on a lone
    /// wire connects nothing, a dot where two wires cross joins two nets.
    pub(crate) fn wires_at(&self, x: f64, y: f64) -> usize {
        self.wires
            .iter()
            .filter(|w| point_on_segment(x, y, w.x1, w.y1, w.x2, w.y2, self.on_wire.tolerance()))
            .count()
    }

    /// Whether a junction dot belongs at `(x, y)`: three or more exits, counted
    /// as the wire tools count them (#897). A wire passing through counts two
    /// exits even where nothing ends, so this says yes at a bare crossing; a
    /// caller adding a dot refuses that case itself, since the dot would join
    /// two nets.
    pub(crate) fn needs_junction(&self, x: f64, y: f64) -> bool {
        wire_end_needs_junction(
            self.wires,
            &self.junction_exits,
            x,
            y,
            self.on_wire.tolerance(),
        )
    }

    pub(crate) fn has_label(&self, x: f64, y: f64) -> bool {
        self.label_points.contains(x, y)
    }

    pub(crate) fn pin_indices_at(&self, x: f64, y: f64) -> Vec<usize> {
        let mut indices: Vec<_> = self.pin_points.hits_at(x, y).map(|(_, _, i)| i).collect();
        indices.sort_unstable();
        indices
    }

    pub(crate) fn wire_run_pins_at(&self, x: f64, y: f64) -> impl Iterator<Item = usize> + '_ {
        let key = pt_key(x, y);
        self.pin_points
            .hits_at(x, y)
            .filter(move |&(px, py, i)| !self.placed_pins[i].pin.hidden && pt_key(px, py) == key)
            .map(|(_, _, i)| i)
    }

    pub(crate) fn wire_run_joint_has_attachment(&self, x: f64, y: f64) -> bool {
        self.wire_run_pins_at(x, y).next().is_some()
            || self.has_label(x, y)
            || self.has_sheet_pin(x, y)
            || self.has_junction(x, y)
            || self.has_no_connect(x, y)
            || self.has_bus_entry_wire_end(x, y)
            || self.on_wire_interior(x, y)
    }

    /// What is on `(x, y)`, item by item: the component pins there and the
    /// junction dots there, under this index's tolerance. The predicates below
    /// answer whether a point is attached; this answers what is attached, for
    /// the tool that has to report it.
    ///
    /// Sheet pins and no-connect flags are indexed too but are deliberately not
    /// returned: `trace_from_point` does not report them, and handing them over
    /// unused would let the two drift apart silently.
    pub(crate) fn point_detail(&self, x: f64, y: f64) -> PointDetail<'_> {
        let pins = self
            .pin_indices_at(x, y)
            .into_iter()
            .map(|ordinal| &self.placed_pins[ordinal])
            .collect();
        let mut junctions: Vec<(usize, (f64, f64))> = self
            .junction_points
            .hits_at(x, y)
            .map(|(jx, jy, ordinal)| (ordinal, (jx, jy)))
            .collect();
        // Bucket order is not file order, and a reported list that reorders
        // itself with the query point is a list a caller cannot diff.
        junctions.sort_by_key(|(ordinal, _)| *ordinal);
        PointDetail {
            pins,
            junctions: junctions.into_iter().map(|(_, at)| at).collect(),
        }
    }

    /// How many pins lie at `(x, y)`. A point that is itself a pin counts
    /// itself, so two or more means pins are stacked — a legal connection.
    pub(crate) fn pins_at(&self, x: f64, y: f64) -> usize {
        self.pin_points.count_at(x, y)
    }

    pub(crate) fn has_pin(&self, x: f64, y: f64) -> bool {
        self.pin_points.contains(x, y)
    }

    pub(crate) fn has_sheet_pin(&self, x: f64, y: f64) -> bool {
        self.sheet_pin_points.contains(x, y)
    }

    pub(crate) fn has_junction(&self, x: f64, y: f64) -> bool {
        self.junction_points.contains(x, y)
    }

    pub(crate) fn has_no_connect(&self, x: f64, y: f64) -> bool {
        self.no_connect_points.contains(x, y)
    }

    /// Whether a wire endpoint at `(x, y)` is terminated. Everything but the
    /// endpoint itself terminates it: a pin, a label, a hierarchical sheet pin,
    /// a junction dot, a no-connect flag, or another wire's endpoint. A wire it
    /// lands mid-span on does not: eeschema joins that wire only through a
    /// junction or a label there, and its ERC reports the end as unconnected.
    pub(crate) fn terminates_wire_end(&self, x: f64, y: f64) -> bool {
        self.has_pin(x, y)
            || self.has_label(x, y)
            || self.has_sheet_pin(x, y)
            || self.has_junction(x, y)
            || self.has_no_connect(x, y)
            || self.has_bus_entry_wire_end(x, y)
            || self.wire_ends_at(x, y) >= 2
    }

    /// Whether a pin at `(x, y)` is attached to anything. A wire ending on it,
    /// a label naming it, a hierarchical sheet pin meeting it, or a second pin
    /// stacked on it all connect. A pin landing mid-wire connects only through
    /// a junction dot: KiCAD's netlister registers the unsplit wire at a
    /// junction point, so the dot alone is enough (#104).
    pub(crate) fn attaches_pin(&self, x: f64, y: f64) -> bool {
        self.has_wire_end(x, y)
            || self.has_label(x, y)
            || self.has_sheet_pin(x, y)
            || self.pins_at(x, y) >= 2
            || (self.has_junction(x, y) && self.on_wire(x, y))
    }

    /// Every wire endpoint that nothing terminates, as `(x, y, wire uuid)`.
    /// Both tools that report floating ends read this, so a refinement to what
    /// counts as terminated cannot land in only one of them.
    pub(crate) fn floating_wire_ends(&self) -> Vec<(f64, f64, Option<&str>)> {
        self.wires
            .iter()
            .flat_map(|wire| {
                [(wire.x1, wire.y1), (wire.x2, wire.y2)].map(|(x, y)| (x, y, wire.uuid.as_deref()))
            })
            .filter(|&(x, y, _)| !self.terminates_wire_end(x, y))
            .collect()
    }
}

#[cfg(test)]
mod agreement_tests {
    use super::*;
    use crate::tools::{ServerConfig, ToolContext};
    use copperpour_sexp::schematic::{
        extract_all_net_labels, extract_buses, extract_wires, read_schematic,
    };
    use serde_json::json;
    use std::io::Write;
    use std::sync::Arc;

    #[test]
    fn snapshot_preserves_hidden_power_join_and_name_scope() {
        let source = include_str!("../../tests/fixtures/connector_hidden_power_kicad10.kicad_sch");
        let tree = copperpour_sexp::parse_sexp(source).unwrap();
        let mut graph = snapshot_net_graph(&tree, &extract_wires(&tree));
        let connector = graph.root_at(0.0, 0.0);
        assert_eq!(connector, graph.root_at(10.16, 20.32));
        assert_eq!(
            graph.scoped_name_of_root(connector),
            Some(("VCC".into(), 0))
        );

        let local = source.replacen("(power global)", "(power local)", 1);
        assert_ne!(local, source);
        let tree = copperpour_sexp::parse_sexp(&local).unwrap();
        let mut graph = snapshot_net_graph(&tree, &extract_wires(&tree));
        let connector = graph.root_at(0.0, 0.0);
        assert_eq!(connector, graph.root_at(10.16, 20.32));
        assert_eq!(
            graph.scoped_name_of_root(connector),
            Some(("VCC".into(), 0))
        );
        assert!(graph
            .named_net_aliases()
            .contains(&(("VCC".into(), 0), ("VCC".into(), 1))));
    }

    #[test]
    fn snapshot_does_not_join_a_visible_power_input_by_name() {
        let source = include_str!("../../tests/fixtures/connector_hidden_power_kicad10.kicad_sch");
        let visible = source.replacen(
            "(length 2.54)\n\t\t\t\t\t(hide yes)\n\t\t\t\t\t(name \"VCC\"",
            "(length 2.54)\n\t\t\t\t\t(name \"VCC\"",
            1,
        );
        assert_ne!(visible, source);
        let tree = copperpour_sexp::parse_sexp(&visible).unwrap();
        let mut graph = snapshot_net_graph(&tree, &extract_wires(&tree));
        let connector = graph.root_at(0.0, 0.0);
        let rail = graph.root_at(10.16, 20.32);
        assert_ne!(connector, rail);
        assert_eq!(graph.scoped_name_of_root(connector), None);
        assert_eq!(graph.scoped_name_of_root(rail), Some(("VCC".into(), 0)));
    }

    /// Run a registered tool from either schematic toolset against a temporary
    /// file, exactly as the MCP dispatch layer does after selecting its
    /// `ToolDef`. Both toolsets are searched so one test can ask two tools the
    /// same question — which is the whole point of this module.
    async fn call(
        tool_name: &str,
        schematic: &str,
        mut args: serde_json::Value,
    ) -> serde_json::Value {
        let mut file = tempfile::NamedTempFile::with_suffix(".kicad_sch").unwrap();
        file.write_all(schematic.as_bytes()).unwrap();
        file.flush().unwrap();
        args["schematic"] = json!(file.path().to_str().unwrap());

        let definition = crate::tools::sch_analysis::tools()
            .into_iter()
            .chain(crate::tools::sch_batch::tools())
            .find(|tool| tool.name == tool_name)
            .unwrap_or_else(|| panic!("no tool named {tool_name}"));
        let context = ToolContext::new(
            ServerConfig::default(),
            Arc::new(crate::router::ToolRouter::new()),
        );
        let result = (definition.handler)(&args, Arc::new(context))
            .await
            .unwrap();
        assert!(!result.is_error, "{tool_name} failed: {:?}", result.content);
        let crate::mcp::protocol::ToolContent::Text { text } = &result.content[0] else {
            panic!("expected text content from {tool_name}");
        };
        serde_json::from_str(text).unwrap()
    }

    /// A one-pin symbol whose connection point is the placement origin.
    const LIB: &str = "\t(lib_symbols\n\t\t(symbol \"Test:P1\"\n\t\t\t(symbol \"P1_1_1\"\n\t\t\t\t(pin passive line (at 0 0 0) (length 2.54)\n\t\t\t\t\t(name \"~\" (effects (font (size 1.27 1.27))))\n\t\t\t\t\t(number \"1\" (effects (font (size 1.27 1.27))))\n\t\t\t\t)\n\t\t\t)\n\t\t)\n\t)\n";

    fn symbol(reference: &str, uuid: &str, x: f64, y: f64) -> String {
        format!(
            "\t(symbol\n\t\t(lib_id \"Test:P1\")\n\t\t(at {x} {y} 0)\n\t\t(unit 1)\n\t\t(uuid \"{uuid}\")\n\t\t(property \"Reference\" \"{reference}\"\n\t\t\t(at {x} {y} 0)\n\t\t)\n\t)\n"
        )
    }

    fn sheet(x: f64, y: f64, pin_x: f64, pin_y: f64) -> String {
        format!(
            "\t(sheet\n\t\t(at {x} {y})\n\t\t(size 20 20)\n\t\t(uuid \"sh1\")\n\t\t(pin \"OUT\" input\n\t\t\t(at {pin_x} {pin_y} 180)\n\t\t\t(uuid \"sp1\")\n\t\t)\n\t)\n"
        )
    }

    fn schematic(body: &str) -> String {
        format!("(kicad_sch\n\t(version 20260306)\n\t(generator \"eeschema\")\n\t(uuid \"root\")\n{LIB}{body}\t(sheet_instances (path \"/\" (page \"1\")))\n)\n")
    }

    /// U1 —— wire —— sheet pin. Both ends of the wire are terminated, and every
    /// tool has to say so: `validate_wire_connections` used to report the
    /// sheet-pin end as floating because sheet pins were not in its item set.
    #[tokio::test]
    async fn a_sheet_pin_terminates_a_wire_end_for_every_tool() {
        let sch = schematic(&format!(
            "\t(wire\n\t\t(pts (xy 100 80) (xy 120 80))\n\t\t(uuid \"w1\")\n\t)\n{}{}",
            symbol("U1", "u1", 100.0, 80.0),
            sheet(120.0, 70.0, 120.0, 80.0),
        ));

        let orphans = call("find_orphan_items", &sch, json!({})).await;
        assert_eq!(orphans["orphan_count"], 0, "{orphans}");

        let wires = call("validate_wire_connections", &sch, json!({})).await;
        assert_eq!(wires["floating_count"], 0, "{wires}");
    }

    /// Two pins placed on each other are a legal connection with no wire at
    /// all. `find_orphan_items` has always counted it; the component validator
    /// used to demand a wire endpoint and report both pins unconnected.
    #[tokio::test]
    async fn stacked_pins_are_connected_for_every_tool() {
        let sch = schematic(&format!(
            "{}{}",
            symbol("U1", "u1", 100.0, 80.0),
            symbol("U2", "u2", 100.0, 80.0),
        ));

        let orphans = call("find_orphan_items", &sch, json!({})).await;
        assert_eq!(orphans["orphan_count"], 0, "{orphans}");

        let components = call("validate_component_connections", &sch, json!({})).await;
        assert_eq!(components["unconnected_count"], 0, "{components}");
    }

    /// The point the validator called connected is the point `trace_from_point`
    /// inventories, and both read one index: the pins the decision was made on
    /// are the pins reported. A second scan of its own is how this tool could
    /// answer with a set the decision never saw (#539).
    #[tokio::test]
    async fn a_traced_point_reports_the_pins_the_validator_judged() {
        let sch = schematic(&format!(
            "\t(junction (at 100 80) (uuid \"j1\"))\n{}{}",
            symbol("U1", "u1", 100.0, 80.0),
            symbol("U2", "u2", 100.0, 80.0),
        ));

        let components = call("validate_component_connections", &sch, json!({})).await;
        assert_eq!(components["unconnected_count"], 0, "{components}");

        let trace = call("trace_from_point", &sch, json!({ "x": 100.0, "y": 80.0 })).await;
        let mut found: Vec<&str> = trace["pins_here"]
            .as_array()
            .expect("pins_here is always present")
            .iter()
            .map(|pin| pin["reference"].as_str().unwrap())
            .collect();
        found.sort_unstable();
        assert_eq!(found, ["U1", "U2"], "{trace}");
        assert_eq!(
            trace["junctions_here"].as_array().unwrap().len(),
            1,
            "{trace}"
        );
    }

    /// A lone pin is unconnected for both tools — the agreement has to hold in
    /// the direction that still reports a fault, or the fix above is just a
    /// blanket "everything is connected".
    #[tokio::test]
    async fn an_isolated_pin_is_unconnected_for_every_tool() {
        let sch = schematic(&symbol("U1", "u1", 100.0, 80.0));

        let orphans = call("find_orphan_items", &sch, json!({})).await;
        assert_eq!(orphans["orphan_count"], 1, "{orphans}");
        assert_eq!(orphans["orphans"][0]["type"], "unconnected_pin");

        let components = call("validate_component_connections", &sch, json!({})).await;
        assert_eq!(components["unconnected_count"], 1, "{components}");
        assert_eq!(components["unconnected_pins"][0]["reference"], "U1");
    }

    /// The points of `items` whose `type` is `kind`, or of all of them.
    fn points_of(items: &serde_json::Value, kind: Option<&str>) -> Vec<(f64, f64)> {
        items
            .as_array()
            .unwrap()
            .iter()
            .filter(|item| kind.is_none_or(|kind| item["type"] == kind))
            .map(|item| (item["x"].as_f64().unwrap(), item["y"].as_f64().unwrap()))
            .collect()
    }

    /// A wire ending mid-span on another wire with no junction there is not
    /// joined to it: eeschema joins a wire's interior only through a junction
    /// or a label, and its ERC reports the end as `unconnected_wire_endpoint`.
    /// The `tee` group of `route_obstacles_kicad10.kicad_sch`, as KiCad saved
    /// it, is that shape: R53's wire ends at (190.5, 156.21) on R51's. See the
    /// README.
    #[tokio::test]
    async fn a_wire_ending_mid_span_without_a_junction_floats_for_every_tool() {
        let sch = include_str!("../../tests/fixtures/route_obstacles_kicad10.kicad_sch");

        let wires = call("validate_wire_connections", sch, json!({})).await;
        assert_eq!(
            points_of(&wires["floating_endpoints"], None),
            [(190.5, 156.21)],
            "{wires}"
        );

        let orphans = call("find_orphan_items", sch, json!({})).await;
        assert_eq!(
            points_of(&orphans["orphans"], Some("dangling_wire_end")),
            [(190.5, 156.21)],
            "{orphans}"
        );
    }

    async fn served(
        handler: &crate::mcp::handler::McpHandler,
        tool: &str,
        path: &std::path::Path,
        mut arguments: serde_json::Value,
    ) -> serde_json::Value {
        arguments["schematic"] = json!(path.display().to_string());
        let response = handler
            .handle_message(json!({
                "jsonrpc": "2.0",
                "id": 1,
                "method": "tools/call",
                "params": { "name": tool, "arguments": arguments }
            }))
            .await
            .expect("tools/call receives a response");
        let result = response.result.expect("successful JSON-RPC response");
        assert_ne!(result["isError"], json!(true), "{tool}: {result}");
        serde_json::from_str(result["content"][0]["text"].as_str().unwrap()).unwrap()
    }

    /// The same shape made through Copperpour's own tools. `batch_add_wire`
    /// dots R1's branch where it lands mid-span, so both tools pass the sheet;
    /// `batch_delete` removing that dot leaves the branch end unjoined, as
    /// KiCad's netlist and ERC then say, and both tools must report it.
    #[tokio::test]
    async fn a_tee_whose_dot_is_deleted_floats_for_every_tool() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("wire_end_junctions_kicad10.kicad_sch");
        std::fs::write(
            &path,
            include_str!("../../tests/fixtures/wire_end_junctions_kicad10.kicad_sch"),
        )
        .unwrap();
        let handler = crate::mcp::handler::McpHandler::new(ServerConfig {
            eager_toolsets: true,
            ..Default::default()
        })
        .await
        .expect("handler builds");
        // R1 pin 1 at (152.4, 97.79) up to a wire that runs to U1's output,
        // whose open end a label names.
        let wires = json!([
            { "x1": 127.0, "y1": 88.9, "x2": 198.12, "y2": 88.9 },
            { "x1": 198.12, "y1": 88.9, "x2": 198.12, "y2": 101.6 },
            { "x1": 152.4, "y1": 97.79, "x2": 152.4, "y2": 88.9 }
        ]);
        served(&handler, "batch_add_wire", &path, json!({ "wires": wires })).await;
        let label = json!({ "net": "N", "x": 127.0, "y": 88.9 });
        served(&handler, "add_schematic_net_label", &path, label).await;

        let wires = served(&handler, "validate_wire_connections", &path, json!({})).await;
        assert_eq!(wires["floating_count"], 0, "{wires}");
        let orphans = served(&handler, "find_orphan_items", &path, json!({})).await;
        let dangling = points_of(&orphans["orphans"], Some("dangling_wire_end"));
        assert!(dangling.is_empty(), "{orphans}");

        let tree = copperpour_sexp::parse_sexp(&std::fs::read_to_string(&path).unwrap()).unwrap();
        let dots = tree.find_all("junction");
        assert_eq!(dots.len(), 1, "batch_add_wire dots the T alone");
        let dot = dots[0].find_str("uuid").unwrap().to_string();
        served(&handler, "batch_delete", &path, json!({ "uuids": [dot] })).await;

        let wires = served(&handler, "validate_wire_connections", &path, json!({})).await;
        assert_eq!(
            points_of(&wires["floating_endpoints"], None),
            [(152.4, 88.9)],
            "{wires}"
        );
        let orphans = served(&handler, "find_orphan_items", &path, json!({})).await;
        assert_eq!(
            points_of(&orphans["orphans"], Some("dangling_wire_end")),
            [(152.4, 88.9)],
            "{orphans}"
        );
    }

    fn bus(x1: f64, y1: f64, x2: f64, y2: f64, uuid: &str) -> String {
        format!("\t(bus (pts (xy {x1} {y1}) (xy {x2} {y2})) (uuid \"{uuid}\"))\n")
    }

    fn bus_entry(x: f64, y: f64, dx: f64, dy: f64, uuid: &str) -> String {
        format!(
            "\t(bus_entry (at {x} {y}) (size {dx} {dy}) (stroke (width 0) (type default)) (uuid \"{uuid}\"))\n"
        )
    }

    fn wire(x1: f64, y1: f64, x2: f64, y2: f64, uuid: &str) -> String {
        format!("\t(wire (pts (xy {x1} {y1}) (xy {x2} {y2})) (uuid \"{uuid}\"))\n")
    }

    fn label(net: &str, x: f64, y: f64, uuid: &str) -> String {
        format!("\t(label \"{net}\" (at {x} {y} 0) (uuid \"{uuid}\"))\n")
    }

    /// KiCad connects the corner opposite the bus only when the other corner
    /// really lies on bus geometry. Both public validators consume that same
    /// decision, and a label directly on the bus is not a floating label.
    #[tokio::test]
    async fn a_verified_bus_entry_terminates_only_its_wire_side_for_every_tool() {
        let sch = schematic(&format!(
            "{}{}{}{}{}",
            bus(90.0, 80.0, 110.0, 80.0, "b1"),
            bus_entry(100.0, 80.0, 2.54, 2.54, "be1"),
            wire(102.54, 82.54, 120.0, 82.54, "w1"),
            label("D0", 120.0, 82.54, "l1"),
            label("D[0..3]", 95.0, 80.0, "l2"),
        ));

        let orphans = call("find_orphan_items", &sch, json!({})).await;
        assert_eq!(orphans["orphan_count"], 0, "{orphans}");

        let wires = call("validate_wire_connections", &sch, json!({})).await;
        assert_eq!(wires["floating_count"], 0, "{wires}");

        let (tree, wires, labels) = index_for(&sch);
        let buses = extract_buses(&tree);
        let index = ConnectivityIndex::build(&tree, &wires, &buses, &labels, COINCIDENT_TOLERANCE);
        assert!(index.on_bus(100.0, 80.0), "test setup needs a bus");
        assert!(index.has_bus_entry_wire_end(102.54, 82.54));
        assert!(!index.has_bus_entry_wire_end(100.0, 80.0));
    }

    /// The acceptance fixture was resaved by KiCad 10. Its companion README
    /// records the CLI ERC comparison: KiCad does not report the wire/entry or
    /// bus label as dangling (only the deliberately pin-less scalar label).
    #[tokio::test]
    async fn kicad_saved_bus_fixture_agrees_across_internal_validators() {
        let sch = include_str!("../../tests/fixtures/bus_connectivity_kicad10.kicad_sch");

        let orphans = call("find_orphan_items", sch, json!({})).await;
        assert_eq!(orphans["orphan_count"], 0, "{orphans}");

        let wires = call("validate_wire_connections", sch, json!({})).await;
        assert_eq!(wires["floating_count"], 0, "{wires}");
    }

    #[test]
    fn bus_entry_classification_abstains_for_detached_and_ambiguous_geometry() {
        struct Case {
            name: &'static str,
            body: String,
            wire_end: (f64, f64),
        }
        let cases = [
            Case {
                name: "direct wire-to-bus touch has no entry",
                body: format!(
                    "{}{}",
                    bus(90.0, 80.0, 110.0, 80.0, "b1"),
                    wire(100.0, 80.0, 120.0, 80.0, "w1")
                ),
                wire_end: (100.0, 80.0),
            },
            Case {
                name: "bus-side corner is not the wire side",
                body: format!(
                    "{}{}{}",
                    bus(90.0, 80.0, 110.0, 80.0, "b1"),
                    bus_entry(100.0, 80.0, 2.54, 2.54, "be1"),
                    wire(100.0, 80.0, 120.0, 80.0, "w1")
                ),
                wire_end: (100.0, 80.0),
            },
            Case {
                name: "detached entry cannot infer a wire side",
                body: format!(
                    "{}{}{}",
                    bus(90.0, 80.0, 110.0, 80.0, "b1"),
                    bus_entry(100.0, 85.0, 2.54, 2.54, "be1"),
                    wire(102.54, 87.54, 120.0, 87.54, "w1")
                ),
                wire_end: (102.54, 87.54),
            },
            Case {
                name: "entry with both corners on buses is ambiguous",
                body: format!(
                    "{}{}{}{}",
                    bus(90.0, 80.0, 110.0, 80.0, "b1"),
                    bus(90.0, 82.54, 110.0, 82.54, "b2"),
                    bus_entry(100.0, 80.0, 0.0, 2.54, "be1"),
                    wire(100.0, 82.54, 120.0, 82.54, "w1")
                ),
                wire_end: (100.0, 82.54),
            },
        ];

        for case in cases {
            let sch = schematic(&case.body);
            let (tree, wires, labels) = index_for(&sch);
            let buses = extract_buses(&tree);
            let index =
                ConnectivityIndex::build(&tree, &wires, &buses, &labels, COINCIDENT_TOLERANCE);
            assert!(index.on_bus(100.0, 80.0), "{}", case.name);
            assert!(
                !index.has_bus_entry_wire_end(case.wire_end.0, case.wire_end.1),
                "{}",
                case.name
            );
            assert!(
                !index.terminates_wire_end(case.wire_end.0, case.wire_end.1),
                "{}",
                case.name
            );
        }
    }

    #[tokio::test]
    async fn a_label_near_but_outside_bus_tolerance_stays_floating() {
        let sch = schematic(&format!(
            "{}{}",
            bus(90.0, 80.0, 110.0, 80.0, "b1"),
            label("NEAR_BUS", 100.0, 80.02, "l1")
        ));

        let orphans = call("find_orphan_items", &sch, json!({ "tolerance": 0.01 })).await;
        assert_eq!(orphans["orphan_count"], 1, "{orphans}");
        assert_eq!(orphans["orphans"][0]["type"], "floating_label");
        assert_eq!(orphans["orphans"][0]["net"], "NEAR_BUS");
    }

    /// The component validator still reports the value it always has, which now
    /// comes from a lookup rather than from the instance being iterated.
    #[tokio::test]
    async fn an_unconnected_pin_still_reports_its_component_value() {
        let sch = schematic(&symbol("U1", "u1", 100.0, 80.0).replace(
            "(property \"Reference\" \"U1\"",
            "(property \"Value\" \"10k\"\n\t\t\t(at 100 80 0)\n\t\t)\n\t\t(property \"Reference\" \"U1\"",
        ));

        let components = call("validate_component_connections", &sch, json!({})).await;
        assert_eq!(
            components["unconnected_pins"][0]["value"], "10k",
            "{components}"
        );
    }

    /// A graph seeded exactly as `net_graph_for` seeds one, but at a chosen
    /// tolerance, so a test can vary the only knob the seeder takes.
    fn graph_at(
        tree: &copperpour_sexp::SexpNode,
        wires: &[Wire],
        labels: &[Label],
        tol: f64,
    ) -> NetGraph {
        seed_net_graph(tree, wires, labels, &WireIndex::build(wires, tol))
    }

    fn index_for(sch: &str) -> (copperpour_sexp::SexpNode, Vec<Wire>, Vec<Label>) {
        let mut file = tempfile::NamedTempFile::with_suffix(".kicad_sch").unwrap();
        file.write_all(sch.as_bytes()).unwrap();
        file.flush().unwrap();
        let (_, tree) = read_schematic(file.path()).unwrap();
        let wires = extract_wires(&tree);
        let labels = extract_all_net_labels(&tree);
        (tree, wires, labels)
    }

    /// The graph now attaches at the index's tolerance instead of a hardcoded
    /// 0.01, so a label 0.03 mm off a wire joins that wire's net when the
    /// caller asked for 0.05 and stays an island when it asked for 0.01.
    #[tokio::test]
    async fn the_graph_attaches_at_the_index_tolerance() {
        let sch = schematic(
            "\t(wire\n\t\t(pts (xy 100 80) (xy 120 80))\n\t\t(uuid \"w1\")\n\t)\n\t(label \"NETA\"\n\t\t(at 110 80.03 0)\n\t\t(uuid \"l1\")\n\t)\n",
        );
        let (tree, wires, labels) = index_for(&sch);

        for (tolerance, expected) in [(0.05, Some("NETA".to_string())), (0.01, None)] {
            let mut graph = graph_at(&tree, &wires, &labels, tolerance);
            assert_eq!(
                graph.net_at(100.0, 80.0),
                expected,
                "tolerance {tolerance} should have given {expected:?}"
            );
        }
    }

    /// A wire *ending* on a sheet pin reaches it, so the net does not stop at
    /// the sheet boundary — the blindness this index was built to remove.
    #[tokio::test]
    async fn a_wire_ending_on_a_sheet_pin_joins_its_net() {
        let sch = schematic(&format!(
            "\t(wire\n\t\t(pts (xy 100 80) (xy 120 80))\n\t\t(uuid \"w1\")\n\t)\n\t(label \"NETA\"\n\t\t(at 100 80 0)\n\t\t(uuid \"l1\")\n\t)\n{}",
            sheet(120.0, 70.0, 120.0, 80.0),
        ));
        let (tree, wires, labels) = index_for(&sch);
        let buses = Vec::new();
        let index = ConnectivityIndex::build(&tree, &wires, &buses, &labels, COINCIDENT_TOLERANCE);

        assert!(index.has_sheet_pin(120.0, 80.0));
        let mut graph = net_graph_for(&tree, &wires, &labels);
        assert_eq!(graph.net_at(120.0, 80.0), Some("NETA".to_string()));
    }

    /// A sheet pin a wire merely passes through is a pin landing mid-wire, and
    /// KiCad connects one of those only through a junction dot (#104). Merging
    /// without the dot would invent a short between the net on the wire and
    /// whatever the sheet pin carries.
    #[tokio::test]
    async fn a_sheet_pin_mid_wire_joins_its_net_only_through_a_junction() {
        for (junction, expected) in [
            ("", None),
            (
                "\t(junction (at 120 80) (diameter 0) (color 0 0 0 0) (uuid \"j1\"))\n",
                Some("NETA".to_string()),
            ),
        ] {
            let sch = schematic(&format!(
                "\t(wire\n\t\t(pts (xy 100 80) (xy 140 80))\n\t\t(uuid \"w1\")\n\t)\n\t(label \"NETA\"\n\t\t(at 100 80 0)\n\t\t(uuid \"l1\")\n\t)\n{junction}{}",
                sheet(110.0, 70.0, 120.0, 80.0),
            ));
            let (tree, wires, labels) = index_for(&sch);
            let buses = Vec::new();
            let index =
                ConnectivityIndex::build(&tree, &wires, &buses, &labels, COINCIDENT_TOLERANCE);

            assert!(index.has_sheet_pin(120.0, 80.0));
            let mut graph = net_graph_for(&tree, &wires, &labels);
            assert_eq!(
                graph.net_at(120.0, 80.0),
                expected,
                "junction present: {}",
                !junction.is_empty()
            );
        }
    }
    fn power_symbol(reference: &str, value: &str, x: f64, y: f64) -> String {
        format!(
            "\t(symbol\n\t\t(lib_id \"power:GND\")\n\t\t(at {x} {y} 0)\n\t\t(unit 1)\n\t\t(uuid \"p1\")\n\t\t(property \"Reference\" \"{reference}\"\n\t\t\t(at {x} {y} 0)\n\t\t)\n\t\t(property \"Value\" \"{value}\"\n\t\t\t(at {x} {y} 0)\n\t\t)\n\t)\n"
        )
    }

    /// The real KiCad fixture, at the graph level: `TP7` reaches the `+3V3`
    /// rail only through the `ALT` label sitting on the second `+3V3` power
    /// symbol's own pin. KiCad nets them together (`+3V3` has five pins), so a
    /// graph that keeps one name per point loses that join — and with it the
    /// alias every classifier reads.
    ///
    /// Provenance and KiCad's own net table: `two_name_nets.README.md`.
    #[tokio::test]
    async fn an_alias_on_a_power_symbol_pin_joins_its_segment() {
        let sch = include_str!("../../tests/fixtures/two_name_nets.kicad_sch");
        let (tree, wires, labels) = index_for(sch);
        let mut graph = net_graph_for(&tree, &wires, &labels);

        // TP7's pin, and U1's VCC pin at the other end of the rail.
        let stub = graph.root_at(30.48, 88.9);
        let rail = graph.root_at(110.49, 72.39);
        assert_eq!(stub, rail, "TP7 joins the rail through the ALT alias");
        assert_eq!(graph.name_of_root(rail), Some("+3V3".to_string()));
        assert!(
            graph.aliases_of_root(rail).contains("ALT"),
            "the alias survives to the classifiers: {:?}",
            graph.aliases_of_root(rail)
        );

        // The same join without an alias: two `+3V3` power symbols on separate
        // stubs are one rail, and C1's pin reaches U1's through it.
        assert_eq!(graph.root_at(59.69, 96.52), rail);

        // And a plain label doing it: `U1.5` and `R1.2` carry `SDA` on segments
        // that share no wire. KiCad nets both (`/SDA` has two pins).
        assert_eq!(graph.root_at(120.65, 77.47), graph.root_at(160.02, 63.5));
    }

    /// A power symbol's net name is synthesised as a pseudo-label sitting on
    /// the symbol's own pin. Index that as a label and every power pin sees a
    /// label at its own position and reports itself attached — so an unwired
    /// GND, one of the commonest real mistakes, becomes invisible.
    #[tokio::test]
    async fn an_unwired_power_symbol_is_still_an_orphan() {
        let sch = format!(
            "(kicad_sch\n\t(version 20260306)\n\t(generator \"eeschema\")\n\t(uuid \"root\")\n\t(lib_symbols\n\t\t(symbol \"power:GND\"\n\t\t\t(power)\n\t\t\t(symbol \"GND_0_1\"\n\t\t\t\t(pin power_in line (at 0 0 270) (length 0)\n\t\t\t\t\t(name \"~\") (number \"1\"))\n\t\t\t)\n\t\t)\n\t)\n{}\t(sheet_instances (path \"/\" (page \"1\")))\n)\n",
            power_symbol("#PWR01", "GND", 100.0, 80.0)
        );

        let orphans = call("find_orphan_items", &sch, json!({})).await;
        assert_eq!(orphans["orphan_count"], 1, "{orphans}");
        assert_eq!(orphans["orphans"][0]["type"], "unconnected_pin");
        assert_eq!(orphans["orphans"][0]["reference"], "#PWR01");
    }

    /// Values are read from the instance that placed each pin, not looked up by
    /// reference: on a pre-annotation sheet every part is `R?`, and a
    /// reference-keyed map would report one arbitrary value for all of them.
    #[tokio::test]
    async fn unannotated_components_keep_their_own_values() {
        let with_value = |reference: &str, uuid: &str, value: &str, x: f64| {
            symbol(reference, uuid, x, 80.0).replace(
                "(property \"Reference\"",
                &format!("(property \"Value\" \"{value}\"\n\t\t\t(at {x} 80 0)\n\t\t)\n\t\t(property \"Reference\""),
            )
        };
        let sch = schematic(&format!(
            "{}{}",
            with_value("R?", "r1", "1k", 100.0),
            with_value("R?", "r2", "10k", 120.0),
        ));

        let body = call("validate_component_connections", &sch, json!({})).await;
        let mut values: Vec<&str> = body["unconnected_pins"]
            .as_array()
            .unwrap()
            .iter()
            .map(|pin| pin["value"].as_str().unwrap())
            .collect();
        values.sort_unstable();
        assert_eq!(values, ["10k", "1k"], "{body}");
    }

    /// The power-symbol trap again, one layer down. A pseudo-label is kept out
    /// of the index's label points, but the *graph* still holds it — so a
    /// reachability fallback answered "connected" for a power symbol attached
    /// to nothing, disagreeing with `find_orphan_items` on the same sheet.
    ///
    /// There is no such fallback now: every point the graph can name is a wire
    /// endpoint, label, junction or sheet pin, and `attaches_pin` already
    /// covers all four at tolerance rather than at exact `pt_key` equality.
    #[tokio::test]
    async fn an_unwired_power_symbol_is_unconnected_for_every_tool() {
        let sch = format!(
            "(kicad_sch\n\t(version 20260306)\n\t(generator \"eeschema\")\n\t(uuid \"root\")\n\t(lib_symbols\n\t\t(symbol \"power:GND\"\n\t\t\t(power)\n\t\t\t(symbol \"GND_0_1\"\n\t\t\t\t(pin power_in line (at 0 0 270) (length 0)\n\t\t\t\t\t(name \"~\") (number \"1\"))\n\t\t\t)\n\t\t)\n\t)\n{}\t(sheet_instances (path \"/\" (page \"1\")))\n)\n",
            power_symbol("#PWR01", "GND", 100.0, 80.0)
        );

        let orphans = call("find_orphan_items", &sch, json!({})).await;
        assert_eq!(orphans["orphan_count"], 1, "{orphans}");
        assert_eq!(orphans["orphans"][0]["reference"], "#PWR01");

        let components = call("validate_component_connections", &sch, json!({})).await;
        assert_eq!(components["unconnected_count"], 1, "{components}");
        assert_eq!(components["unconnected_pins"][0]["reference"], "#PWR01");
    }

    /// A power symbol wired to something is connected for both — the agreement
    /// has to hold in the direction that reports no fault, too.
    #[tokio::test]
    async fn a_wired_power_symbol_is_connected_for_every_tool() {
        let sch = format!(
            "(kicad_sch\n\t(version 20260306)\n\t(generator \"eeschema\")\n\t(uuid \"root\")\n\t(lib_symbols\n\t\t(symbol \"power:GND\"\n\t\t\t(power)\n\t\t\t(symbol \"GND_0_1\"\n\t\t\t\t(pin power_in line (at 0 0 270) (length 0)\n\t\t\t\t\t(name \"~\") (number \"1\"))\n\t\t\t)\n\t\t)\n\t\t(symbol \"Test:P1\"\n\t\t\t(symbol \"P1_1_1\"\n\t\t\t\t(pin passive line (at 0 0 0) (length 2.54)\n\t\t\t\t\t(name \"~\") (number \"1\"))\n\t\t\t)\n\t\t)\n\t)\n\t(wire\n\t\t(pts (xy 100 80) (xy 120 80))\n\t\t(uuid \"w1\")\n\t)\n{}{}\t(sheet_instances (path \"/\" (page \"1\")))\n)\n",
            power_symbol("#PWR01", "GND", 100.0, 80.0),
            symbol("U1", "u1", 120.0, 80.0),
        );

        let orphans = call("find_orphan_items", &sch, json!({})).await;
        assert_eq!(orphans["orphan_count"], 0, "{orphans}");

        let components = call("validate_component_connections", &sch, json!({})).await;
        assert_eq!(components["unconnected_count"], 0, "{components}");
    }
}
