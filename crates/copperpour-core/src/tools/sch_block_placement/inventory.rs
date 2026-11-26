use super::super::sch_connectivity::{
    redraw_net_graph, redraw_point_key, snapshot_net_graph, NetRoot,
};
use super::super::{resolved_placed_pins_by_reference, UNRESOLVED_PIN_GEOMETRY};
use copperpour_sexp::schematic::{
    extract_labels, extract_power_symbol_labels, extract_wires, find_lib_symbol, pin_endpoint,
    pin_outward_direction, LibPin, SymbolInstance, Wire,
};
use copperpour_sexp::SexpNode;
use serde::Serialize;
use std::collections::{BTreeMap, BTreeSet};

pub(super) type Point = (f64, f64);

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd, serde::Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(super) struct SymbolId<'a> {
    pub uuid: &'a str,
    pub unit: u32,
}

pub(super) struct Pin {
    pub lib: LibPin,
    pub tip: Point,
    pub outward: f64,
    pub net: NetRoot,
    pub drawn: NetRoot,
}

pub(super) struct Part<'a> {
    pub id: SymbolId<'a>,
    pub inst: SymbolInstance,
    pub lib: &'a SexpNode,
    pub node: &'a SexpNode,
    pub pins: Vec<Pin>,
}

impl Part<'_> {
    pub fn pin(&self, number: &str) -> Option<&Pin> {
        self.pins.iter().find(|p| p.lib.number == number)
    }

    fn library_reference(&self) -> Option<&str> {
        self.lib
            .find_all("property")
            .into_iter()
            .find(|p| p.get(1).and_then(SexpNode::as_str) == Some("Reference"))?
            .get(2)?
            .as_str()
    }

    pub fn is_connector(&self) -> bool {
        self.inst.lib_id.starts_with("Connector")
    }

    fn is_support(&self) -> bool {
        self.pins.len() == 2
            && self.lib.find("power").is_none()
            && !self.is_connector()
            && self.pins.iter().all(|p| p.lib.electrical_type == "passive")
    }

    fn is_possible_owner(&self) -> bool {
        !self.is_connector()
            && self.lib.find("power").is_none()
            && self.pins.iter().any(|p| {
                matches!(
                    p.lib.electrical_type.as_str(),
                    "input"
                        | "output"
                        | "bidirectional"
                        | "open_collector"
                        | "open_emitter"
                        | "tri_state"
                        | "power_in"
                        | "power_out"
                )
            })
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum Role<'a> {
    Series {
        near: &'a str,
        serves: &'a str,
    },
    Shunt {
        near: &'a str,
        serves: &'a str,
        ground: bool,
    },
    Feedback {
        input: (&'a str, &'a str),
        output: (&'a str, &'a str),
    },
    Bypass {
        supply: &'a str,
        serves: &'a str,
    },
}

#[derive(Serialize)]
pub(super) struct PinEvidence<'a> {
    #[serde(flatten)]
    symbol: SymbolId<'a>,
    reference: &'a str,
    pin: &'a str,
}

impl<'a> PinEvidence<'a> {
    fn new(part: &'a Part<'a>, pin: &'a Pin) -> Self {
        Self {
            symbol: part.id,
            reference: &part.inst.reference,
            pin: &pin.lib.number,
        }
    }
}

#[derive(Serialize)]
pub(super) struct Membership<'a> {
    #[serde(flatten)]
    symbol: SymbolId<'a>,
    reference: &'a str,
    anchor_pins: Vec<PinEvidence<'a>>,
    competing_pins: Vec<PinEvidence<'a>>,
    suggested: bool,
    reason: &'static str,
}

pub(super) struct Inventory<'a> {
    pub parts: BTreeMap<&'a str, Part<'a>>,
    pub wires: Vec<Wire>,
    rails: BTreeMap<NetRoot, bool>,
}

impl<'a> Inventory<'a> {
    pub fn new(tree: &'a SexpNode) -> Result<Self, String> {
        if ["bus", "bus_entry", "bus_alias", "sheet"]
            .iter()
            .any(|tag| tree.find(tag).is_some())
        {
            return Err("block placement requires a sheet without buses or child sheets".into());
        }
        let instances = resolved_placed_pins_by_reference(tree).ok_or(UNRESOLVED_PIN_GEOMETRY)?;
        let wires = extract_wires(tree);
        if wires.len() != tree.find_all("wire").len()
            || wires
                .iter()
                .any(|w| ![w.x1, w.y1, w.x2, w.y2].into_iter().all(f64::is_finite))
        {
            return Err("one or more wires have unresolved geometry".into());
        }
        let mut ids = BTreeSet::new();
        for object in tree.children().into_iter().flatten() {
            if let Some(id) = object.find_str("uuid") {
                if !ids.insert(id) {
                    return Err(format!("duplicate placed UUID {id}"));
                }
            } else if matches!(
                object.head(),
                Some("symbol" | "wire" | "junction" | "text" | "text_box")
            ) {
                return Err(format!(
                    "placed {} has no UUID",
                    object.head().unwrap_or("object")
                ));
            }
        }
        let nodes: BTreeMap<_, _> = tree
            .find_all("symbol")
            .into_iter()
            .filter_map(|n| Some((n.find_str("uuid")?, n)))
            .collect();
        if nodes.len() != instances.len() {
            return Err(UNRESOLVED_PIN_GEOMETRY.into());
        }
        let libraries = tree
            .find("lib_symbols")
            .map(|node| node.find_all("symbol"))
            .unwrap_or_default();
        let mut graph = snapshot_net_graph(tree, &wires);
        let mut drawn = redraw_net_graph(tree, &wires, &extract_labels(tree));
        let mut rails = BTreeMap::new();
        for label in extract_power_symbol_labels(tree) {
            let ground = is_ground(&label.net);
            let root = graph.root_at(label.x, label.y);
            rails
                .entry(root)
                .and_modify(|g| *g &= ground)
                .or_insert(ground);
        }
        let mut parts = BTreeMap::new();
        for (inst, pins) in instances {
            let (uuid, node) = inst
                .uuid
                .as_deref()
                .and_then(|id| nodes.get_key_value(id))
                .ok_or(UNRESOLVED_PIN_GEOMETRY)?;
            let lib = find_lib_symbol(&libraries, &inst).ok_or(UNRESOLVED_PIN_GEOMETRY)?;
            let id = SymbolId {
                uuid,
                unit: inst.unit,
            };
            let mut numbers = BTreeSet::new();
            let mut tips = BTreeSet::new();
            let mut observed = Vec::new();
            for (pin, transform) in pins {
                let tip = pin_endpoint(&pin, transform);
                let outward = pin_outward_direction(&pin, transform);
                if ![tip.0, tip.1, outward].into_iter().all(f64::is_finite) {
                    return Err(format!("{} has an unreadable pin pose", inst.reference));
                }
                if !numbers.insert(pin.number.clone())
                    || !tips.insert(redraw_point_key(tip.0, tip.1))
                {
                    return Err(format!("{} has shared or stacked pins", inst.reference));
                }
                observed.push(Pin {
                    lib: pin,
                    tip,
                    outward,
                    net: graph.root_at(tip.0, tip.1),
                    drawn: drawn.root_at(tip.0, tip.1),
                });
            }
            parts.insert(
                *uuid,
                Part {
                    id,
                    inst,
                    lib,
                    node,
                    pins: observed,
                },
            );
        }
        Ok(Self {
            parts,
            wires,
            rails,
        })
    }

    pub fn symbol(&self, id: SymbolId<'_>) -> Result<&Part<'a>, String> {
        self.parts
            .get(id.uuid)
            .filter(|p| p.id.unit == id.unit)
            .ok_or_else(|| format!("no placed symbol {} unit {}", id.uuid, id.unit))
    }

    pub fn is_rail(&self, root: NetRoot) -> bool {
        self.rails.contains_key(&root)
    }

    pub fn is_ground(&self, root: NetRoot) -> bool {
        self.rails.get(&root) == Some(&true)
    }

    fn has_other_feedback_path(
        &self,
        member: &Part<'_>,
        from: NetRoot,
        reaches_anchor: impl Fn(NetRoot) -> bool,
    ) -> bool {
        let mut visited = BTreeSet::new();
        let mut pending = vec![from];
        while let Some(net) = pending.pop() {
            if reaches_anchor(net) {
                return true;
            }
            if !visited.insert(net) {
                continue;
            }
            for other in self
                .parts
                .values()
                .filter(|p| p.id != member.id && p.is_support())
            {
                let [a, b] = other.pins.as_slice() else {
                    continue;
                };
                let next = if a.net == net {
                    b.net
                } else if b.net == net {
                    a.net
                } else {
                    continue;
                };
                if !self.is_rail(next) {
                    pending.push(next);
                }
            }
        }
        false
    }

    fn competitors<'s>(
        &'s self,
        anchor: &Part<'_>,
        root: NetRoot,
    ) -> impl Iterator<Item = PinEvidence<'s>> + 's {
        self.parts
            .values()
            .filter({
                let anchor_uuid = anchor.id.uuid.to_owned();
                move |part| part.id.uuid != anchor_uuid && part.is_possible_owner()
            })
            .flat_map(move |part| {
                part.pins
                    .iter()
                    .filter(move |pin| pin.net == root && pin.lib.electrical_type != "no_connect")
                    .map(|pin| PinEvidence::new(part, pin))
            })
    }

    pub fn membership<'s>(&'s self, anchor: &'s Part<'a>) -> Vec<Membership<'s>> {
        let mut suggestions = Vec::new();
        for part in self
            .parts
            .values()
            .filter(|p| p.id.uuid != anchor.id.uuid && p.is_support())
        {
            // When a signal is present it is ownership evidence; a shared ground
            // on the other leg of the part is not evidence of a competing owner.
            let matching = |pin: &&Pin| part.pins.iter().any(|p| p.net == pin.net);
            let signal: Vec<_> = anchor
                .pins
                .iter()
                .filter(matching)
                .filter(|p| !self.is_rail(p.net))
                .collect();
            let rail_only = signal.is_empty();
            let pins: Vec<_> = if rail_only {
                anchor
                    .pins
                    .iter()
                    .filter(matching)
                    .filter(|p| !self.is_ground(p.net))
                    .collect()
            } else {
                signal
            };
            if pins.is_empty() {
                continue;
            }
            let roots: BTreeSet<_> = pins.iter().map(|pin| pin.net).collect();
            let competing_pins: Vec<_> = roots
                .iter()
                .flat_map(|&root| self.competitors(anchor, root))
                .collect();
            let local = pins
                .iter()
                .any(|a| part.pins.iter().any(|p| p.drawn == a.drawn));
            let suggested = !rail_only && local && competing_pins.is_empty();
            let reason = if rail_only {
                "a shared rail does not establish which pin the part serves"
            } else if !competing_pins.is_empty() {
                "the signal also reaches another possible owner"
            } else if !local {
                "the matching net is not drawn locally"
            } else {
                "a locally drawn signal reaches only this anchor"
            };
            suggestions.push(Membership {
                symbol: part.id,
                reference: &part.inst.reference,
                anchor_pins: pins
                    .into_iter()
                    .map(|pin| PinEvidence::new(anchor, pin))
                    .collect(),
                competing_pins,
                suggested,
                reason,
            });
        }
        suggestions
    }

    pub fn role<'s>(
        &'s self,
        anchor: &'s Part<'a>,
        part: &'s Part<'a>,
        assigned: Option<&str>,
    ) -> Result<Role<'s>, &'static str> {
        if !part.is_support() {
            return Err("only two-pin passive supporting parts can be placed");
        }
        let [p, q] = part.pins.as_slice() else {
            unreachable!("is_support")
        };
        if p.net == q.net {
            return Err("both member pins are already on one net");
        }
        let on_anchor = |pin: &Pin| {
            anchor
                .pins
                .iter()
                .filter(|a| a.net == pin.net)
                .collect::<Vec<_>>()
        };
        let choose = |pin: &Pin| {
            let matches = on_anchor(pin);
            if let Some(assigned) = assigned {
                return matches
                    .into_iter()
                    .find(|a| a.lib.number == assigned)
                    .ok_or("the member does not reach the assigned pin");
            }
            if self.competitors(anchor, pin.net).next().is_some() {
                return Err("the member has competing owners; assign the pin it serves");
            }
            match matches.as_slice() {
                [one] => Ok(*one),
                [] => Err("the member does not reach the anchor"),
                _ => Err("the member reaches multiple anchor pins; assign the pin it serves"),
            }
        };
        match (self.is_rail(p.net), self.is_rail(q.net)) {
            (true, true) => {
                if part.library_reference() != Some("C") {
                    return Err("a part across two rails must be a capacitor");
                }
                if assigned.is_none() {
                    return Err("assign the supply pin served by this capacitor");
                }
                let supply = match (self.is_ground(p.net), self.is_ground(q.net)) {
                    (true, false) => q,
                    (false, true) => p,
                    _ => return Err("the bypass needs one supply and one ground pin"),
                };
                let served = choose(supply)?;
                if !matches!(
                    served.lib.electrical_type.as_str(),
                    "power_in" | "power_out"
                ) {
                    return Err("the assigned anchor pin is not a supply pin");
                }
                Ok(Role::Bypass {
                    supply: &supply.lib.number,
                    serves: &served.lib.number,
                })
            }
            (true, false) | (false, true) => {
                let (near, rail) = if self.is_rail(p.net) { (q, p) } else { (p, q) };
                let served = choose(near)?;
                if near.drawn != served.drawn {
                    return Err("the shunt has no drawn leg to the served anchor pin");
                }
                Ok(Role::Shunt {
                    near: &near.lib.number,
                    serves: &served.lib.number,
                    ground: self.is_ground(rail.net),
                })
            }
            (false, false) => match (on_anchor(p).is_empty(), on_anchor(q).is_empty()) {
                (true, true) => Err("the member does not reach the anchor"),
                (true, false) | (false, true) => {
                    let (near, far) = if on_anchor(p).is_empty() {
                        (q, p)
                    } else {
                        (p, q)
                    };
                    let served = choose(near)?;
                    let target = match served.lib.electrical_type.as_str() {
                        "input" => Some("output"),
                        "output" => Some("input"),
                        _ => None,
                    };
                    if let Some(target) = target {
                        if self.has_other_feedback_path(part, far.net, |net| {
                            anchor
                                .pins
                                .iter()
                                .any(|pin| pin.net == net && pin.lib.electrical_type == target)
                        }) {
                            return Err("a multi-part feedback path cannot be placed");
                        }
                    }
                    Ok(Role::Series {
                        near: &near.lib.number,
                        serves: &served.lib.number,
                    })
                }
                (false, false) => {
                    let opposite =
                        |a: &Pin, b: &Pin| (a.outward - b.outward).rem_euclid(360.0) == 180.0;
                    let (a, b) = match (on_anchor(p).as_slice(), on_anchor(q).as_slice()) {
                        ([a], [b]) if opposite(a, b) => (*a, *b),
                        _ => return Err("feedback needs two unique, opposite-facing anchor pins"),
                    };
                    if p.drawn != a.drawn || q.drawn != b.drawn {
                        return Err("the feedback resistor has no drawn leg to both anchor pins");
                    }
                    if part.library_reference() != Some("R") {
                        return Err("only a one-resistor feedback path can be placed");
                    }
                    if self.has_other_feedback_path(part, p.net, |root| root == q.net) {
                        return Err("a parallel or multi-part feedback path cannot be placed");
                    }
                    if let Some(number) = assigned {
                        if [a, b].iter().all(|pin| pin.lib.number != number) {
                            return Err("the feedback resistor does not reach the assigned pin");
                        }
                    } else if [a, b]
                        .iter()
                        .any(|pin| self.competitors(anchor, pin.net).next().is_some())
                    {
                        return Err("a feedback signal also reaches another possible owner");
                    }
                    let pair = |member: &'s Pin, anchor: &'s Pin| {
                        (member.lib.number.as_str(), anchor.lib.number.as_str())
                    };
                    match (
                        a.lib.electrical_type.as_str(),
                        b.lib.electrical_type.as_str(),
                    ) {
                        ("input", "output") => Ok(Role::Feedback {
                            input: pair(p, a),
                            output: pair(q, b),
                        }),
                        ("output", "input") => Ok(Role::Feedback {
                            input: pair(q, b),
                            output: pair(p, a),
                        }),
                        _ => Err("feedback needs a signal input and output"),
                    }
                }
            },
        }
    }
}

fn is_ground(name: &str) -> bool {
    let name = name.to_ascii_uppercase();
    name.contains("GND") || matches!(name.as_str(), "VSS" | "VSSA") || name.starts_with("EARTH")
}
