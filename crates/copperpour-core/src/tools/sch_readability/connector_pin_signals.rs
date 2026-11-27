use crate::mcp::{error::ToolErrorKind, protocol::CallToolResult};
use crate::tool;
use crate::tools::{
    get_path, opt_str_list,
    placement::ref_prefix,
    sch_connectivity::{
        add_symbol_power_drivers, drawn_net_graph_for, label_roots, schematic_pin_endpoint,
        ConnectivityIndex, NamingPin, NetRoot, PinNetNames, SymbolPower, UnitNotation,
        COINCIDENT_TOLERANCE,
    },
    sch_hierarchy::natural_compare,
    symbol_body_style, ToolContext, ToolDef, UNRESOLVED_PIN_GEOMETRY,
};
use copperpour_sexp::schematic::{
    extract_buses, extract_labels, extract_sheet_pins, extract_wires, find_lib_symbol,
    lib_pin_nodes_for_body_style, parse_lib_pin, pin_endpoint, read_schematic, symbol_instance,
    Label, LabelKind, LibPin, SymbolInstance,
};
use copperpour_sexp::SexpNode;
use serde_json::{json, Value};
use std::collections::{BTreeSet, HashMap, HashSet};

pub(super) fn tool() -> ToolDef {
    tool!(
        "check_connector_pin_signals",
        "Report which connector pins show their signal on one saved schematic sheet. \
         Lists connected pins with no signal evidence as findings.",
        json!({ "type": "object",
            "properties": {
                "schematic": { "type": "string", "description": "Path to the saved .kicad_sch file" },
                "references": {
                    "type": "array", "items": { "type": "string" },
                    "description": "Exact references to check instead of the default J prefix. \
                                    An empty list selects nothing. Power symbols are always excluded."
                }
            },
            "required": ["schematic"] }),
        |args, ctx| async move { handle_check_connector_pin_signals(args, ctx).await }
    )
}

fn hides_pin_names(lib: &SexpNode) -> bool {
    lib.find("pin_names").is_some_and(|names| {
        names.find_str("hide") == Some("yes")
            || names
                .children()
                .unwrap_or_default()
                .iter()
                .any(|node| node.as_str() == Some("hide"))
    })
}

fn meaningful_name(name: &str, number: &str) -> bool {
    let generic = name.strip_prefix("Pin_").is_some_and(|suffix| {
        !suffix.is_empty() && suffix.bytes().all(|byte| byte.is_ascii_digit())
    });
    !name.is_empty() && name != "~" && name != number && !generic
}

struct DrawnPin {
    net_at: (f64, f64),
    pin: LibPin,
    at: (f64, f64),
    name_hidden: bool,
    alternate_name: Option<String>,
}

impl DrawnPin {
    fn visible_name(&self) -> Option<&str> {
        let name = self
            .alternate_name
            .as_deref()
            .unwrap_or(&self.pin.name)
            .trim();
        (!self.name_hidden && !name.is_empty() && name != "~").then_some(name)
    }

    fn signal_name(&self) -> Option<&str> {
        self.visible_name()
            .filter(|name| meaningful_name(name, &self.pin.number))
    }
}

struct DrawnSymbol {
    instance: SymbolInstance,
    pins: Vec<DrawnPin>,
    power: SymbolPower,
}

fn read_symbol(node: &SexpNode, libs: &[&SexpNode]) -> Result<DrawnSymbol, String> {
    let instance = symbol_instance(node).ok_or("a placed symbol lacks a library ID or position")?;
    let lib = find_lib_symbol(libs, &instance)
        .ok_or_else(|| format!("{}: {UNRESOLVED_PIN_GEOMETRY}", instance.reference))?;
    let transform = instance.pin_transform();
    let alternates: HashMap<_, _> = node
        .find_all("pin")
        .into_iter()
        .filter_map(|pin| Some((pin.get(1)?.as_str()?, pin.find_str("alternate")?)))
        .collect();
    let names_hidden = hides_pin_names(lib);
    let mut pins = Vec::new();
    for node in lib_pin_nodes_for_body_style(lib, instance.unit, symbol_body_style(node)) {
        let mut pin = parse_lib_pin(node)
            .filter(|pin| {
                node.find_str("number").is_some()
                    && pin.local_x.is_finite()
                    && pin.local_y.is_finite()
                    && pin.rotation.is_finite()
            })
            .ok_or_else(|| {
                format!(
                    "{}: a pin has invalid geometry or no number",
                    instance.reference
                )
            })?;
        let mut alternate_name = None;
        if let Some(&alternate) = alternates
            .get(pin.number.as_str())
            .filter(|name| !name.is_empty())
        {
            if let Some(kind) = node.find_all("alternate").into_iter().find_map(|alt| {
                (alt.get(1)?.as_str()? == alternate)
                    .then(|| alt.get(2)?.as_str())
                    .flatten()
            }) {
                pin.electrical_type = kind.to_owned();
                alternate_name = Some(alternate.to_owned());
            }
        }
        let name_hidden = names_hidden
            || pin.hidden
            || node
                .find("name")
                .and_then(|name| name.find("effects"))
                .and_then(|effects| effects.find("font"))
                .and_then(|font| font.find("size"))
                .and_then(|size| size.get_f64(1))
                == Some(0.0);
        let at = pin_endpoint(&pin, transform);
        pins.push(DrawnPin {
            net_at: schematic_pin_endpoint(&pin, transform),
            pin,
            at,
            name_hidden,
            alternate_name,
        });
    }
    pins.sort_by(|a, b| {
        natural_compare(&a.pin.number, &b.pin.number).then_with(|| a.pin.number.cmp(&b.pin.number))
    });
    Ok(DrawnSymbol {
        instance,
        pins,
        power: SymbolPower::from_library(lib),
    })
}

fn no_connect_reason(
    index: &ConnectivityIndex<'_>,
    pin: &LibPin,
    (x, y): (f64, f64),
) -> Option<&'static str> {
    if index.has_no_connect(x, y) {
        Some("flag")
    } else if pin.electrical_type == "no_connect" {
        Some("pin_type")
    } else {
        None
    }
}

fn nearest_mark(
    marks: Option<&Vec<&Label>>,
    at: (f64, f64),
    matches: impl Fn(LabelKind) -> bool,
) -> Option<Value> {
    marks?
        .iter()
        .filter(|mark| matches(mark.kind))
        .map(|mark| (mark, (mark.x - at.0).hypot(mark.y - at.1)))
        .min_by(|(a, da), (b, db)| {
            da.total_cmp(db)
                .then_with(|| a.net.cmp(&b.net))
                .then_with(|| a.x.total_cmp(&b.x))
                .then_with(|| a.y.total_cmp(&b.y))
        })
        .map(|(mark, distance_mm)| {
            json!({
                "name": mark.net, "x": mark.x, "y": mark.y, "distance_mm": distance_mm
            })
        })
}

async fn handle_check_connector_pin_signals(
    args: &Value,
    _ctx: &ToolContext,
) -> anyhow::Result<CallToolResult> {
    let path = get_path(args, "schematic")?;
    let references = match opt_str_list(args, "references") {
        Ok(references) => references.map(|refs| refs.into_iter().collect::<BTreeSet<_>>()),
        Err(error) => return Ok(error),
    };
    let (_, tree) = read_schematic(&path)?;
    let malformed = |reason: &str| {
        CallToolResult::error_kind(
            ToolErrorKind::MalformedFile {
                path: path.display().to_string(),
                reason: reason.into(),
            },
            format!("Cannot check connector pin signals: {reason}. Nothing was written."),
        )
    };
    if tree.head() != Some("kicad_sch") {
        return Ok(malformed("expected a KiCad schematic"));
    }

    let libs = tree
        .find("lib_symbols")
        .map(|node| node.find_all("symbol"))
        .unwrap_or_default();
    let placed = match tree
        .find_all("symbol")
        .into_iter()
        .map(|node| read_symbol(node, &libs))
        .collect::<Result<Vec<_>, _>>()
    {
        Ok(placed) => placed,
        Err(reason) => return Ok(malformed(&reason)),
    };
    let wires = extract_wires(&tree);
    let mut labels = extract_labels(&tree);
    let mut runs = drawn_net_graph_for(&tree, &wires, &labels);
    let mut nets = runs.clone();
    for symbol in &placed {
        add_symbol_power_drivers(
            &mut nets,
            &symbol.instance,
            symbol.power,
            symbol.pins.iter().map(|drawn| (&drawn.pin, drawn.net_at)),
            |mark| labels.push(mark),
        );
    }
    let buses = extract_buses(&tree);
    let index = ConnectivityIndex::with_placed_pins(
        &tree,
        &wires,
        &buses,
        &labels,
        COINCIDENT_TOLERANCE,
        placed
            .iter()
            .map(|symbol| {
                (
                    symbol.instance.clone(),
                    symbol
                        .pins
                        .iter()
                        .filter(|drawn| drawn.pin.electrical_type != "no_connect")
                        .map(|drawn| (drawn.pin.clone(), symbol.instance.pin_transform()))
                        .collect(),
                )
            })
            .collect(),
    );
    let mut nets = nets.into_named_nets();
    let notation = UnitNotation::project_notation(&path);
    let pin_nets = PinNetNames::build_with_pins(
        &tree,
        &mut nets,
        notation.unwrap_or_default(),
        true,
        |index, _, _, _| {
            placed[index]
                .pins
                .iter()
                .map(|drawn| NamingPin {
                    pin: drawn.pin.clone(),
                    alternate_name: drawn.alternate_name.clone(),
                    global_power: drawn.pin.hidden && drawn.pin.electrical_type == "power_in",
                })
                .collect()
        },
    );
    let mut placed: Vec<_> = placed.iter().collect();
    placed.sort_by(|a, b| {
        let (a, b) = (&a.instance, &b.instance);
        natural_compare(&a.reference, &b.reference)
            .then_with(|| a.reference.cmp(&b.reference))
            .then_with(|| a.unit.cmp(&b.unit))
            .then_with(|| a.uuid.cmp(&b.uuid))
    });
    let mut marks_by_run = HashMap::<NetRoot, Vec<&Label>>::new();
    for (root, label) in label_roots(&mut runs, &labels) {
        marks_by_run.entry(root).or_default().push(label);
    }
    let mut net_evidence = HashMap::<NetRoot, (bool, bool)>::new();
    for (root, label) in label_roots(&mut nets, &labels) {
        let (has_label, has_power) = net_evidence.entry(root).or_default();
        match label.kind {
            LabelKind::PowerSymbol => *has_power = true,
            _ => *has_label = true,
        }
    }
    let sheet_pin_nets: HashSet<_> = extract_sheet_pins(&tree)
        .into_iter()
        .map(|(x, y)| nets.root_at(x, y))
        .collect();

    let mut drawn_names_by_net = HashMap::<NetRoot, Vec<(&SymbolInstance, &DrawnPin, &str)>>::new();
    for DrawnSymbol {
        instance,
        pins,
        power,
    } in &placed
    {
        if *power != SymbolPower::Ordinary {
            continue;
        }
        for drawn in pins {
            let at @ (x, y) = drawn.at;
            let pin = &drawn.pin;
            if no_connect_reason(&index, pin, at).is_some() || !index.attaches_pin(x, y) {
                continue;
            }
            if let Some(name) = drawn.visible_name() {
                drawn_names_by_net
                    .entry(nets.root_at(drawn.net_at.0, drawn.net_at.1))
                    .or_default()
                    .push((instance, drawn, name));
            }
        }
    }

    let mut unmatched = references.clone().unwrap_or_default();
    let mut connectors = Vec::new();
    let mut findings = Vec::new();
    for DrawnSymbol {
        instance,
        pins,
        power,
    } in placed
    {
        let reference = &instance.reference;
        let selected_by = match &references {
            _ if *power != SymbolPower::Ordinary => continue,
            Some(refs) if refs.contains(reference) => "references",
            None if ref_prefix(reference) == "J" => "reference_prefix",
            _ => continue,
        };
        unmatched.remove(reference);
        let mut reports = Vec::new();
        for drawn in pins {
            let at @ (x, y) = drawn.at;
            let pin = &drawn.pin;
            let mut evidence = Vec::new();
            let mut details = json!({});
            let status = if let Some(reason) = no_connect_reason(&index, pin, at) {
                details["no_connect"] = json!(reason);
                "no_connect"
            } else if !index.attaches_pin(x, y) {
                "unconnected"
            } else {
                let (px, py) = drawn.net_at;
                let root = nets.root_at(px, py);
                let (has_label, has_power) = net_evidence.get(&root).copied().unwrap_or_default();
                let marks = marks_by_run.get(&runs.root_at(px, py));
                if has_label {
                    evidence.push("label");
                    details["nearest_label"] =
                        json!(nearest_mark(marks, at, |kind| kind != LabelKind::PowerSymbol));
                }
                if has_power {
                    evidence.push("power_symbol");
                    details["nearest_power_symbol"] =
                        json!(nearest_mark(marks, at, |kind| kind == LabelKind::PowerSymbol));
                }
                if sheet_pin_nets.contains(&root) {
                    evidence.push("sheet_pin");
                }
                if let Some(name) = drawn.signal_name() {
                    evidence.push("pin_name");
                    details["pin_name"] = json!(name);
                }
                if evidence.is_empty() {
                    details["net"] = json!(pin_nets.net_of_pin(&mut nets, pin, px, py));
                    details["other_pin_names"] = json!(drawn_names_by_net
                        .get(&root)
                        .into_iter()
                        .flatten()
                        .filter(|(_, other, _)| !std::ptr::eq(drawn, *other))
                        .map(|(symbol, other, name)| json!({
                            "reference": symbol.reference, "unit": symbol.unit,
                            "symbol_uuid": symbol.uuid, "pin": other.pin.number, "name": name,
                            "x": other.at.0, "y": other.at.1
                        }))
                        .collect::<Vec<_>>());
                    findings.push(json!({
                        "reference": reference, "unit": instance.unit, "symbol_uuid": instance.uuid,
                        "pin": pin.number, "net": details["net"], "other_pin_names": details["other_pin_names"]
                    }));
                    "unidentified"
                } else {
                    "identified"
                }
            };
            reports.push(json!({
                "pin": pin.number, "x": x, "y": y, "status": status,
                "evidence": evidence, "context": details
            }));
        }
        connectors.push(json!({
            "reference": reference, "unit": instance.unit, "symbol_uuid": instance.uuid,
            "selected_by": selected_by, "pins": reports
        }));
    }
    Ok(CallToolResult::json(&json!({
        "schematic": path, "source": "saved_schematic",
        "unit_notation_source": if notation.is_some() { "project" } else { "kicad_defaults" },
        "connectors": connectors, "findings": findings, "unidentified_count": findings.len(),
        "unmatched_references": unmatched
    })))
}

#[cfg(test)]
mod tests {
    use super::*;
    use copperpour_sexp::parse_sexp;

    fn pin_name(symbol_flags: &str, pin_flags: &str, name: &str) -> Option<String> {
        let lib = parse_sexp(&format!(
            r#"(symbol "Example" {symbol_flags}
                (pin passive line (at 0 0 0) (length 2.54)
                    {pin_flags} (name "{name}") (number "7")))"#
        ))
        .unwrap();
        let symbol = parse_sexp(r#"(symbol (lib_id "Example") (at 0 0 0))"#).unwrap();
        read_symbol(&symbol, &[&lib]).unwrap().pins[0]
            .signal_name()
            .map(str::to_owned)
    }

    #[test]
    fn symbol_and_pin_visibility_each_hide_meaningful_names() {
        assert_eq!(pin_name("(pin_names (hide yes))", "", "RESET"), None);
        assert_eq!(pin_name("(pin_names hide)", "", "RESET"), None);
        assert_eq!(pin_name("", "(hide yes)", "RESET"), None);
        assert_eq!(
            pin_name("(pin_names (hide no))", "(hide no)", "~{RESET}").as_deref(),
            Some("~{RESET}")
        );
    }

    #[test]
    fn generic_names_are_not_evidence_even_when_shown() {
        for name in ["", " ", "~", "7", "Pin_7", "Pin_12"] {
            assert_eq!(pin_name("", "", name), None, "{name}");
        }
        assert_eq!(
            pin_name("", "", "Pin_ENABLE").as_deref(),
            Some("Pin_ENABLE")
        );
    }

    #[test]
    fn zero_size_pin_name_is_not_plotted() {
        let lib = parse_sexp(
            r#"(symbol "Connector:Sample"
            (pin passive line (at 0 0 0) (number "1")
                (name "RESET" (effects (font (size 0 0))))))"#,
        )
        .unwrap();
        let placed = parse_sexp(r#"(symbol (lib_id "Connector:Sample") (at 0 0 0))"#).unwrap();
        let placed = read_symbol(&placed, &[&lib]).unwrap();
        let drawn = &placed.pins[0];
        assert_eq!(drawn.signal_name(), None);
    }
}
