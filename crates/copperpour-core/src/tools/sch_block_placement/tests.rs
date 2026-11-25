use super::*;
use crate::mcp::handler::McpHandler;
use crate::tools::ServerConfig;
use copperpour_sexp::parse_sexp;
use copperpour_sexp::schematic::{format_junction, format_wire, Wire};
use std::path::Path;

struct Sheet {
    dir: tempfile::TempDir,
    tree: SexpNode,
    path: std::path::PathBuf,
}

impl Sheet {
    fn fixture(name: &str) -> Self {
        let dir = tempfile::tempdir().unwrap();
        let input = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/block_placement_kicad10")
            .join(name);
        let path = dir.path().join(format!("{name}.kicad_sch"));
        std::fs::copy(input.join(path.file_name().unwrap()), &path).unwrap();
        std::fs::copy(
            input.join(format!("{name}.kicad_pro")),
            path.with_extension("kicad_pro"),
        )
        .unwrap();
        let (_, tree) = read_saved_sheet(&path).unwrap();
        Self { dir, tree, path }
    }

    fn id(&self, reference: &str) -> Value {
        let inventory = Inventory::new(&self.tree).unwrap();
        let part = inventory
            .parts
            .values()
            .find(|p| p.inst.reference == reference)
            .unwrap();
        json!(part.id)
    }

    fn saved(&self) -> BTreeMap<std::ffi::OsString, Vec<u8>> {
        std::fs::read_dir(self.dir.path())
            .unwrap()
            .map(|entry| {
                let entry = entry.unwrap();
                (entry.file_name(), std::fs::read(entry.path()).unwrap())
            })
            .collect()
    }

    async fn call(&self, args: Value) -> (Value, Value) {
        let saved = self.saved();
        let handler = McpHandler::new(ServerConfig {
            eager_toolsets: true,
            ..Default::default()
        })
        .await
        .unwrap();
        let reply = handler
            .handle_message(json!({
                "jsonrpc": "2.0", "id": 776, "method": "tools/call",
                "params": { "name": "preview_block_placement", "arguments": args }
            }))
            .await
            .unwrap()
            .result
            .unwrap();
        assert_eq!(self.saved(), saved);
        let body: Value =
            serde_json::from_str(reply["content"][0]["text"].as_str().unwrap()).unwrap();
        (reply, body)
    }

    async fn serve(&self, args: Value) -> Value {
        let (reply, body) = self.call(args).await;
        assert_ne!(reply["isError"], true, "{reply}");
        assert_eq!(
            body["revision"],
            crate::hex::sha256_hex(&std::fs::read(&self.path).unwrap())
        );
        body
    }

    async fn refuses(&self, args: Value, kind: &str) -> Value {
        let (reply, body) = self.call(args).await;
        assert_eq!(reply["isError"], true, "{reply}");
        assert_eq!(body["error"]["kind"], kind, "{body}");
        assert!(body.get("plan").is_none(), "{body}");
        body["error"].clone()
    }

    fn replay(&self, plan: &Value) -> SexpNode {
        let mut tree = self.tree.clone();
        let SexpNode::List(items) = &mut tree else {
            unreachable!()
        };
        items.retain(|item| {
            let id = item.find_str("uuid");
            ![
                "remove_wire_uuids",
                "remove_junction_uuids",
                "removed_power_symbol_uuids",
            ]
            .into_iter()
            .any(|key| {
                plan[key]
                    .as_array()
                    .unwrap()
                    .iter()
                    .any(|v| v.as_str() == id)
            })
        });
        for key in ["poses", "field_moves", "note_moves", "no_connect_moves"] {
            for change in plan[key].as_array().unwrap() {
                let node = items
                    .iter_mut()
                    .find(|n| n.find_str("uuid") == change["uuid"].as_str())
                    .unwrap();
                let node = if key == "field_moves" {
                    let SexpNode::List(fields) = node else {
                        unreachable!()
                    };
                    fields.retain(|f| f.head() != Some("fields_autoplaced"));
                    fields
                        .iter_mut()
                        .find(|f| {
                            f.head() == Some("property")
                                && f.get(1).and_then(SexpNode::as_str) == change["field"].as_str()
                        })
                        .unwrap()
                } else {
                    node
                };
                let angle = change["rotation"]
                    .as_f64()
                    .unwrap_or_else(|| copperpour_sexp::schematic::parse_at(node).unwrap().2);
                poses::set_at(
                    node,
                    (
                        change["x"].as_f64().unwrap(),
                        change["y"].as_f64().unwrap(),
                        angle,
                    ),
                )
                .unwrap();
            }
        }
        for wire in plan["add_wires"].as_array().unwrap() {
            let [(x1, y1), (x2, y2)]: [inventory::Point; 2] =
                serde_json::from_value(wire.clone()).unwrap();
            items.push(parse_sexp(&format_wire(x1, y1, x2, y2)).unwrap());
        }
        for dot in plan["add_junctions"].as_array().unwrap() {
            let (x, y): inventory::Point = serde_json::from_value(dot.clone()).unwrap();
            items.push(parse_sexp(&format_junction(x, y)).unwrap());
        }
        tree
    }

    fn with_tree(mut self, tree: SexpNode) -> Self {
        std::fs::write(&self.path, candidate::to_source(&tree)).unwrap();
        self.tree = tree;
        self
    }

    fn export_fixture_pair(&self, replay: &SexpNode) {
        self.export_named_fixture_pair(self.path.file_stem().unwrap(), replay);
    }

    fn export_named_fixture_pair(&self, pair: &std::ffi::OsStr, replay: &SexpNode) {
        let Some(root) = std::env::var_os("BLOCK_PLACEMENT_DUMP_DIR") else {
            return;
        };
        let root = std::path::PathBuf::from(root);
        for role in ["before", "after"] {
            let dir = root.join(pair).join(role);
            std::fs::create_dir_all(&dir).unwrap();
            let path = dir.join(self.path.file_name().unwrap());
            std::fs::copy(
                self.path.with_extension("kicad_pro"),
                path.with_extension("kicad_pro"),
            )
            .unwrap();
            if role == "before" {
                std::fs::copy(&self.path, path).unwrap();
            } else {
                std::fs::write(path, candidate::to_source(replay)).unwrap();
            }
        }
    }

    fn opamp(&self) -> Value {
        json!({
            "schematic": self.path,
            "anchor": self.id("U1"),
            "members": [self.id("R1"), self.id("R2"), self.id("R3"), self.id("C1")],
            "assignments": [{"member": self.id("C1"), "anchor_pin": "5"}],
            "connectors": [
                {"connector": self.id("J1"), "connector_pin": "1", "port": self.id("R1"), "port_pin": "1"},
                {"connector": self.id("J2"), "connector_pin": "1", "port": self.id("U1"), "port_pin": "1"}
            ]
        })
    }
}

#[tokio::test]
async fn missing_placed_targets_return_stale_target() {
    let sheet = Sheet::fixture("opamp_stage");
    let absent = "00000000-0000-0000-0000-000000000000";
    assert!(!std::fs::read_to_string(&sheet.path)
        .unwrap()
        .contains(absent));
    for (field, value) in [
        ("anchor", json!({"uuid": absent, "unit": 1})),
        ("fixed_uuids", json!([absent])),
        ("note_uuids", json!([absent])),
    ] {
        let mut args = sheet.opamp();
        args[field] = value;
        let error = sheet.refuses(args, "stale_target").await;
        assert_eq!(error["target"], absent);
        assert!(error["reason"].as_str().unwrap().contains(field));
    }
    let mut args = sheet.opamp();
    args["assignments"][0]["member"]["uuid"] = json!(absent);
    let error = sheet.refuses(args, "stale_target").await;
    assert_eq!(error["target"], absent);
    assert!(error["reason"]
        .as_str()
        .unwrap()
        .contains("assignments[0].member.uuid"));
}

#[tokio::test]
async fn duplicate_placed_uuid_returns_ambiguous_target() {
    let sheet = Sheet::fixture("opamp_stage");
    let args = sheet.opamp();
    let anchor = args["anchor"]["uuid"].as_str().unwrap();
    let mut tree = sheet.tree.clone();
    let duplicate = tree
        .find_all("symbol")
        .into_iter()
        .find(|node| node.find_str("uuid") == Some(anchor))
        .unwrap()
        .clone();
    let SexpNode::List(items) = &mut tree else {
        unreachable!()
    };
    items.push(duplicate);
    let sheet = sheet.with_tree(tree);
    let error = sheet.refuses(args.clone(), "ambiguous_target").await;
    assert_eq!(error["target"], format!("schematic UUID {anchor}"));
    assert_eq!(error["candidates"], json!(["symbol", "symbol"]));
}

#[tokio::test]
async fn invalid_selection_reports_the_exact_argument_path() {
    let sheet = Sheet::fixture("opamp_stage");
    for (path, field) in [
        ("/anchor/unit", "anchor.unit"),
        ("/members/0/unit", "members[0].unit"),
        ("/assignments/0/member/unit", "assignments[0].member.unit"),
        (
            "/connectors/0/connector/unit",
            "connectors[0].connector.unit",
        ),
        ("/connectors/0/port/unit", "connectors[0].port.unit"),
        ("/assignments/0/anchor_pin", "assignments[0].anchor_pin"),
        ("/connectors/0/connector_pin", "connectors[0].connector_pin"),
        ("/connectors/0/port_pin", "connectors[0].port_pin"),
    ] {
        let mut args = sheet.opamp();
        *args.pointer_mut(path).unwrap() = if path.ends_with("/unit") {
            json!(999)
        } else {
            json!("no_such_pin")
        };
        let error = sheet.refuses(args, "invalid_argument").await;
        assert_eq!(error["field"], field);
    }
}

#[tokio::test]
async fn shared_rail_is_not_membership_evidence() {
    let sheet = Sheet::fixture("shared_rail");
    let report = sheet
        .serve(json!({
            "schematic": sheet.path, "anchor": sheet.id("U1"), "members": []
        }))
        .await;
    assert_eq!(report["status"], "unchanged");
    assert!(report["plan"].is_null());
    for reference in ["C1", "C2", "R1"] {
        let member = report["suggestions"]
            .as_array()
            .unwrap()
            .iter()
            .find(|v| v["reference"] == reference)
            .unwrap();
        assert_eq!(member["suggested"], false, "{report}");
        assert!(
            member["competing_pins"]
                .as_array()
                .unwrap()
                .iter()
                .any(|p| p["reference"] == "U2"),
            "{report}"
        );
    }
}

#[tokio::test]
async fn opamp_preview_is_an_improvement_without_writing() {
    let sheet = Sheet::fixture("opamp_stage");
    let args = sheet.opamp();
    let report = sheet.serve(args.clone()).await;
    assert_eq!(report["status"], "improved", "{report}");
    assert_eq!(report["comparison"]["verdict"], "improved");
    assert_eq!(
        report["plan"]["removed_power_symbol_uuids"],
        json!([sheet.id("#PWR06")["uuid"]])
    );
    assert_eq!(sheet.serve(args).await, report);
    let replay = sheet.replay(&report["plan"]);
    let compare = |tree: &SexpNode, side, raw: f64, effective: &str| {
        let wires = extract_wires(tree);
        let excluded = &report["comparison"]["excluded_supply_stubs"][side];
        let segments = excluded["segments"].as_array().unwrap();
        assert!(!segments.is_empty());
        let total: f64 = segments
            .iter()
            .map(|segment| {
                let segment: [inventory::Point; 2] =
                    serde_json::from_value(segment.clone()).unwrap();
                assert!(wires.iter().any(|wire| endpoints(wire) == segment));
                length(segment)
            })
            .sum();
        assert!((excluded["length_mm"].as_f64().unwrap() - total).abs() < 1e-6);
        let measured: f64 = wires.iter().map(|wire| length(endpoints(wire))).sum();
        let ranked = report["comparison"][effective]["wire_length_mm"]
            .as_f64()
            .unwrap();
        assert!((measured - raw).abs() < 1e-6);
        assert!((ranked + total - measured).abs() < 1e-6);
    };
    compare(&sheet.tree, "baseline", 497.84, "baseline_score");
    compare(&replay, "candidate", 160.02, "score");
    let observed = Inventory::new(&replay).unwrap();
    let bounds = |reference| {
        let part = observed
            .parts
            .values()
            .find(|p| p.inst.reference == reference)
            .unwrap();
        symbol_geometry_for_instance(part.lib, &part.inst)
            .unwrap()
            .envelope()
            .unwrap()
    };
    let member_bounds = ["U1", "R1", "R2", "R3", "C1"].map(bounds);
    let left = member_bounds
        .iter()
        .map(|b| b.min_x)
        .reduce(f64::min)
        .unwrap();
    let right = member_bounds
        .iter()
        .map(|b| b.max_x)
        .reduce(f64::max)
        .unwrap();
    for gap in [left - bounds("J1").max_x, bounds("J2").min_x - right] {
        assert!((gap - 7.62).abs() <= 0.635 + 1e-6, "connector gap: {gap}");
    }
    sheet.export_fixture_pair(&replay);
    let replayed = sheet.with_tree(replay);
    let next = replayed.serve(replayed.opamp()).await;
    assert_eq!(next["status"], "unchanged", "{next}");
    assert!(next["plan"].is_null(), "{next}");
}

#[tokio::test]
async fn a_short_wire_outside_the_block_refuses_the_preview() {
    let sheet = Sheet::fixture("opamp_stage");
    let mut tree = sheet.tree.clone();
    let SexpNode::List(items) = &mut tree else {
        unreachable!()
    };
    items.push(parse_sexp(&format_wire(230.0, 180.0, 231.27, 180.0)).unwrap());
    let sheet = sheet.with_tree(tree);
    let report = sheet.serve(sheet.opamp()).await;
    assert_eq!(report["status"], "blocked", "{report}");
    assert!(report["plan"].is_null());
    assert!(
        report["blockers"]
            .to_string()
            .contains("shorter than 2.54 mm"),
        "{report}"
    );
}

#[tokio::test]
async fn aliased_rail_names_do_not_make_a_private_supply_removable() {
    let sheet = Sheet::fixture("opamp_stage");
    let args = sheet.opamp();
    let supply = sheet.id("#PWR06")["uuid"].as_str().unwrap().to_owned();
    let mut tree = sheet.tree.clone();
    let SexpNode::List(items) = &mut tree else {
        unreachable!()
    };
    let symbol = items
        .iter_mut()
        .find(|node| node.find_str("uuid") == Some(&supply))
        .unwrap();
    let SexpNode::List(children) = symbol else {
        unreachable!()
    };
    let property = children
        .iter_mut()
        .find(|node| {
            node.head() == Some("property")
                && node.get(1).and_then(SexpNode::as_str) == Some("Value")
        })
        .unwrap();
    let SexpNode::List(property) = property else {
        unreachable!()
    };
    property[2] = SexpNode::Str("ALIAS_5V".into());
    // The named graph now sees the same root. The two private drawn islands
    // still carry different supply values.
    items.push(parse_sexp(&format_wire(230.0, 180.0, 235.08, 180.0)).unwrap());
    for (name, x) in [("+5V", 230.0), ("ALIAS_5V", 235.08)] {
        items.push(parse_sexp(&format!(
            r#"(label "{name}" (at {x} 180 0) (effects (font (size 1.27 1.27)) (justify left bottom)))"#,
        )).unwrap());
    }
    let sheet = sheet.with_tree(tree);
    let report = sheet.serve(args).await;
    assert_eq!(report["status"], "blocked", "{report}");
    assert!(report["plan"].is_null());
    assert!(
        report["blockers"]
            .to_string()
            .contains("not a removable duplicate"),
        "{report}"
    );
}

#[tokio::test]
async fn a_power_flag_on_a_private_lead_is_not_a_removable_supply() {
    let sheet = Sheet::fixture("opamp_stage");
    let args = sheet.opamp();
    let supply = sheet.id("#PWR06")["uuid"].as_str().unwrap().to_owned();
    let inventory = Inventory::new(&sheet.tree).unwrap();
    let tip = inventory.parts[supply.as_str()].pins[0].tip;
    let wire = inventory
        .wires
        .iter()
        .map(endpoints)
        .find(|ends| ends.contains(&tip))
        .unwrap();
    let (x, y) = ((wire[0].0 + wire[1].0) / 2.0, (wire[0].1 + wire[1].1) / 2.0);
    // Name this island through the legacy graph without adding a physically
    // attached label, so only the electrical type disqualifies the private lead.
    let (x, y) = if wire[0].0 == wire[1].0 {
        (x + 0.001, y)
    } else {
        (x, y + 0.001)
    };
    let mut tree = sheet.tree.clone();
    let SexpNode::List(items) = &mut tree else {
        unreachable!()
    };
    let symbol = items
        .iter_mut()
        .find(|node| node.find_str("uuid") == Some(&supply))
        .unwrap();
    let SexpNode::List(children) = symbol else {
        unreachable!()
    };
    let lib_id = children
        .iter_mut()
        .find(|node| node.head() == Some("lib_id"))
        .unwrap();
    *lib_id = parse_sexp(r#"(lib_id "power:PWR_FLAG")"#).unwrap();
    items.push(parse_sexp(&format!(
        r#"(label "+5V" (at {x} {y} 0) (effects (font (size 1.27 1.27)) (justify left bottom)))"#,
    )).unwrap());
    let sheet = sheet.with_tree(tree);
    let report = sheet.serve(args).await;
    assert_eq!(report["status"], "blocked", "{report}");
    assert!(report["plan"].is_null());
    assert!(
        report["blockers"]
            .to_string()
            .contains("member supply does not have a private lead"),
        "{report}"
    );
}

#[tokio::test]
async fn a_fixed_wire_in_a_redrawn_island_cannot_be_replaced() {
    let (sheet, mut args) = with_output_connector_detour().await;
    args["members"] = json!([]);
    args["assignments"] = json!([]);
    let plan = sheet.serve(args.clone()).await;
    assert_eq!(plan["status"], "improved", "{plan}");
    let inventory = Inventory::new(&sheet.tree).unwrap();
    let request = Request::parse(&args).unwrap();
    let pairing = &request.connectors[0];
    let tip = inventory
        .symbol(pairing.connector)
        .unwrap()
        .pin(pairing.connector_pin)
        .unwrap()
        .tip;
    let wire = inventory
        .wires
        .iter()
        .find(|wire| endpoints(wire).contains(&tip))
        .unwrap();
    let id = wire.uuid.as_deref().unwrap();
    assert!(plan["plan"]["remove_wire_uuids"]
        .as_array()
        .unwrap()
        .contains(&json!(id)));
    args["fixed_uuids"] = json!([id]);
    let report = sheet.serve(args).await;
    assert_eq!(report["status"], "blocked", "{report}");
    assert!(report["plan"].is_null());
    assert!(
        report["blockers"]
            .to_string()
            .contains(&format!("candidate would change fixed item {id}")),
        "{report}"
    );
}

#[tokio::test]
async fn locked_output_connector_refuses_an_overlong_run() {
    let sheet = Sheet::fixture("opamp_stage");
    let mut args = sheet.opamp();
    args["fixed_uuids"] = json!([sheet.id("J2")["uuid"]]);
    let report = sheet.serve(args).await;
    assert_eq!(report["status"], "blocked", "{report}");
    assert!(report["plan"].is_null());
    assert!(
        report["blockers"]
            .to_string()
            .contains("fixed connector J2"),
        "{report}"
    );
}

#[tokio::test]
async fn named_rail_alone_is_not_a_drawn_connector_run() {
    let sheet = Sheet::fixture("ldo_enable");
    let report = sheet.serve(json!({
        "schematic": sheet.path,
        "anchor": sheet.id("U1"),
        "members": [],
        "connectors": [
            {"connector": sheet.id("J1"), "connector_pin": "1", "port": sheet.id("U1"), "port_pin": "1"}
        ]
    })).await;
    assert_eq!(report["status"], "blocked", "{report}");
    assert!(report["plan"].is_null());
    assert!(
        report["blockers"].to_string().contains("no drawn run"),
        "{report}"
    );
}

#[tokio::test]
async fn a_connector_on_the_correct_side_must_also_face_its_port() {
    let sheet = Sheet::fixture("opamp_stage");
    let inventory = Inventory::new(&sheet.tree).unwrap();
    let j2 = inventory
        .parts
        .values()
        .find(|p| p.inst.reference == "J2")
        .unwrap();
    let pin = j2.pin("1").unwrap();
    let reversed = Pose::with_tip(
        j2,
        pin,
        pin.tip,
        (j2.inst.rotation + 180.0).rem_euclid(360.0),
    );
    let mut tree = sheet.tree.clone();
    let SexpNode::List(items) = &mut tree else {
        unreachable!()
    };
    let placed = items
        .iter_mut()
        .find(|n| n.find_str("uuid") == Some(j2.id.uuid))
        .unwrap();
    *placed = reversed.relocate(j2).unwrap();
    let sheet = sheet.with_tree(tree);
    let report = sheet.serve(json!({
        "schematic": sheet.path,
        "anchor": sheet.id("U1"),
        "members": [],
        "connectors": [
            {"connector": sheet.id("J2"), "connector_pin": "1", "port": sheet.id("U1"), "port_pin": "1"}
        ]
    })).await;
    assert_eq!(report["status"], "blocked", "{report}");
    assert!(report["plan"].is_null());
    assert!(
        report["blockers"].to_string().contains("J2 does not face"),
        "{report}"
    );
}

#[tokio::test]
async fn locked_connector_refuses_a_long_drawn_detour_between_close_tips() {
    let (sheet, mut args) = with_output_connector_detour().await;
    args["members"] = json!([]);
    args["assignments"] = json!([]);
    args["fixed_uuids"] = json!([sheet.id("J2")["uuid"]]);
    let report = sheet.serve(args).await;
    assert_eq!(report["status"], "blocked", "{report}");
    assert!(report["plan"].is_null());
    assert!(
        report["blockers"].to_string().contains("drawn run"),
        "{report}"
    );
}

async fn with_output_connector_detour() -> (Sheet, Value) {
    let sheet = Sheet::fixture("opamp_stage");
    let mut args = sheet.opamp();
    let report = sheet.serve(args.clone()).await;
    assert_eq!(report["status"], "improved", "{report}");
    let mut tree = sheet.replay(&report["plan"]);
    let inventory = Inventory::new(&tree).unwrap();
    let reference = |name| {
        inventory
            .parts
            .values()
            .find(|p| p.inst.reference == name)
            .unwrap()
    };
    let start = reference("J2").pin("1").unwrap().tip;
    let end = reference("U1").pin("1").unwrap().tip;
    let wire = inventory
        .wires
        .iter()
        .find(|w| endpoints(w).contains(&start) && endpoints(w).contains(&end))
        .unwrap()
        .uuid
        .clone()
        .unwrap();
    assert!(span(start, end) < MAX_CONNECTOR_RUN);
    let SexpNode::List(items) = &mut tree else {
        unreachable!()
    };
    items.retain(|item| item.find_str("uuid") != Some(&wire));
    let y = start.1 + 12.7;
    for [(x1, y1), (x2, y2)] in [
        [start, (start.0, y)],
        [(start.0, y), (end.0, y)],
        [(end.0, y), end],
    ] {
        items.push(parse_sexp(&format_wire(x1, y1, x2, y2)).unwrap());
    }
    args["connectors"] = json!([args["connectors"][1]]);
    (sheet.with_tree(tree), args)
}

#[tokio::test]
async fn an_unlocked_connector_at_its_target_can_get_a_wire_only_repair() {
    let (sheet, mut args) = with_output_connector_detour().await;
    args["fixed_uuids"] = json!(["R1", "R2", "R3", "C1"].map(|r| sheet.id(r)["uuid"].clone()));
    let report = sheet.serve(args).await;
    assert_eq!(report["status"], "improved", "{report}");
    assert_eq!(report["plan"]["poses"], json!([]), "{report}");
    assert!(!report["plan"]["add_wires"].as_array().unwrap().is_empty());
    let replay = sheet.replay(&report["plan"]);
    let args = sheet.opamp();
    check_connector_runs(&replay, &Request::parse(&args).unwrap()).unwrap();
    let inventory = Inventory::new(&sheet.tree).unwrap();
    let connector = inventory
        .symbol(Request::parse(&args).unwrap().connectors[1].connector)
        .unwrap();
    let labels = extract_labels(&sheet.tree);
    let topology = topology::Topology::new(&sheet.tree, &inventory, &labels);
    let ground = &topology.islands[&connector.pin("2").unwrap().drawn];
    for uuid in ground
        .wires
        .iter()
        .map(|w| w.uuid.as_deref().unwrap())
        .chain(
            sheet
                .tree
                .find_all("junction")
                .into_iter()
                .filter_map(|node| {
                    let (x, y, _) = copperpour_sexp::schematic::parse_at(node)?;
                    if ground.junctions.contains(&(x, y)) {
                        node.find_str("uuid")
                    } else {
                        None
                    }
                }),
        )
    {
        let object = |tree: &SexpNode| {
            tree.children()
                .into_iter()
                .flatten()
                .find(|node| node.find_str("uuid") == Some(uuid))
                .cloned()
        };
        assert_eq!(object(&sheet.tree), object(&replay), "{uuid}");
    }
}

#[tokio::test]
async fn wire_only_repair_does_not_join_a_separate_same_name_stub() {
    for offset in [0.0, 0.00004] {
        check_wire_only_repair_does_not_join_a_separate_same_name_stub(offset).await;
    }
}

async fn check_wire_only_repair_does_not_join_a_separate_same_name_stub(offset: f64) {
    let (sheet, mut args) = with_output_connector_detour().await;
    args["fixed_uuids"] = json!(["R1", "R2", "R3", "C1"].map(|r| sheet.id(r)["uuid"].clone()));
    let inventory = Inventory::new(&sheet.tree).unwrap();
    let request = Request::parse(&args).unwrap();
    let pairing = &request.connectors[0];
    let pin = inventory
        .symbol(pairing.connector)
        .unwrap()
        .pin(pairing.connector_pin)
        .unwrap();
    let port = inventory
        .symbol(pairing.port)
        .unwrap()
        .pin(pairing.port_pin)
        .unwrap()
        .tip;
    let x = copperpour_sexp::geometry::round6((pin.tip.0 + port.0) / 2.0);
    let (_, y) = pin.tip;
    let tip = (x, y - 5.08);
    let mut tree = sheet.tree.clone();
    let SexpNode::List(items) = &mut tree else {
        unreachable!()
    };
    items.push(parse_sexp(&format_wire(x, y + offset, tip.0, tip.1)).unwrap());
    for (x, y) in [pin.tip, tip] {
        items.push(parse_sexp(&format!(
            r#"(label "SIG" (at {x} {y} 90) (effects (font (size 1.27 1.27)) (justify left bottom)))"#,
        )).unwrap());
    }
    let sheet = sheet.with_tree(tree);
    let inventory = Inventory::new(&sheet.tree).unwrap();
    let labels = extract_labels(&sheet.tree);
    let mut drawn =
        crate::tools::sch_connectivity::redraw_net_graph(&sheet.tree, &inventory.wires, &labels);
    let mut named =
        crate::tools::sch_connectivity::snapshot_net_graph(&sheet.tree, &inventory.wires);
    assert_ne!(drawn.root_at(x, y), drawn.root_at(port.0, port.1));
    assert_eq!(named.root_at(x, y), named.root_at(port.0, port.1));
    let report = sheet.serve(args).await;
    assert_eq!(report["status"], "blocked", "{report}");
    assert!(report["plan"].is_null());
    assert!(
        report["blockers"]
            .to_string()
            .contains("join separate drawn islands"),
        "{report}"
    );
}

#[tokio::test]
async fn wire_only_repair_does_not_absorb_a_near_miss_label() {
    let (sheet, mut args) = with_output_connector_detour().await;
    args["fixed_uuids"] = json!(["R1", "R2", "R3", "C1"].map(|r| sheet.id(r)["uuid"].clone()));
    let inventory = Inventory::new(&sheet.tree).unwrap();
    let request = Request::parse(&args).unwrap();
    let pairing = &request.connectors[0];
    let (x, y) = inventory
        .symbol(pairing.connector)
        .unwrap()
        .pin(pairing.connector_pin)
        .unwrap()
        .tip;
    let near = (x + 0.001, y + 5.08);
    let far = (x + 10.16, near.1);
    let stub = parse_sexp(&format_wire(near.0, near.1, far.0, far.1)).unwrap();
    let stub_id = stub.find_str("uuid").unwrap().to_owned();
    let mut tree = sheet.tree.clone();
    let SexpNode::List(items) = &mut tree else {
        unreachable!()
    };
    items.push(stub.clone());
    for (x, y) in [near, far] {
        items.push(parse_sexp(&format!(
            r#"(label "NEAR_MISS" (at {x} {y} 90) (effects (font (size 1.27 1.27)) (justify left bottom)))"#,
        )).unwrap());
    }
    let sheet = sheet.with_tree(tree);
    let report = sheet.serve(args).await;
    if report["status"] == "blocked" {
        assert!(report["plan"].is_null());
        return;
    }
    assert_eq!(report["status"], "improved", "{report}");
    let replay = sheet.replay(&report["plan"]);
    assert_eq!(
        replay
            .find_all("wire")
            .into_iter()
            .find(|node| node.find_str("uuid") == Some(&stub_id)),
        Some(&stub)
    );
    let labels = extract_labels(&replay);
    let wires = extract_wires(&replay);
    let mut drawn = crate::tools::sch_connectivity::redraw_net_graph(&replay, &wires, &labels);
    assert_ne!(drawn.root_at(x, y), drawn.root_at(near.0, near.1));
}

#[tokio::test]
async fn an_aligned_series_part_with_a_detour_is_not_already_placed() {
    let sheet = Sheet::fixture("opamp_stage");
    let report = sheet.serve(sheet.opamp()).await;
    assert_eq!(report["status"], "improved", "{report}");
    let mut tree = sheet.replay(&report["plan"]);
    let inventory = Inventory::new(&tree).unwrap();
    let part = |name| {
        inventory
            .parts
            .values()
            .find(|p| p.inst.reference == name)
            .unwrap()
    };
    let a = part("U1").pin("3").unwrap().tip;
    let b = part("R1").pin("2").unwrap().tip;
    let uuid = inventory
        .wires
        .iter()
        .find(|w| endpoints(w).contains(&a) && endpoints(w).contains(&b))
        .unwrap()
        .uuid
        .clone()
        .unwrap();
    let SexpNode::List(items) = &mut tree else {
        unreachable!()
    };
    items.retain(|n| n.find_str("uuid") != Some(&uuid));
    let y = a.1 - 5.08;
    for [(x1, y1), (x2, y2)] in [[a, (a.0, y)], [(a.0, y), (b.0, y)], [(b.0, y), b]] {
        items.push(parse_sexp(&format_wire(x1, y1, x2, y2)).unwrap());
    }
    let sheet = sheet.with_tree(tree);
    let report = sheet
        .serve(json!({
            "schematic": sheet.path, "anchor": sheet.id("U1"), "members": [sheet.id("R1")]
        }))
        .await;
    assert_eq!(report["status"], "improved", "{report}");
    assert!(
        report["plan"]["add_wires"]
            .as_array()
            .unwrap()
            .iter()
            .any(|wire| {
                let [start, end]: [inventory::Point; 2] =
                    serde_json::from_value(wire.clone()).unwrap();
                start.1 == a.1 && end.1 == a.1 && [start, end].contains(&a)
            }),
        "{report}"
    );
}

#[tokio::test]
async fn an_explicit_shared_rail_assignment_can_be_a_wire_adding_tradeoff() {
    let sheet = Sheet::fixture("shared_rail");
    let report = sheet
        .serve(json!({
            "schematic": sheet.path, "anchor": sheet.id("U1"), "members": [sheet.id("C1")],
            "assignments": [{ "member": sheet.id("C1"), "anchor_pin": "5" }]
        }))
        .await;
    assert_eq!(report["status"], "grouping_adds_wire", "{report}");
    assert!(
        report["comparison"]["delta"]["wire_length_mm"]
            .as_f64()
            .unwrap()
            > 0.0
    );
    assert!(report["plan"]["poses"]
        .as_array()
        .unwrap()
        .iter()
        .all(|p| p["uuid"] != sheet.id("C2")["uuid"]));
    assert_eq!(
        report["plan"]["removed_power_symbol_uuids"],
        json!([sheet.id("#PWR07")["uuid"]])
    );
    let replay = sheet.replay(&report["plan"]);
    sheet.export_fixture_pair(&replay);
}

#[tokio::test]
async fn a_clearer_wire_adding_group_is_not_a_cleanup() {
    let sheet = Sheet::fixture("shared_rail");
    let mut source = std::fs::read_to_string(&sheet.path).unwrap();
    let original = "(property \"Reference\" \"C1\"\n\t\t\t(at 145.415 128.27 0)";
    assert_eq!(source.matches(original).count(), 1);
    // Put only C1's Reference across its old ground lead. Moving C1 clears
    // the crossing, so the scorer improves even though the grouping adds wire.
    source = source.replacen(
        original,
        "(property \"Reference\" \"C1\"\n\t\t\t(at 143.5 135.89 0)",
        1,
    );
    let sheet = sheet.with_tree(parse_sexp(&source).unwrap());
    let report = sheet
        .serve(json!({
            "schematic": sheet.path, "anchor": sheet.id("U1"), "members": [sheet.id("C1")],
            "assignments": [{ "member": sheet.id("C1"), "anchor_pin": "5" }]
        }))
        .await;
    assert_eq!(report["comparison"]["verdict"], "improved", "{report}");
    assert_eq!(report["comparison"]["delta"]["wire_field_overlap"], -1);
    assert!(
        report["comparison"]["delta"]["wire_length_mm"]
            .as_f64()
            .unwrap()
            > 0.0
    );
    assert_eq!(report["status"], "grouping_adds_wire", "{report}");
    assert!(!report["plan"].is_null());
}

#[tokio::test]
async fn constrained_shared_rail_assignment_does_not_move_the_other_capacitor() {
    let sheet = Sheet::fixture("shared_rail");
    let report = sheet
        .serve(json!({
            "schematic": sheet.path, "anchor": sheet.id("U2"), "members": [sheet.id("C2")],
            "assignments": [{ "member": sheet.id("C2"), "anchor_pin": "5" }]
        }))
        .await;
    assert_eq!(report["status"], "blocked", "{report}");
    assert!(report["plan"].is_null());
}

fn ldo_request(sheet: &Sheet) -> Value {
    json!({
        "schematic": sheet.path,
        "anchor": sheet.id("U1"),
        "members": [sheet.id("R1"), sheet.id("C1"), sheet.id("C2")],
        "assignments": [
            {"member": sheet.id("C1"), "anchor_pin": "1"},
            {"member": sheet.id("C2"), "anchor_pin": "5"}
        ],
        "connectors": [
            {"connector": sheet.id("J2"), "connector_pin": "1", "port": sheet.id("U1"), "port_pin": "5"}
        ]
    })
}

#[test]
fn length_walk_turns_only_at_an_explicit_junction_on_an_unsplit_span() {
    let wires = [
        Wire {
            x1: 0.0,
            y1: 0.0,
            x2: 12.7,
            y2: 0.0,
            uuid: None,
        },
        Wire {
            x1: 7.62,
            y1: 0.0,
            x2: 7.62,
            y2: 5.08,
            uuid: None,
        },
    ];
    let mut island = topology::Island {
        wires: wires.iter().collect(),
        ..Default::default()
    };
    assert!(!island.reaches_within((0.0, 0.0), (7.62, 5.08), MAX_CONNECTOR_RUN));
    island.junctions.push((7.62, 0.0));
    assert!(island.reaches_within((0.0, 0.0), (7.62, 5.08), MAX_CONNECTOR_RUN));
    assert!(!island.reaches_within((0.0, 0.0), (7.62, 5.08), 10.16));
}

#[test]
fn length_walk_follows_a_physical_label_tap_but_not_matching_label_names() {
    let tree = parse_sexp(
        r#"(kicad_sch
            (label "SIG" (at 7.62 0 0))
            (label "SIG" (at 20.32 0 0)))"#,
    )
    .unwrap();
    let labels = extract_labels(&tree);
    assert_eq!(labels.len(), 2);
    let wires = [
        (0.0, 0.0, 12.7, 0.0),
        (7.62, 0.0, 7.62, 5.08),
        (20.32, 0.0, 20.32, 5.08),
    ]
    .map(|(x1, y1, x2, y2)| Wire {
        x1,
        y1,
        x2,
        y2,
        uuid: None,
    });
    let island = topology::Island {
        wires: wires.iter().collect(),
        terminals: labels.iter().map(topology::Terminal::Label).collect(),
        ..Default::default()
    };
    assert!(island.reaches_within((0.0, 0.0), (7.62, 5.08), MAX_CONNECTOR_RUN));
    assert!(!island.reaches_within((0.0, 0.0), (20.32, 5.08), f64::INFINITY));
}

#[tokio::test]
async fn a_label_off_the_wire_does_not_prove_a_drawn_run() {
    let sheet = Sheet::fixture("opamp_stage");
    let report = sheet.serve(sheet.opamp()).await;
    assert_eq!(report["status"], "improved", "{report}");
    let clean = sheet.replay(&report["plan"]);
    let mut probes = Vec::new();
    for (offset, should_reach) in [(0.0, true), (0.001, false), (0.00004, true)] {
        let mut tree = clean.clone();
        let SexpNode::List(items) = &mut tree else {
            unreachable!()
        };
        let y = 88.9 + offset;
        items.push(parse_sexp(&format_wire(138.43, y, 138.43, 83.82)).unwrap());
        items.push(
            parse_sexp(&format!(
                r#"(label "TOUCH_PROBE" (at 138.43 {y} 90)
            (effects (font (size 1.27 1.27)) (justify left bottom)))"#
            ))
            .unwrap(),
        );
        let inventory = Inventory::new(&tree).unwrap();
        let labels = extract_labels(&tree);
        let mut queried =
            crate::tools::sch_connectivity::drawn_net_graph_for(&tree, &inventory.wires, &labels);
        assert_eq!(
            queried.root_at(142.24, 88.9),
            queried.root_at(138.43, 83.82),
            "the shared graph tolerates the offset"
        );
        let mut drawn =
            crate::tools::sch_connectivity::redraw_net_graph(&tree, &inventory.wires, &labels);
        let root = drawn.root_at(142.24, 88.9);
        assert_eq!(root == drawn.root_at(138.43, 83.82), should_reach);
        let topology = topology::Topology::new(&tree, &inventory, &labels);
        assert_eq!(
            topology.islands[&root].reaches_within(
                (142.24, 88.9),
                (138.43, 83.82),
                MAX_CONNECTOR_RUN
            ),
            should_reach
        );
        probes.push(tree);
    }
    let sheet = sheet.with_tree(probes.remove(0));
    sheet.export_named_fixture_pair(std::ffi::OsStr::new("label_tap_probe"), &probes[0]);
    sheet.export_named_fixture_pair(std::ffi::OsStr::new("rounded_label_tap_probe"), &probes[1]);
}

#[tokio::test]
async fn ldo_preview_groups_separate_local_supply_islands() {
    let sheet = Sheet::fixture("ldo_enable");
    let args = ldo_request(&sheet);
    let report = sheet.serve(args.clone()).await;
    assert_eq!(report["status"], "improved", "{report}");
    assert_eq!(
        report["plan"]["removed_power_symbol_uuids"]
            .as_array()
            .unwrap()
            .len(),
        4
    );
    assert!(report["plan"]["field_moves"]
        .as_array()
        .unwrap()
        .iter()
        .any(|p| p["uuid"] == sheet.id("U1")["uuid"]));
    assert_eq!(report["plan"]["no_connect_moves"], json!([]));
    assert!(report["plan"]["poses"]
        .as_array()
        .unwrap()
        .iter()
        .all(|p| p["uuid"] != sheet.id("U1")["uuid"] && p["uuid"] != sheet.id("J1")["uuid"]));
    let replay = sheet.replay(&report["plan"]);
    sheet.export_fixture_pair(&replay);
    let replayed = sheet.with_tree(replay);
    let next = replayed.serve(args).await;
    assert_eq!(next["status"], "unchanged", "{next}");
    assert!(next["plan"].is_null(), "{next}");
}

#[tokio::test]
async fn an_explicitly_fixed_anchor_keeps_its_fields() {
    let sheet = Sheet::fixture("ldo_enable");
    let mut args = ldo_request(&sheet);
    let anchor = args["anchor"]["uuid"].clone();
    args["fixed_uuids"] = json!([anchor]);
    let report = sheet.serve(args).await;
    assert_eq!(report["status"], "improved", "{report}");
    assert!(report["plan"]["field_moves"]
        .as_array()
        .unwrap()
        .iter()
        .all(|p| p["uuid"] != anchor));
}

#[tokio::test]
async fn placement_preview_preserves_the_other_stage() {
    let first = ["U1", "R1", "R2", "R3", "C1"];
    let second = ["U2", "R4", "R5", "R6", "C2"];
    for (stage, fixed) in [(first, second), (second, first)] {
        let sheet = Sheet::fixture("two_blocks");
        let [anchor, r1, r2, r3, bypass] = stage.map(|name| sheet.id(name));
        let locked = fixed.map(|name| sheet.id(name)["uuid"].clone());
        let request = json!({
            "schematic": sheet.path,
            "anchor": anchor,
            "members": [r1, r2, r3, bypass],
            "assignments": [{ "member": bypass, "anchor_pin": "5" }],
            "fixed_uuids": locked
        });
        let report = sheet.serve(request.clone()).await;
        assert!(
            matches!(
                report["status"].as_str(),
                Some("improved" | "grouping_adds_wire")
            ),
            "{report}"
        );
        let replay = sheet.replay(&report["plan"]);
        sheet.export_named_fixture_pair(
            std::ffi::OsStr::new(&format!("two_blocks_{}", stage[0])),
            &replay,
        );
        let replayed = sheet.with_tree(replay);
        let next = replayed.serve(request).await;
        assert_eq!(next["status"], "unchanged", "{next}");
        assert!(next["plan"].is_null());
    }
}

fn feedback_with_extra_resistor(in_series: bool) -> Sheet {
    let sheet = Sheet::fixture("opamp_stage");
    let inventory = Inventory::new(&sheet.tree).unwrap();
    let part = |reference| {
        inventory
            .parts
            .values()
            .find(|p| p.inst.reference == reference)
            .unwrap()
    };
    let resistor = part("R2");
    let anchor = part("U1");
    let pose = Pose {
        x: 127.0,
        y: 111.76,
        rotation: 90.0,
    };
    let mut extra = pose.relocate(resistor).unwrap();
    let SexpNode::List(fields) = &mut extra else {
        unreachable!()
    };
    fields.retain(|field| field.head() != Some("uuid"));
    fields.push(parse_sexp(r#"(uuid "77600000-0000-4000-8000-000000000005")"#).unwrap());
    let reference = fields
        .iter_mut()
        .find(|field| field.get(1).and_then(SexpNode::as_str) == Some("Reference"))
        .unwrap();
    let SexpNode::List(atoms) = reference else {
        unreachable!()
    };
    atoms[2] = SexpNode::Str("R9".into());

    let mut tree = sheet.tree.clone();
    let SexpNode::List(items) = &mut tree else {
        unreachable!()
    };
    items.push(extra);
    let detached = if in_series {
        let out = anchor.pin("1").unwrap().net;
        let tip = resistor.pins.iter().find(|pin| pin.net == out).unwrap().tip;
        let wire = inventory
            .wires
            .iter()
            .find(|w| endpoints(w).contains(&tip))
            .unwrap();
        items.retain(|n| n.find_str("uuid") != wire.uuid.as_deref());
        Some(tip)
    } else {
        None
    };
    for (index, pin) in resistor.pins.iter().enumerate() {
        let start = pose.tip(resistor, pin);
        let end = if in_series && index == 0 {
            detached.unwrap()
        } else {
            let net = if in_series {
                anchor.pin("1").unwrap().net
            } else {
                pin.net
            };
            anchor
                .pins
                .iter()
                .find(|other| other.net == net)
                .unwrap()
                .tip
        };
        let bend = (end.0, start.1);
        for [(x1, y1), (x2, y2)] in [[start, bend], [bend, end]] {
            items.push(parse_sexp(&format_wire(x1, y1, x2, y2)).unwrap());
        }
    }
    sheet.with_tree(tree)
}

#[tokio::test]
async fn a_parallel_feedback_leg_is_not_a_one_resistor_lane() {
    let sheet = feedback_with_extra_resistor(false);
    let report = sheet
        .serve(json!({
            "schematic": sheet.path,
            "anchor": sheet.id("U1"),
            "members": [sheet.id("R2")]
        }))
        .await;
    assert_eq!(report["status"], "blocked", "{report}");
    assert!(report["plan"].is_null());
    assert!(
        report["blockers"]
            .to_string()
            .contains("parallel or multi-part feedback"),
        "{report}"
    );
}

#[tokio::test]
async fn a_two_resistor_feedback_path_is_not_classified_as_series_members() {
    let sheet = feedback_with_extra_resistor(true);
    let report = sheet
        .serve(json!({
            "schematic": sheet.path,
            "anchor": sheet.id("U1"),
            "members": [sheet.id("R2"), sheet.id("R9")]
        }))
        .await;
    assert_eq!(report["status"], "blocked", "{report}");
    assert!(report["plan"].is_null());
    assert!(
        report["blockers"]
            .to_string()
            .contains("multi-part feedback path"),
        "{report}"
    );
}

#[tokio::test]
async fn matching_label_names_do_not_attach_a_feedback_or_shunt_leg() {
    for (reference, served_pin, expected) in [
        ("R2", "1", "feedback resistor has no drawn leg"),
        ("R3", "4", "shunt has no drawn leg"),
    ] {
        let sheet = Sheet::fixture("opamp_stage");
        let inventory = Inventory::new(&sheet.tree).unwrap();
        let anchor = inventory
            .parts
            .values()
            .find(|p| p.inst.reference == "U1")
            .unwrap();
        let member = inventory
            .parts
            .values()
            .find(|p| p.inst.reference == reference)
            .unwrap();
        let served = anchor.pin(served_pin).unwrap();
        let leg = member
            .pins
            .iter()
            .find(|pin| pin.net == served.net)
            .unwrap();
        let wire = inventory
            .wires
            .iter()
            .find(|w| endpoints(w).contains(&leg.tip))
            .unwrap();
        let mut tree = sheet.tree.clone();
        let SexpNode::List(items) = &mut tree else {
            unreachable!()
        };
        items.retain(|n| n.find_str("uuid") != wire.uuid.as_deref());
        for (x, y) in [leg.tip, served.tip] {
            items.push(
                parse_sexp(&format!(
                    r#"(label "SEPARATE" (at {x} {y} 0) (effects (font (size 1.27 1.27))))"#,
                ))
                .unwrap(),
            );
        }
        let sheet = sheet.with_tree(tree);
        let inventory = Inventory::new(&sheet.tree).unwrap();
        let anchor = inventory
            .parts
            .values()
            .find(|p| p.inst.reference == "U1")
            .unwrap();
        let member = inventory
            .parts
            .values()
            .find(|p| p.inst.reference == reference)
            .unwrap();
        let served = anchor.pin(served_pin).unwrap();
        let leg = member
            .pins
            .iter()
            .find(|pin| pin.net == served.net)
            .unwrap();
        assert_ne!(
            leg.drawn, served.drawn,
            "the matching labels must sit on separate drawn islands"
        );
        let report = sheet
            .serve(json!({
                "schematic": sheet.path,
                "anchor": sheet.id("U1"),
                "members": [sheet.id(reference)]
            }))
            .await;
        assert_eq!(report["status"], "blocked", "{report}");
        assert!(report["plan"].is_null());
        assert!(
            report["blockers"].to_string().contains(expected),
            "{report}"
        );
    }
}

#[test]
fn feedback_search_reserves_trials_for_both_sides_of_the_anchor() {
    let sheet = Sheet::fixture("opamp_stage");
    let args = json!({ "anchor": sheet.id("U1"), "members": [sheet.id("R2")] });
    let request = Request::parse(&args).unwrap();
    let inventory = Inventory::new(&sheet.tree).unwrap();
    let anchor = inventory.symbol(request.anchor).unwrap();
    let labels = extract_labels(&sheet.tree);
    let topology = topology::Topology::new(&sheet.tree, &inventory, &labels);
    let members = movable_parts(anchor, &inventory, &request).unwrap();
    let id = members[0].0.id.uuid;
    let below = (0..MAX_TRIALS / 2)
        .find_map(|trial| place(anchor, &inventory, &topology, &members, &request, trial).ok())
        .unwrap();
    let above = (MAX_TRIALS / 2..MAX_TRIALS)
        .find_map(|trial| place(anchor, &inventory, &topology, &members, &request, trial).ok())
        .unwrap();
    assert!(below.poses[id].y > anchor.inst.y);
    assert!(above.poses[id].y < anchor.inst.y);
}

#[tokio::test]
async fn feedback_can_use_the_other_side_when_a_fixed_note_blocks_its_lane() {
    let sheet = Sheet::fixture("opamp_stage");
    let report = sheet.serve(sheet.opamp()).await;
    assert_eq!(report["status"], "improved", "{report}");
    let mut tree = sheet.replay(&report["plan"]);
    // Give the original lower lane a detour so a clear upper lane can
    // improve the score as well as clear the fixed note.
    let inventory = Inventory::new(&tree).unwrap();
    let resistor = inventory
        .parts
        .values()
        .find(|p| p.inst.reference == "R2")
        .unwrap();
    let output = resistor.pin("2").unwrap().tip;
    let wire = inventory
        .wires
        .iter()
        .find(|w| endpoints(w).contains(&output) && w.y1 == w.y2)
        .unwrap();
    let id = wire.uuid.clone().unwrap();
    let [a, b] = endpoints(wire);
    let uuid = "77600000-0000-4000-8000-000000000007";
    let SexpNode::List(items) = &mut tree else {
        unreachable!()
    };
    items.retain(|node| node.find_str("uuid") != Some(&id));
    let y = a.1 + 20.32;
    for [(x1, y1), (x2, y2)] in [[a, (a.0, y)], [(a.0, y), (b.0, y)], [(b.0, y), b]] {
        items.push(parse_sexp(&format_wire(x1, y1, x2, y2)).unwrap());
    }
    items.push(
        parse_sexp(&format!(
            r#"(text "Keep clear" (at 131.445 98.6 0)
           (effects (font (size 1.27 1.27))) (uuid "{uuid}"))"#,
        ))
        .unwrap(),
    );
    let sheet = sheet.with_tree(tree);
    let feedback = sheet.id("R2");
    let result = sheet
        .serve(json!({
            "schematic": sheet.path,
            "anchor": sheet.id("U1"),
            "members": [feedback],
            "fixed_uuids": [uuid]
        }))
        .await;
    assert_eq!(result["status"], "improved", "{result}");
    let poses = result["plan"]["poses"].as_array().unwrap();
    assert!(
        poses
            .iter()
            .any(|pose| pose["uuid"] == feedback["uuid"]
                && pose["y"].as_f64().is_some_and(|y| y < 88.9)),
        "{result}"
    );
}

#[tokio::test]
async fn fixed_note_cannot_be_moved_to_make_room() {
    let sheet = Sheet::fixture("opamp_stage");
    let mut tree = sheet.tree.clone();
    let SexpNode::List(items) = &mut tree else {
        unreachable!()
    };
    let uuid = "77600000-0000-4000-8000-000000000001";
    items.push(
        parse_sexp(&format!(
            r#"(text "FIXED  CLEARANCE  PROBE" (at 112 88.9 0)
        (effects (font (size 15 15))) (uuid "{uuid}"))"#
        ))
        .unwrap(),
    );
    let sheet = sheet.with_tree(tree);
    let mut args = sheet.opamp();
    args["fixed_uuids"] = json!([uuid]);
    let report = sheet.serve(args).await;
    assert_eq!(report["status"], "blocked", "{report}");
    assert!(report["plan"].is_null());
}

#[tokio::test]
async fn unreadable_note_geometry_refuses_instead_of_assuming_clearance() {
    let sheet = Sheet::fixture("opamp_stage");
    let mut tree = sheet.tree.clone();
    let SexpNode::List(items) = &mut tree else {
        unreachable!()
    };
    let uuid = "77600000-0000-4000-8000-000000000002";
    items.push(
        parse_sexp(&format!(
            r#"(text "A note of unknown extent" (at 50.8 22.86 0) (uuid "{uuid}"))"#
        ))
        .unwrap(),
    );
    let sheet = sheet.with_tree(tree);
    let report = sheet.serve(sheet.opamp()).await;
    assert_eq!(report["status"], "blocked", "{report}");
    assert!(report["plan"].is_null());
    assert!(report["blockers"].to_string().contains(uuid), "{report}");
}

#[tokio::test]
async fn note_clearance_can_move_only_the_selected_note() {
    let sheet = Sheet::fixture("opamp_stage");
    let mut tree = sheet.tree.clone();
    let SexpNode::List(items) = &mut tree else {
        unreachable!()
    };
    let note = "77600000-0000-4000-8000-000000000003";
    let peer = "77600000-0000-4000-8000-000000000004";
    for (uuid, x, y) in [(note, 77.47, 60.96), (peer, 52.07, 44.45)] {
        items.push(
            parse_sexp(&format!(
                r#"(text "Note" (at {x} {y} 0) (effects (font (size 1.27 1.27))) (uuid "{uuid}"))"#
            ))
            .unwrap(),
        );
    }
    let sheet = sheet.with_tree(tree);
    let mut args = sheet.opamp();
    let unselected = sheet.serve(args.clone()).await;
    assert_eq!(unselected["status"], "blocked", "{unselected}");
    assert!(unselected["plan"].is_null());
    args["note_uuids"] = json!([note]);
    let selected = sheet.serve(args).await;
    assert_eq!(selected["status"], "improved", "{selected}");
    let moves = selected["plan"]["note_moves"].as_array().unwrap();
    assert_eq!(moves.len(), 1);
    assert_eq!(moves[0]["uuid"], note);
}

fn with_fixed_reference_crossing(sheet: Sheet) -> Sheet {
    let mut tree = sheet.tree.clone();
    let SexpNode::List(items) = &mut tree else {
        unreachable!()
    };
    let uuid = sheet.id("J3")["uuid"].as_str().unwrap().to_owned();
    let symbol = items
        .iter_mut()
        .find(|s| s.find_str("uuid") == Some(&uuid))
        .unwrap();
    let SexpNode::List(fields) = symbol else {
        unreachable!()
    };
    let reference = fields
        .iter_mut()
        .find(|f| {
            f.head() == Some("property") && f.get(1).and_then(SexpNode::as_str) == Some("Reference")
        })
        .unwrap();
    // The fixed power-entry wire runs from (71.12, 60.96) to (83.82, 60.96).
    poses::set_at(reference, (77.47, 60.96, 0.0)).unwrap();
    sheet.with_tree(tree)
}

#[tokio::test]
async fn a_wire_crossing_an_unselected_reference_outside_the_block_refuses_the_preview() {
    let sheet = with_fixed_reference_crossing(Sheet::fixture("opamp_stage"));
    let report = sheet.serve(sheet.opamp()).await;
    assert_eq!(report["status"], "blocked", "{report}");
    assert!(report["plan"].is_null());
    assert!(
        report["blockers"]
            .to_string()
            .contains("wire_field_overlap"),
        "{report}"
    );
}

#[tokio::test]
async fn an_already_placed_block_still_checks_clearance() {
    let sheet = Sheet::fixture("opamp_stage");
    let args = sheet.opamp();
    let report = sheet.serve(args.clone()).await;
    assert_eq!(report["status"], "improved", "{report}");
    let replay = sheet.replay(&report["plan"]);
    let sheet = sheet.with_tree(replay);
    let no_change = sheet.serve(args.clone()).await;
    assert_eq!(no_change["status"], "unchanged", "{no_change}");
    let sheet = with_fixed_reference_crossing(sheet);
    let report = sheet.serve(args).await;
    assert_eq!(report["status"], "blocked", "{report}");
    assert!(report["plan"].is_null());
    assert!(
        report["blockers"]
            .to_string()
            .contains("wire_field_overlap"),
        "{report}"
    );
}

#[tokio::test]
async fn a_netclass_directive_on_a_rebuilt_island_refuses_without_touching_the_sheet() {
    let sheet = Sheet::fixture("opamp_stage");
    let source = include_str!("../../../tests/fixtures/rigid_block_move_kicad10.kicad_sch");
    let existing = parse_sexp(source).unwrap();
    let mut directive = existing.find("netclass_flag").unwrap().clone();
    poses::set_at(&mut directive, (71.12, 127.0, 0.0)).unwrap();
    let mut tree = sheet.tree.clone();
    let SexpNode::List(items) = &mut tree else {
        unreachable!()
    };
    items.push(directive);
    let sheet = sheet.with_tree(tree);
    let report = sheet.serve(sheet.opamp()).await;
    assert_eq!(report["status"], "blocked", "{report}");
    assert!(report["plan"].is_null());
    assert!(
        report["blockers"]
            .to_string()
            .contains("net-class directive"),
        "{report}"
    );
}

#[test]
fn only_verified_power_leads_are_discounted() {
    let sheet = Sheet::fixture("opamp_stage");
    let mut tree = sheet.tree.clone();
    let SexpNode::List(items) = &mut tree else {
        unreachable!()
    };
    items.push(parse_sexp(&format_wire(50.8, 60.96, 53.34, 60.96)).unwrap());
    let inventory = Inventory::new(&tree).unwrap();
    let ids = mandatory_stubs(&tree, &inventory);
    let wire_at = |a, b| {
        inventory
            .wires
            .iter()
            .find(|w| {
                let ends = endpoints(w);
                ends.contains(&a) && ends.contains(&b)
            })
            .unwrap()
            .uuid
            .as_ref()
            .unwrap()
    };
    assert!(ids.contains(wire_at((124.46, 81.28), (124.46, 78.74))));
    assert!(!ids.contains(wire_at((63.5, 63.5), (71.12, 63.5))));
    assert!(!ids.contains(wire_at((50.8, 60.96), (53.34, 60.96))));
}

#[test]
fn custom_rail_route_refuses_to_pick_up_an_unrelated_label() {
    let sheet = Sheet::fixture("opamp_stage");
    let args = sheet.opamp();
    let request = Request::parse(&args).unwrap();
    let inventory = Inventory::new(&sheet.tree).unwrap();
    let anchor = inventory.symbol(request.anchor).unwrap();
    let labels = extract_labels(&sheet.tree);
    let topology = topology::Topology::new(&sheet.tree, &inventory, &labels);
    let members = movable_parts(anchor, &inventory, &request).unwrap();
    let changes = (0..MAX_TRIALS)
        .find_map(|trial| {
            let changes = place(anchor, &inventory, &topology, &members, &request, trial).ok()?;
            candidate::build(&sheet.tree, &inventory, &topology, &changes).ok()?;
            Some(changes)
        })
        .expect("at least one baseline trial can route");
    let [a, b] = changes.rail_routes.values().next().unwrap()[0];
    let at = ((a.0 + b.0) / 2.0, (a.1 + b.1) / 2.0);
    assert!(!inventory
        .wires
        .iter()
        .any(|w| topology::on_wire(at, endpoints(w))));
    let mut tree = sheet.tree.clone();
    let SexpNode::List(items) = &mut tree else {
        unreachable!()
    };
    items.push(
        parse_sexp(&format!(
            r#"(label "DO_NOT_JOIN" (at {} {} 0) (effects (font (size 1.27 1.27))))"#,
            at.0, at.1,
        ))
        .unwrap(),
    );
    let inventory = Inventory::new(&tree).unwrap();
    let labels = extract_labels(&tree);
    let topology = topology::Topology::new(&tree, &inventory, &labels);
    let error = candidate::build(&tree, &inventory, &topology, &changes).unwrap_err();
    assert!(
        error.contains("local rail route would join label DO_NOT_JOIN"),
        "{error}"
    );
}

#[tokio::test]
async fn bypass_can_use_an_existing_rail_tee_without_redrawing_its_branch() {
    let sheet = Sheet::fixture("opamp_stage");
    let mut tree = sheet.tree.clone();
    let supply = sheet.id("#PWR01");
    let original_wire = extract_wires(&tree)
        .into_iter()
        .find(|wire| {
            endpoints(wire).contains(&(124.46, 81.28)) && endpoints(wire).contains(&(124.46, 78.74))
        })
        .unwrap()
        .uuid
        .unwrap();
    let SexpNode::List(items) = &mut tree else {
        unreachable!()
    };
    let symbol = items
        .iter_mut()
        .find(|node| node.find_str("uuid") == supply["uuid"].as_str())
        .unwrap();
    let SexpNode::List(fields) = symbol else {
        unreachable!()
    };
    for field in fields
        .iter_mut()
        .filter(|node| node.head() == Some("property"))
    {
        if let Some((x, y, angle)) = copperpour_sexp::schematic::parse_at(field) {
            poses::set_at(field, (x, y - 5.08, angle)).unwrap();
        }
    }
    poses::set_at(symbol, (124.46, 73.66, 0.0)).unwrap();
    let branch = parse_sexp(&format_wire(124.46, 78.74, 116.84, 78.74)).unwrap();
    let branch_uuid = branch.find_str("uuid").unwrap().to_owned();
    items.extend([
        branch,
        parse_sexp(&format_wire(124.46, 78.74, 124.46, 73.66)).unwrap(),
        parse_sexp(&format_junction(124.46, 78.74)).unwrap(),
        parse_sexp(
            r#"(label "SUPPLY_ALIAS" (at 116.84 78.74 180)
            (effects (font (size 1.27 1.27)) (justify right bottom))
            (uuid "77600000-0000-4000-8000-000000000006"))"#,
        )
        .unwrap(),
    ]);
    let sheet = sheet.with_tree(tree);
    let report = sheet.serve(sheet.opamp()).await;
    assert_eq!(report["status"], "improved", "{report}");
    assert_eq!(
        report["plan"]["removed_power_symbol_uuids"],
        json!([sheet.id("#PWR06")["uuid"]])
    );
    let removed = report["plan"]["remove_wire_uuids"].as_array().unwrap();
    assert!(!removed
        .iter()
        .any(|id| id == &branch_uuid || id == &original_wire));
}

#[test]
fn the_preservation_gate_requires_the_removal_uuid_even_on_a_good_drawing() {
    let sheet = Sheet::fixture("opamp_stage");
    let args = sheet.opamp();
    let request = Request::parse(&args).unwrap();
    let (_, status, reasons, _, plan) = preview(&sheet.tree, &request).unwrap();
    assert_eq!(status, "improved", "{reasons:?}");
    let after = sheet.replay(&plan);
    let removed = vec![sheet.id("#PWR06")["uuid"].as_str().unwrap().to_owned()];
    assert!(accept_candidate(&sheet.tree, &after, &removed).is_ok());
    let Err(reasons) = accept_candidate(&sheet.tree, &after, &[]) else {
        panic!("the same candidate passed without declaring the removed supply");
    };
    assert!(
        reasons.iter().any(|why| why.contains(&removed[0])),
        "{reasons:?}"
    );
}

#[test]
fn the_preservation_gate_rejects_a_severed_bypass_even_with_the_correct_removal_uuid() {
    let sheet = Sheet::fixture("opamp_stage");
    let args = sheet.opamp();
    let (_, status, reasons, _, plan) =
        preview(&sheet.tree, &Request::parse(&args).unwrap()).unwrap();
    assert_eq!(status, "improved", "{reasons:?}");
    let mut after = sheet.replay(&plan);
    let inventory = Inventory::new(&after).unwrap();
    let cap = inventory
        .parts
        .values()
        .find(|p| p.inst.reference == "C1")
        .unwrap();
    let tip = cap.pin("1").unwrap().tip;
    let branch = inventory
        .wires
        .iter()
        .find(|wire| endpoints(wire).contains(&tip))
        .unwrap()
        .uuid
        .clone()
        .unwrap();
    let SexpNode::List(items) = &mut after else {
        unreachable!()
    };
    items.retain(|node| node.find_str("uuid") != Some(&branch));
    let removed: Vec<String> =
        serde_json::from_value(plan["removed_power_symbol_uuids"].clone()).unwrap();
    let Err(blockers) = accept_candidate(&sheet.tree, &after, &removed) else {
        panic!("the preview accepted a severed supply branch");
    };
    assert!(
        blockers.iter().any(|why| why.contains("C1")),
        "{blockers:?}"
    );
}
