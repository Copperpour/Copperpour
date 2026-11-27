use super::*;

pub(super) fn replay(source: &str, plan: &Value) -> String {
    let mut candidate = source.to_owned();
    for step in array(plan, "steps") {
        let removed: BTreeSet<_> = [
            "wires_removed",
            "power_symbols_removed",
            "junctions_removed",
        ]
        .into_iter()
        .flat_map(|key| array(step, key))
        .map(|id| id.as_str().unwrap())
        .collect();
        let mut found = BTreeSet::new();
        let mut carried = BTreeSet::new();
        let mut edits = Vec::new();
        for (start, end) in find_direct_child_blocks(&candidate, "kicad_sch") {
            let block = &candidate[start..end];
            let node = parse_sexp(block).unwrap();
            let Some(id) = node.find_str("uuid") else {
                continue;
            };
            if let Some(id) = removed.get(id) {
                assert!(found.insert(*id));
                edits.push(SexpEdit::delete(start, end));
            } else if let Some(m) = array(step, "no_connects_moved")
                .iter()
                .find(|m| m["uuid"] == id)
            {
                assert!(carried.insert(id.to_owned()));
                assert_eq!(node.head(), Some("no_connect"));
                let (x, y, _) = parse_at(&node).unwrap();
                assert_eq!(json!([x, y]), m["from"]);
                edits.push(SexpEdit::replace(
                    start,
                    end,
                    format!(
                        "(no_connect (at {} {}) (uuid {}))",
                        m["to"][0], m["to"][1], id
                    ),
                ));
            } else if let Some(m) = array(step, "symbols_moved")
                .iter()
                .find(|m| m["uuid"] == id)
            {
                let mut symbol =
                    cse::Symbol::from_sexp(&cse::sexp::parser::parse(block).unwrap()).unwrap();
                assert_eq!(json!(symbol.position()), m["from"]);
                symbol.set_rotation(m["rotation"].as_f64().unwrap());
                symbol.move_to(m["to"][0].as_f64().unwrap(), m["to"][1].as_f64().unwrap());
                edits.push(SexpEdit::replace(
                    start,
                    end,
                    cse::sexp::writer::write(&symbol.to_sexp()),
                ));
            }
        }
        assert_eq!(removed, found);
        assert_eq!(carried.len(), array(step, "no_connects_moved").len());
        candidate = apply_edits(candidate, edits);
        let points = array(step, "route");
        let wires = array(step, "wire_uuids_added");
        assert_eq!(points.len().saturating_sub(1), wires.len());
        for (leg, id) in points.windows(2).zip(wires) {
            candidate = insert_before_close(
                &candidate,
                &format!(
                "(wire (pts (xy {} {}) (xy {} {})) (stroke (width 0) (type default)) (uuid {}))",
                leg[0][0], leg[0][1], leg[1][0], leg[1][1], id,
            ),
            );
        }
        let junctions = array(step, "junctions_added");
        let ids = array(step, "junction_uuids_added");
        assert_eq!(junctions.len(), ids.len());
        for (at, id) in junctions.iter().zip(ids) {
            candidate = insert_before_close(
                &candidate,
                &format!(
                    "(junction (at {} {}) (diameter 0) (color 0 0 0 0) (uuid {}))",
                    at[0], at[1], id,
                ),
            );
        }
    }
    candidate
}

#[tokio::test]
#[ignore = "needs an installed KiCad 10 kicad-cli"]
async fn kicad_keeps_netlist_and_erc_for_replayed_cleanup() {
    const CROWDED_PRO: &str =
        include_str!("../../../tests/fixtures/region_cleanup_kicad10/crowded/crowded.kicad_pro");
    const FIELDS_PRO: &str =
        include_str!("../../../tests/fixtures/field_placement_kicad10/field_placement.kicad_pro");
    let jogged = jogged_power_lead(FIELDS);
    let flag_lead = flag_lead_with_blocked_horizontal_elbow();
    let near_miss = crowded_with_near_miss_wire();
    let cli = std::env::var("COPPERPOUR_TEST_KICAD_CLI").unwrap_or_else(|_| {
        crate::kicad_install::find_cli("")
            .expect("installed kicad-cli")
            .into_os_string()
            .into_string()
            .expect("UTF-8 kicad-cli path")
    });
    let full = [0, 0, 200, 200];
    for (case, (name, source, project, movable, [x1, y1, x2, y2])) in [
        ("crowded", CROWDED, CROWDED_PRO, &[][..], full),
        ("crowded", CROWDED, CROWDED_PRO, &["C1"][..], full),
        (
            "crowded",
            CARRIED_NO_CONNECT,
            CROWDED_PRO,
            &["C1"][..],
            full,
        ),
        ("crowded", CROWDED, CROWDED_PRO, &["#PWR05"][..], full),
        (
            "crowded",
            CROWDED,
            CROWDED_PRO,
            &["C1", "#PWR05"][..],
            [60, 60, 120, 106],
        ),
        (
            "crowded",
            REDUNDANT_POWER,
            CROWDED_PRO,
            &["#PWR99"][..],
            full,
        ),
        ("field_placement", FIELDS, FIELDS_PRO, &["#PWR03"][..], full),
        ("field_placement", &jogged, FIELDS_PRO, &[][..], full),
        (
            "field_placement",
            &flag_lead,
            FIELDS_PRO,
            &["#FLG99"][..],
            full,
        ),
        ("crowded", &near_miss, CROWDED_PRO, &[][..], full),
    ]
    .into_iter()
    .enumerate()
    {
        let args = json!({"x1":x1, "y1":y1, "x2":x2, "y2":y2, "movable":movable});
        let preview = Preview::new(source).await;
        let response = preview.ok(TOOL, args.clone()).await;
        assert_eq!(response, preview.ok(TOOL, args.clone()).await);
        let candidate = replay(source, &response);
        assert_eq!(
            source == candidate,
            source == near_miss,
            "case {} ({name} {movable:?}): {response}",
            case + 1
        );
        let again = Preview::new(&candidate).await.ok(TOOL, args).await;
        assert_eq!(again["steps"], json!([]), "{name} {movable:?}: {again}");

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(format!("{name}.kicad_sch"));
        std::fs::write(path.with_extension("kicad_pro"), project).unwrap();
        let mut netlists = Vec::new();
        let mut reports = Vec::new();
        for source in [source, candidate.as_str()] {
            std::fs::write(&path, source).unwrap();
            let output = path.with_extension("net");
            crate::tools::cli::export_netlist(&cli, &path, &output, "kicadsexpr")
                .await
                .unwrap();
            let text = std::fs::read_to_string(output).unwrap();
            let tree = parse_sexp(&text).unwrap();
            netlists.push(["components", "nets"].map(|tag| tree.find(tag).unwrap().clone()));
            let report = crate::tools::cli::run_erc(&cli, &path).await.unwrap();
            let mut counts = BTreeMap::new();
            for v in report.violations {
                // Library availability depends on the runner's KiCad tables.
                if !matches!(
                    v.rule.as_str(),
                    "lib_symbol_issues" | "footprint_link_issues"
                ) {
                    *counts.entry((v.rule, v.severity)).or_insert(0) += 1;
                }
            }
            reports.push(counts);
        }
        assert_eq!(netlists[0], netlists[1], "{name} {movable:?}: netlist");
        assert_eq!(reports[0], reports[1], "{name} {movable:?}: ERC");
    }
}
