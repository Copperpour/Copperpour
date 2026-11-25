//! End-to-end test against a real KiCAD installation (kicad-cli).
//!
//! Drives the shipped binary over stdio through a full design loop:
//! create project → place components → wire → ERC → export Gerbers → DRC.
//! Also checks selected-layer PCB SVG export through the same MCP boundary.
//!
//! Requires kicad-cli and the standard symbol libraries, so it is `#[ignore]`
//! by default and run explicitly by the e2e-kicad workflow (and locally):
//!
//!     cargo test -p copperpour --test e2e_kicad -- --ignored --nocapture

use serde_json::{json, Value};
use std::io::{BufRead, BufReader, Write};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};

struct Mcp {
    child: Child,
    stdin: ChildStdin,
    reader: BufReader<ChildStdout>,
    next_id: i64,
}

/// Locate kicad-cli: KICAD_CLI env override, PATH, then platform defaults.
fn find_kicad_cli() -> Option<String> {
    if let Ok(p) = std::env::var("KICAD_CLI") {
        if std::path::Path::new(&p).exists() {
            return Some(p);
        }
    }
    let name = if cfg!(windows) {
        "kicad-cli.exe"
    } else {
        "kicad-cli"
    };
    if Command::new(name)
        .arg("--version")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok()
    {
        return Some(name.to_string());
    }
    let candidates: &[&str] = if cfg!(windows) {
        &[
            r"C:\KiCad\10.0\bin\kicad-cli.exe",
            r"C:\Program Files\KiCad\10.0\bin\kicad-cli.exe",
        ]
    } else if cfg!(target_os = "macos") {
        &["/Applications/KiCad/KiCad.app/Contents/MacOS/kicad-cli"]
    } else {
        &["/usr/bin/kicad-cli", "/usr/local/bin/kicad-cli"]
    };
    candidates
        .iter()
        .find(|c| std::path::Path::new(c).exists())
        .map(|c| c.to_string())
}

impl Mcp {
    fn spawn(kicad_cli: &str) -> Self {
        Self::spawn_with_env(kicad_cli, &[])
    }

    fn spawn_with_env(kicad_cli: &str, env: &[(&str, &std::ffi::OsStr)]) -> Self {
        let mut config = tempfile::Builder::new().suffix(".json").tempfile().unwrap();
        write!(config, "{}", json!({"kicad_cli": kicad_cli})).unwrap();
        config.flush().unwrap();
        let (_persist, config_path) = config.keep().unwrap();

        let mut cmd = Command::new(env!("CARGO_BIN_EXE_copperpour"));
        cmd.arg("--config")
            .arg(&config_path)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null());
        for (key, value) in env {
            cmd.env(key, value);
        }
        let mut child = cmd.spawn().unwrap();
        let stdin = child.stdin.take().unwrap();
        let reader = BufReader::new(child.stdout.take().unwrap());
        let mut p = Mcp {
            child,
            stdin,
            reader,
            next_id: 1,
        };
        p.request(
            "initialize",
            json!({
                "protocolVersion": "2025-06-18", "capabilities": {},
                "clientInfo": {"name": "e2e", "version": "0"}
            }),
        );
        p
    }

    fn request(&mut self, method: &str, params: Value) -> Value {
        let id = self.next_id;
        self.next_id += 1;
        writeln!(
            self.stdin,
            "{}",
            json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params})
        )
        .unwrap();
        self.stdin.flush().unwrap();
        loop {
            let mut line = String::new();
            assert!(self.reader.read_line(&mut line).unwrap() > 0, "server died");
            let v: Value = serde_json::from_str(line.trim()).unwrap();
            if v.get("id").and_then(Value::as_i64) == Some(id) {
                return v;
            }
        }
    }

    fn tool(&mut self, name: &str, args: Value) -> Value {
        let r = self.request("tools/call", json!({"name": name, "arguments": args}));
        let result = r["result"].clone();
        assert_ne!(
            result["isError"],
            json!(true),
            "tool {name} failed: {}",
            result["content"][0]["text"].as_str().unwrap_or("?")
        );
        result
    }

    fn load(&mut self, toolset: &str) {
        self.tool("load_toolset", json!({"name": toolset}));
    }
}

impl Drop for Mcp {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn body(result: &Value) -> Value {
    serde_json::from_str(result["content"][0]["text"].as_str().unwrap_or("{}"))
        .unwrap_or(Value::Null)
}

#[test]
#[ignore = "requires kicad-cli; run via e2e workflow"]
fn selected_layer_pcb_svg_export_through_mcp() {
    let Some(kicad_cli) = find_kicad_cli() else {
        panic!("kicad-cli not found — set KICAD_CLI or install KiCAD (this test is e2e-only)");
    };
    // KiCad 10-reserialized fixture with a board-owned F.SilkS line and an
    // Edge.Cuts outline; see its README for the source and geometry.
    let board = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../copperpour-core/tests/fixtures/drc_ownership_j1.kicad_pcb");
    let dir = tempfile::tempdir().unwrap();
    let both_path = dir.path().join("silk-and-edge.svg");
    let silk_path = dir.path().join("silk-only.svg");
    let mut mcp = Mcp::spawn(&kicad_cli);
    mcp.load("pcb_export");

    let both = body(&mcp.tool(
        "export_svg",
        json!({
            "board": board.to_string_lossy(),
            "output": both_path.to_string_lossy(),
            "layers": ["F.SilkS", "Edge.Cuts"]
        }),
    ));
    assert_eq!(both["success"], true, "{both}");
    let both_svg = std::fs::read_to_string(&both_path)
        .expect("selected-layer SVG was not written")
        .replace("\r\n", "\n");
    assert!(both_svg.contains("<svg") && both_svg.contains("</svg>"));
    assert!(both_svg.contains("M125.0000 60.0000\nL135.0000 60.0000"));
    assert!(both_svg.contains("M120.0000 56.5000\nL160.0000 56.5000"));

    let silk = body(&mcp.tool(
        "export_svg",
        json!({
            "board": board.to_string_lossy(),
            "output": silk_path.to_string_lossy(),
            "layers": ["F.SilkS"]
        }),
    ));
    assert_eq!(silk["success"], true, "{silk}");
    let silk_svg = std::fs::read_to_string(&silk_path)
        .expect("silkscreen SVG was not written")
        .replace("\r\n", "\n");
    assert!(silk_svg.contains("<svg") && silk_svg.contains("</svg>"));
    assert!(silk_svg.contains("M125.0000 60.0000\nL135.0000 60.0000"));
    assert!(!silk_svg.contains("M120.0000 56.5000\nL160.0000 56.5000"));
}

#[test]
#[ignore = "requires kicad-cli; run via e2e workflow"]
fn structural_scan_fixture_reloads_through_real_kicad() {
    let Some(kicad_cli) = find_kicad_cli() else {
        panic!("kicad-cli not found — set KICAD_CLI or install KiCAD (this test is e2e-only)");
    };
    let source = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../copperpour-core/tests/fixtures/structural_scans_kicad10.kicad_sch");
    let directory = tempfile::tempdir().unwrap();
    let schematic = directory.path().join("structural_scans.kicad_sch");
    std::fs::copy(&source, &schematic).unwrap();

    let upgrade = Command::new(&kicad_cli)
        .args(["sch", "upgrade", "--force"])
        .arg(&schematic)
        .output()
        .expect("failed to run KiCad schematic upgrade");
    assert!(
        upgrade.status.success(),
        "KiCad could not reload and save the structural fixture: {}",
        String::from_utf8_lossy(&upgrade.stderr)
    );

    let netlist = directory.path().join("structural_scans.net");
    let export = Command::new(&kicad_cli)
        .args(["sch", "export", "netlist", "--output"])
        .arg(&netlist)
        .arg(&schematic)
        .output()
        .expect("failed to run KiCad netlist export");
    assert!(
        export.status.success(),
        "KiCad could not export the reloaded structural fixture: {}",
        String::from_utf8_lossy(&export.stderr)
    );
    let netlist = std::fs::read_to_string(netlist).expect("KiCad wrote no netlist");
    assert!(netlist.contains("R1") && netlist.contains("R3"));
}

#[test]
#[ignore = "requires kicad-cli + symbol libraries; run via e2e workflow"]
fn full_design_loop_with_real_kicad() {
    let Some(kicad_cli) = find_kicad_cli() else {
        panic!("kicad-cli not found — set KICAD_CLI or install KiCAD (this test is e2e-only)");
    };
    // COPPERPOUR_E2E_KEEP_DIR: persist the generated project there (CI uploads
    // it as a failure artifact so file-format rejections can be diagnosed).
    let tmp = tempfile::tempdir().unwrap();
    let base: std::path::PathBuf = match copperpour_core::app_dirs::env_var("E2E_KEEP_DIR") {
        Some(d) => {
            std::fs::create_dir_all(&d).unwrap();
            d.into()
        }
        None => tmp.path().to_path_buf(),
    };
    let proj = base.join("e2e");
    let proj_s = proj.to_string_lossy().to_string();
    let sch = proj.join("e2e.kicad_sch");
    let pcb = proj.join("e2e.kicad_pcb");
    let mut p = Mcp::spawn(&kicad_cli);

    // ── Create ───────────────────────────────────────────────────────────
    p.tool("create_project", json!({"name": "e2e", "path": proj_s}));
    assert!(sch.exists() && pcb.exists());

    // ── Schematic: RC divider ────────────────────────────────────────────
    p.load("sch_components");
    p.load("sch_wiring");
    p.tool(
        "add_schematic_component",
        json!({
            "schematic": sch.to_string_lossy(), "lib_id": "Device:R",
            "reference": "R1", "value": "10k", "x": 100.0, "y": 100.0
        }),
    );
    p.tool(
        "add_schematic_component",
        json!({
            "schematic": sch.to_string_lossy(), "lib_id": "Device:C",
            "reference": "C1", "value": "100n", "x": 120.0, "y": 100.0
        }),
    );
    p.tool(
        "add_schematic_component",
        json!({
            "schematic": sch.to_string_lossy(), "lib_id": "Regulator_Linear:LM7805_TO220",
            "reference": "U1", "x": 140.0, "y": 100.0
        }),
    );
    // Both bottom pins sit at the same height, so the wire is straight. C1
    // pin 1 is above them: either L-route to it crosses another pin, which
    // connect_pins refuses.
    p.tool(
        "connect_pins",
        json!({
            "schematic": sch.to_string_lossy(),
            "ref1": "R1", "pin1": "2",
            "ref2": "C1", "pin2": "2"
        }),
    );

    // Labels are placed at the pin endpoint and oriented away from the symbol
    // body, so a rotated label must still bind its pin to the net. eeschema is
    // the only thing that can confirm that, hence here rather than a unit test.
    p.load("sch_batch");
    p.tool(
        "batch_connect_to_net",
        json!({
            "schematic": sch.to_string_lossy(), "net_name": "VIN",
            "pins": [{ "reference": "R1", "pin_number": "1" }]
        }),
    );

    // The written schematic must still parse and contain both parts.
    let content = std::fs::read_to_string(&sch).unwrap();
    let tree = copperpour_sexp::parse_sexp(&content).expect("tool output must reparse");
    let refs: Vec<_> = copperpour_sexp::schematic::extract_symbol_instances(&tree)
        .into_iter()
        .map(|s| s.reference)
        .collect();
    assert!(
        refs.contains(&"R1".to_string())
            && refs.contains(&"C1".to_string())
            && refs.contains(&"U1".to_string())
    );

    // KiCad's BOM exporter reads Datasheet and Description from the placed
    // instance, not the embedded lib_symbols fallback. Both rows must retain
    // the descriptions copied from the real Device library (#226).
    let bom_file = proj.join("metadata.csv");
    let bom_output = Command::new(&kicad_cli)
        .args(["sch", "export", "bom", "--output"])
        .arg(&bom_file)
        .args([
            "--fields",
            "Reference,Datasheet,Description",
            "--labels",
            "Reference,Datasheet,Description",
        ])
        .arg(&sch)
        .output()
        .expect("failed to run KiCad BOM exporter");
    assert!(
        bom_output.status.success(),
        "KiCad BOM export failed: {}",
        String::from_utf8_lossy(&bom_output.stderr)
    );
    let bom = std::fs::read_to_string(&bom_file).expect("KiCad wrote no BOM");
    for reference in ["R1", "C1", "U1"] {
        let prefix = format!("\"{reference}\",");
        let row = bom
            .lines()
            .find(|line| line.starts_with(&prefix))
            .unwrap_or_else(|| panic!("BOM has no {reference} row:\n{bom}"));
        assert!(
            !row.ends_with(",\"\""),
            "{reference} lost its library Description in KiCad's BOM:\n{bom}"
        );
    }
    assert!(
        !bom.lines().any(|line| line.starts_with("\"U1\",\"\",\"")),
        "U1 lost the regulator library Datasheet in KiCad's BOM:\n{bom}"
    );

    // ── ERC through real eeschema ────────────────────────────────────────
    p.load("sch_export");
    p.load("verification");
    let erc = body(&p.tool("run_erc", json!({"schematic": sch.to_string_lossy()})));
    // A 2-part net has floating-pin warnings; what matters is that eeschema
    // parsed OUR file and produced a structured report at all.
    assert!(
        erc.get("errors").is_some()
            || erc.get("violations").is_some()
            || erc.get("summary").is_some(),
        "unexpected ERC shape: {erc}"
    );
    // Every part on this sheet sits at x >= 100 mm, y = 100 mm, so a violation
    // on one of their pins cannot be at single-digit millimetres. That is what
    // KiCad's ERC JSON writer reported up to 10.0.6 — the whole sheet at 1/100
    // scale (#541, kicad#25582) — and this installation's version decides
    // which side of it the response is on.
    let coordinates = &erc["coordinates"];
    let status = coordinates["status"]
        .as_str()
        .unwrap_or_else(|| panic!("ERC response must state what its coordinates are worth: {erc}"));
    for violation in erc["violations"].as_array().into_iter().flatten() {
        for item in violation["items"].as_array().into_iter().flatten() {
            if status == "withheld" {
                assert!(
                    item["x"].is_null(),
                    "an unclassifiable version must offer no location: {item}"
                );
                continue;
            }
            let Some(x) = item["x"].as_f64() else {
                continue;
            };
            let y = item["y"].as_f64().expect("y rides with x");
            assert!(
                x > 10.0 && y > 10.0,
                "ERC placed a violation off the sheet at ({x}, {y}) — \
                 {status} against KiCad {:?}: {item}",
                coordinates["kicad_version"]
            );
        }
    }

    // eeschema's own netlist is the only proof that an oriented label still
    // binds its pin: rotating a label off 0° must not detach it.
    let net_file = proj.join("e2e.net");
    p.tool(
        "generate_netlist",
        json!({"schematic": sch.to_string_lossy(), "output": net_file.to_string_lossy()}),
    );
    let netlist = std::fs::read_to_string(&net_file).expect("kicad-cli wrote no netlist");
    // Not `netlist.contains("VIN")`: a label a millimetre off the pin still
    // names a net somewhere in the file. Only R1 pin 1 appearing as a node OF
    // that net proves the label bound the pin. eeschema may prefix the sheet
    // path to the name, so match it loosely.
    let vin = netlist
        .split("(net")
        .find(|block| {
            block
                .split_once("(name ")
                .and_then(|(_, rest)| rest.split_once(')'))
                .is_some_and(|(name, _)| name.contains("VIN"))
        })
        .unwrap_or_else(|| panic!("eeschema's netlist has no VIN net:\n{netlist}"));
    assert!(
        vin.split("(node")
            .any(|n| n.contains(r#"(ref "R1")"#) && n.contains(r#"(pin "1")"#)),
        "R1 pin 1 is not a node of VIN — the label did not bind its pin:\n{vin}"
    );

    // The review's runtime coverage uses Copperpour's parser so the tool remains
    // usable without kicad-cli. Cross-check that count here against KiCad's
    // own netlist so parser drift cannot turn incomplete extraction into a
    // clean verdict (#184).
    let netlist_tree = copperpour_sexp::parse_sexp(&netlist).expect("KiCad netlist must parse");
    let kicad_component_count = netlist_tree
        .find("components")
        .map(|components| components.find_all("comp").len())
        .unwrap_or(0);
    p.load("design_review");
    let review = body(&p.tool(
        "run_design_review",
        json!({"schematic": sch.to_string_lossy()}),
    ));
    assert_eq!(review["design_review"]["status"], "complete", "{review}");
    assert_eq!(
        review["design_review"]["coverage"]["schematic"]["symbol_instances"],
        json!(kicad_component_count),
        "Copperpour and KiCad disagree on schematic component coverage: {review}"
    );

    // ── PCB: export Gerbers + DRC through real kicad-cli ─────────────────
    p.load("pcb_export");
    let out_dir = proj.join("gerbers");
    p.tool(
        "export_gerber",
        json!({
            "board": pcb.to_string_lossy(),
            "output_dir": out_dir.to_string_lossy()
        }),
    );
    let produced = std::fs::read_dir(&out_dir).map(|d| d.count()).unwrap_or(0);
    assert!(
        produced > 0,
        "no gerber files produced in {}",
        out_dir.display()
    );

    let drc = body(&p.tool("run_drc", json!({"board": pcb.to_string_lossy()})));
    assert!(
        drc.get("errors").is_some()
            || drc.get("violations").is_some()
            || drc.get("summary").is_some(),
        "unexpected DRC shape: {drc}"
    );

    eprintln!("E2E OK: project created, wired, ERC'd, {produced} gerber files, DRC'd");
}

/// #326 in plotted form: wire groups drawn with no stroke, and the junction
/// dot's radius, which collapses from 0.4572 mm to 0.0001 mm when the Default
/// has no `wire_width`. Neither shows up in ERC, so plotting is the only
/// headless way to see it.
fn plotted_wires_and_junction(kicad_cli: &str, sch: &std::path::Path) -> (usize, f64) {
    let out = sch.parent().unwrap().join("svg");
    let _ = std::fs::remove_dir_all(&out);
    let status = Command::new(kicad_cli)
        .args([
            "sch",
            "export",
            "svg",
            "--exclude-drawing-sheet",
            "--output",
        ])
        .arg(&out)
        .arg(sch)
        .status()
        .expect("kicad-cli sch export svg failed to run");
    assert!(status.success(), "kicad-cli sch export svg exited {status}");

    let svg_path = std::fs::read_dir(&out)
        .unwrap()
        .filter_map(Result::ok)
        .map(|e| e.path())
        .find(|p| p.extension().is_some_and(|e| e == "svg"))
        .expect("kicad-cli wrote no SVG");
    let svg = std::fs::read_to_string(svg_path).unwrap();

    // Groups containing wires only. A healthy junction dot is filled rather
    // than stroked, so it carries `stroke:none` too.
    let mut strokeless = 0;
    let chunks: Vec<&str> = svg.split("style=\"").collect();
    for chunk in chunks.iter().skip(1) {
        let Some((style, body)) = chunk.split_once('"') else {
            continue;
        };
        if body.contains("<path d=") && style.contains("stroke:none") {
            strokeless += 1;
        }
    }

    // With two bare wires the only circle is the junction dot.
    let radius = svg
        .split("<circle")
        .skip(1)
        .filter_map(|c| c.split_once("r=\""))
        .filter_map(|(_, rest)| rest.split_once('"'))
        .filter_map(|(r, _)| r.parse::<f64>().ok())
        .fold(0.0_f64, f64::max);

    (strokeless, radius)
}

/// #326: a Default written with only the four PCB fields left eeschema unable
/// to place a junction anywhere in the project. The failure is in how KiCad
/// resolves the class rather than in the JSON, so only real eeschema can
/// confirm the fix.
#[test]
#[ignore = "requires kicad-cli; run via e2e workflow"]
fn a_written_default_netclass_still_plots_wires() {
    let Some(kicad_cli) = find_kicad_cli() else {
        panic!("kicad-cli not found — set KICAD_CLI or install KiCAD (this test is e2e-only)");
    };
    let tmp = tempfile::tempdir().unwrap();
    let proj = tmp.path().join("nc");
    let sch = proj.join("nc.kicad_sch");
    let pcb = proj.join("nc.kicad_pcb");
    let mut p = Mcp::spawn(&kicad_cli);

    p.tool(
        "create_project",
        json!({"name": "nc", "path": proj.to_string_lossy()}),
    );
    p.load("sch_wiring");
    // A T at (120, 100), where the junction dot belongs.
    p.tool(
        "batch_add_wire",
        json!({
            "schematic": sch.to_string_lossy(),
            "wires": [
                { "x1": 100.0, "y1": 100.0, "x2": 140.0, "y2": 100.0 },
                { "x1": 120.0, "y1": 100.0, "x2": 120.0, "y2": 120.0 }
            ]
        }),
    );
    // Baseline: no net_settings, so KiCad's seeded Default applies. The
    // radius comes from the wire width, which is what the assertions below
    // compare against.
    let (strokeless, radius) = plotted_wires_and_junction(&kicad_cli, &sch);
    assert_eq!(
        strokeless, 0,
        "a project with no net_settings must plot normally — the fixture is wrong, not the fix"
    );
    assert!(
        radius > 0.1,
        "no junction dot on the baseline plot (r={radius}) — the fixture is wrong, not the fix"
    );

    // The reported repro, exactly: name the Default and change nothing else.
    p.load("pcb_routing");
    p.tool(
        "create_netclass",
        json!({"board": pcb.to_string_lossy(), "name": "Default"}),
    );
    let (strokeless, after) = plotted_wires_and_junction(&kicad_cli, &sch);
    assert_eq!(
        strokeless, 0,
        "a Default written by create_netclass left eeschema with no wire width (#326)"
    );
    // Before the fix this collapsed to 0.0001 mm. Compared against the
    // baseline rather than a literal in case KiCad changes the ratio.
    assert!(
        (after - radius).abs() < 1e-6,
        "junction dot changed size after writing the Default: {radius} -> {after} (#326)"
    );

    // KiCad picks the default by name, not position, so a sparse named class
    // must not disturb it. Guards against "fixing" #326 by completing
    // whichever class comes first.
    p.tool(
        "create_netclass",
        json!({"board": pcb.to_string_lossy(), "name": "HV", "clearance": 0.5}),
    );
    let classes: Value =
        serde_json::from_str(&std::fs::read_to_string(proj.join("nc.kicad_pro")).unwrap()).unwrap();
    let names: Vec<&str> = classes["net_settings"]["classes"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| c["name"].as_str().unwrap_or_default())
        .collect();
    assert_eq!(names, vec!["Default", "HV"], "unexpected class order");
    let (strokeless, after) = plotted_wires_and_junction(&kicad_cli, &sch);
    assert_eq!(
        strokeless, 0,
        "a sparse named class must not disturb the Default"
    );
    assert!(
        (after - radius).abs() < 1e-6,
        "a sparse named class changed the junction dot: {radius} -> {after}"
    );
}

/// Every `(reference …)` and `Reference` property of `prefix` followed by a
/// number, set back to `prefix?`.
fn reset_references(content: &str, prefix: &str) -> String {
    let mut out = content.to_string();
    for needle in ["(reference \"", "(property \"Reference\" \""] {
        let mut next = String::with_capacity(out.len());
        let mut rest = out.as_str();
        while let Some(at) = rest.find(needle) {
            let value = at + needle.len();
            let len = rest[value..].find('"').unwrap();
            let reference = &rest[value..value + len];
            next.push_str(&rest[..value]);
            match reference.strip_prefix(prefix) {
                Some(number)
                    if !number.is_empty() && number.bytes().all(|b| b.is_ascii_digit()) =>
                {
                    next.push_str(prefix);
                    next.push('?');
                }
                _ => next.push_str(reference),
            }
            rest = &rest[value + len..];
        }
        next.push_str(rest);
        out = next;
    }
    out
}

/// KiCad's netlist of `root`: the component references, and whatever
/// kicad-cli said while exporting it.
fn netlisted(kicad_cli: &str, root: &std::path::Path, label: &str) -> (Vec<String>, String) {
    let netlist = root.with_file_name(format!("{label}.net"));
    let export = Command::new(kicad_cli)
        .args(["sch", "export", "netlist", "--output"])
        .arg(&netlist)
        .arg(root)
        .output()
        .expect("failed to run KiCad netlist export");
    assert!(export.status.success(), "netlist export failed: {export:?}");
    let said = format!(
        "{}{}",
        String::from_utf8_lossy(&export.stdout),
        String::from_utf8_lossy(&export.stderr)
    );
    let text = std::fs::read_to_string(netlist).expect("KiCad wrote no netlist");
    let references = copperpour_sexp::parse_sexp(&text)
        .expect("KiCad netlist must parse")
        .find("components")
        .expect("KiCad netlist has no components")
        .find_all("comp")
        .into_iter()
        .map(|comp| {
            comp.find_str("ref")
                .expect("a comp without a ref")
                .to_string()
        })
        .collect();
    (references, said)
}

#[test]
#[ignore = "requires kicad-cli; run via e2e workflow"]
fn hidden_power_input_shares_the_remote_vcc_net_in_kicad() {
    let cli = find_kicad_cli().expect("kicad-cli required for the native e2e workflow");
    let directory = tempfile::tempdir().unwrap();
    let schematic = directory.path().join("connector_hidden_power.kicad_sch");
    let netlist = schematic.with_extension("net");
    std::fs::write(
        &schematic,
        include_bytes!(
            "../../copperpour-core/tests/fixtures/connector_hidden_power_kicad10.kicad_sch"
        ),
    )
    .unwrap();
    let export = Command::new(cli)
        .args(["sch", "export", "netlist", "--output"])
        .arg(&netlist)
        .arg(&schematic)
        .output()
        .unwrap();
    assert!(export.status.success(), "{export:?}");
    let text = std::fs::read_to_string(netlist).unwrap();
    let tree = copperpour_sexp::parse_sexp(&text).unwrap();
    let vcc = tree
        .find("nets")
        .unwrap()
        .find_all("net")
        .into_iter()
        .find(|net| net.find_str("name") == Some("VCC"))
        .expect("KiCad should merge the hidden input and power symbol into VCC");
    let members: std::collections::BTreeSet<_> = vcc
        .find_all("node")
        .into_iter()
        .map(|pin| (pin.find_str("ref").unwrap(), pin.find_str("pin").unwrap()))
        .collect();
    assert_eq!(
        members,
        [("J1", "1"), ("R1", "1"), ("U1", "1"), ("U1", "14")]
            .into_iter()
            .collect()
    );
}

#[test]
#[ignore = "requires kicad-cli; run via e2e workflow"]
fn a_nested_hierarchy_annotated_from_its_root_netlists_cleanly() {
    let Some(kicad_cli) = find_kicad_cli() else {
        panic!("kicad-cli not found — set KICAD_CLI or install KiCAD (this test is e2e-only)");
    };
    // The nested fixture (#42): cell is placed four times, two of them under
    // each of bank's two placements. Both cases leave one Y?; the second also
    // sets every R back to R?.
    let fixtures = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../copperpour-core/tests/fixtures/annotate_nested_hierarchy");
    for reset in [false, true] {
        let directory = tempfile::tempdir().unwrap();
        for name in [
            "nested_pages.kicad_pro",
            "nested_pages.kicad_sch",
            "bank.kicad_sch",
            "cell.kicad_sch",
        ] {
            let content = std::fs::read_to_string(fixtures.join(name)).unwrap();
            // The fixture is fully annotated. cell's one "Y3" is the crystal's
            // record on north/west, page 11; its Reference property shows
            // south/east's Y1, the first of its instances in page order.
            let content = if name == "cell.kicad_sch" {
                assert_eq!(content.matches("\"Y3\"").count(), 1);
                assert_eq!(content.matches("(reference \"Y3\")").count(), 1);
                content.replace("(reference \"Y3\")", "(reference \"Y?\")")
            } else {
                content
            };
            let content = if reset {
                reset_references(&content, "R")
            } else {
                content
            };
            std::fs::write(directory.path().join(name), content).unwrap();
        }
        let root = directory.path().join("nested_pages.kicad_sch");
        let (_, said) = netlisted(&kicad_cli, &root, "before");
        assert!(said.contains("annotation errors"), "{said}");

        let mut p = Mcp::spawn(&kicad_cli);
        p.load("sch_components");
        let result = p.tool(
            "annotate_schematic",
            json!({"schematic": root.to_string_lossy(), "schematic_scope": "hierarchy"}),
        );
        let body = body(&result);
        assert_eq!(body["outcome"]["status"], "complete", "{body}");

        let (after, said) = netlisted(&kicad_cli, &root, "after");
        assert!(!said.contains("annotation errors"), "{said}");
        let distinct: std::collections::BTreeSet<&String> = after.iter().collect();
        assert_eq!(
            after.len(),
            37,
            "five parts on the root, six on each of two bank instances and five on each of four \
             cell instances, power symbols aside: {after:?}"
        );
        assert_eq!(
            distinct.len(),
            after.len(),
            "every component its own: {after:?}"
        );
        assert!(after.iter().any(|r| r == "Y3"), "{after:?}");
        if reset {
            assert!(
                (1..=16).all(|n| after.contains(&format!("R{n}"))),
                "{after:?}"
            );
        }
    }
}

/// Live Windows KiCad 10 validation for `open_board` (#23):
/// 1. Fresh launch from a path containing spaces → live IPC `get_board_info`.
/// 2. Already-open reuse (`launched: false`).
/// 3. Lock-file refusal (`conflict`).
/// 4. Project Manager (`api.sock`) + `open_board` launching `pcbnew.exe`
///    beside it → selects `api-<pid>.sock` when KiCad binds a fallback pipe,
///    or verifies the honest Windows Project Manager timeout + recovery note
///    on KiCad 10.0.8 (where `common/api/api_server.cpp:115` checks filesystem
///    `socket.Exists()` and never creates `api-<pid>.sock` on Windows).
///
/// Seeds KiCad config inside a private tempdir (`APPDATA` / `KICAD_CONFIG_HOME`)
/// so the developer's real KiCad profile is never touched, and requires `CI` or
/// `COPPERPOUR_E2E_ALLOW_KILL_KICAD=1` before killing KiCad processes.
#[test]
#[cfg(windows)]
#[ignore = "requires Windows KiCad 10 GUI; run via e2e workflow"]
fn e2e_windows_open_board_live_kicad() {
    let allow_kill = std::env::var_os("CI").is_some()
        || std::env::var("COPPERPOUR_E2E_ALLOW_KILL_KICAD").as_deref() == Ok("1");
    if !allow_kill {
        eprintln!(
            "skipping e2e_windows_open_board_live_kicad: set CI=1 or \
             COPPERPOUR_E2E_ALLOW_KILL_KICAD=1 to allow terminating kicad.exe/pcbnew.exe"
        );
        return;
    }

    let Some(kicad_cli) = find_kicad_cli() else {
        panic!("kicad-cli.exe not found — set KICAD_CLI or install KiCad");
    };
    let bin_dir = std::path::Path::new(&kicad_cli)
        .parent()
        .expect("kicad-cli.exe must have a parent bin directory");
    let kicad_exe = bin_dir.join("kicad.exe");
    assert!(kicad_exe.exists(), "kicad.exe not found at {kicad_exe:?}");

    struct KillKiCadOnDrop;
    impl KillKiCadOnDrop {
        fn kill_all() {
            for image in ["pcbnew.exe", "kicad.exe"] {
                let _ = Command::new("taskkill")
                    .args(["/F", "/IM", image])
                    .stdout(Stdio::null())
                    .stderr(Stdio::null())
                    .status();
            }
        }
    }
    impl Drop for KillKiCadOnDrop {
        fn drop(&mut self) {
            Self::kill_all();
        }
    }
    let _cleanup = KillKiCadOnDrop;
    KillKiCadOnDrop::kill_all();

    // Seed KiCad 10 config inside a private tempdir and pass it via APPDATA and
    // KICAD_CONFIG_HOME to the MCP server (inherited by pcbnew.exe) and to
    // kicad.exe so the user's real %APPDATA%\kicad\10.0 profile is never touched.
    // Omit "meta" so KiCad accepts the partial JSON without showing a settings
    // version/corruption "Information" dialog before starting the IPC server.
    let cfg_root = tempfile::tempdir().unwrap();
    let kicad_config_home = cfg_root.path().join("kicad");
    let kicad_cfg_dir = kicad_config_home.join("10.0");
    std::fs::create_dir_all(&kicad_cfg_dir).unwrap();
    let kicad_env: [(&str, &std::ffi::OsStr); 2] = [
        ("APPDATA", cfg_root.path().as_os_str()),
        ("KICAD_CONFIG_HOME", kicad_config_home.as_os_str()),
    ];
    let ensure_api_server_enabled = || {
        let common_path = kicad_cfg_dir.join("kicad_common.json");
        let mut cfg: Value = std::fs::read_to_string(&common_path)
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_else(|| json!({}));
        if !cfg.is_object() {
            cfg = json!({});
        }
        let obj = cfg.as_object_mut().unwrap();
        let api = obj.entry("api").or_insert_with(|| json!({}));
        if !api.is_object() {
            *api = json!({});
        }
        api.as_object_mut()
            .unwrap()
            .insert("enable_server".to_string(), json!(true));
        let dnsa = obj.entry("do_not_show_again").or_insert_with(|| json!({}));
        if !dnsa.is_object() {
            *dnsa = json!({});
        }
        let dnsa_obj = dnsa.as_object_mut().unwrap();
        dnsa_obj.insert("data_collection_prompt".to_string(), json!(true));
        dnsa_obj.insert("update_check_prompt".to_string(), json!(true));
        let gfx = obj.entry("graphics").or_insert_with(|| json!({}));
        if !gfx.is_object() {
            *gfx = json!({});
        }
        gfx.as_object_mut()
            .unwrap()
            .insert("canvas_type".to_string(), json!(2));
        std::fs::write(&common_path, serde_json::to_string_pretty(&cfg).unwrap()).unwrap();
    };
    ensure_api_server_enabled();
    std::fs::write(
        kicad_cfg_dir.join("kicad.json"),
        serde_json::to_string_pretty(&json!({
            "system": {
                "first_run_shown": true,
                "check_for_kicad_updates": false
            }
        }))
        .unwrap(),
    )
    .unwrap();
    std::fs::write(
        kicad_cfg_dir.join("pcbnew.json"),
        serde_json::to_string_pretty(&json!({
            "graphics": { "canvas_type": 2 }
        }))
        .unwrap(),
    )
    .unwrap();
    for (file, body) in [
        ("fp-lib-table", "(fp_lib_table\n  (version 7)\n)\n"),
        ("sym-lib-table", "(sym_lib_table\n  (version 7)\n)\n"),
        (
            "design-block-lib-table",
            "(design_block_lib_table\n  (version 7)\n)\n",
        ),
    ] {
        let p = kicad_cfg_dir.join(file);
        if !p.exists() {
            std::fs::write(p, body).unwrap();
        }
    }

    // Dismiss any headless-CI modal popups (e.g. OpenGL fallback "Information"
    // or first-run welcome dialogs) that would otherwise keep KiCad answering
    // AS_NOT_READY on a GPU-less Windows Server runner.
    struct DismissDialogsGuard(Option<Child>);
    impl Drop for DismissDialogsGuard {
        fn drop(&mut self) {
            if let Some(mut c) = self.0.take() {
                let _ = c.kill();
                let _ = c.wait();
            }
        }
    }
    let dismiss_script = r##"
Add-Type @"
using System;
using System.Runtime.InteropServices;
using System.Text;
public class WinDlg {
    public delegate bool EnumProc(IntPtr h, IntPtr l);
    [DllImport("user32.dll")] public static extern bool EnumWindows(EnumProc p, IntPtr l);
    [DllImport("user32.dll")] public static extern uint GetWindowThreadProcessId(IntPtr h, out uint pid);
    [DllImport("user32.dll", CharSet=CharSet.Auto)] public static extern int GetClassName(IntPtr h, StringBuilder s, int n);
    [DllImport("user32.dll", CharSet=CharSet.Auto)] public static extern int GetWindowText(IntPtr h, StringBuilder s, int n);
    [DllImport("user32.dll")] public static extern bool IsWindowVisible(IntPtr h);
    [DllImport("user32.dll")] public static extern bool PostMessage(IntPtr h, uint msg, IntPtr w, IntPtr l);
}
"@
while ($true) {
    $pids = @(Get-Process kicad,pcbnew -ErrorAction SilentlyContinue | ForEach-Object { [uint32]$_.Id })
    if ($pids.Count -gt 0) {
        [WinDlg]::EnumWindows({
            param($h, $l)
            if (-not [WinDlg]::IsWindowVisible($h)) { return $true }
            $p = [uint32]0
            [void][WinDlg]::GetWindowThreadProcessId($h, [ref]$p)
            if ($pids -contains $p) {
                $cls = New-Object System.Text.StringBuilder 256
                $txt = New-Object System.Text.StringBuilder 256
                [void][WinDlg]::GetClassName($h, $cls, 256)
                [void][WinDlg]::GetWindowText($h, $txt, 256)
                $c = $cls.ToString()
                $t = $txt.ToString()
                if ($t -eq "Confirmation") {
                    Write-Host "[dialog-dismisser] confirming $c '$t' (pid=$p)"
                    [void][WinDlg]::PostMessage($h, 0x0111, [IntPtr]6, [IntPtr]::Zero)
                    [void][WinDlg]::PostMessage($h, 0x0100, [IntPtr]0x0D, [IntPtr]::Zero)
                } elseif ($c -eq "#32770" -or $t -eq "Information" -or $t.StartsWith("Welcome") -or $t.StartsWith("Configure")) {
                    Write-Host "[dialog-dismisser] closing $c '$t' (pid=$p)"
                    [void][WinDlg]::PostMessage($h, 0x0010, [IntPtr]::Zero, [IntPtr]::Zero)
                }
            }
            return $true
        }, [IntPtr]::Zero) | Out-Null
    }
    Start-Sleep -Milliseconds 500
}
"##;
    let _dialog_dismisser = DismissDialogsGuard(
        Command::new("powershell")
            .args(["-NoProfile", "-Command", dismiss_script])
            .stdout(Stdio::inherit())
            .stderr(Stdio::inherit())
            .spawn()
            .ok(),
    );

    let kicad_window_titles = || -> String {
        Command::new("powershell")
            .args([
                "-NoProfile",
                "-Command",
                "Get-Process kicad,pcbnew -ErrorAction SilentlyContinue | ForEach-Object { \"$($_.ProcessName)($($_.Id)): $($_.MainWindowTitle)\" }",
            ])
            .output()
            .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
            .unwrap_or_default()
    };

    // Copy a real KiCad 10 fixture into a directory whose path contains spaces.
    let dir = tempfile::tempdir().unwrap();
    let spaced_dir = dir.path().join("board with spaces");
    std::fs::create_dir_all(&spaced_dir).unwrap();
    let board_path = spaced_dir.join("test board.kicad_pcb");
    std::fs::write(spaced_dir.join("test board.kicad_pro"), "{}\n").unwrap();
    std::fs::write(
        &board_path,
        include_bytes!(
            "../../copperpour-core/tests/fixtures/repeat_layout_routing_kicad10.kicad_pcb"
        ),
    )
    .unwrap();
    let board_str = board_path.to_string_lossy().to_string();

    // 1. Nothing running → open_board launches pcbnew.exe and binds the board.
    let mut mcp = Mcp::spawn_with_env(&kicad_cli, &kicad_env);
    mcp.load("verification");
    mcp.load("pcb_board");
    let opened = body(&mcp.tool(
        "open_board",
        json!({ "board": board_str, "timeout_seconds": 60 }),
    ));
    assert_eq!(
        opened["requested_open"],
        true,
        "fresh launch: {opened}; windows: {}",
        kicad_window_titles()
    );
    assert_eq!(opened["launched"], true, "fresh launch: {opened}");
    assert_eq!(opened["timed_out"], false, "fresh launch: {opened}");

    let info = body(&mcp.tool("get_board_info", json!({ "board": board_str })));
    assert_eq!(
        info["source"], "ipc",
        "subsequent PCB read must reach live IPC: {info}"
    );

    // 2. Board already open → open_board reuses existing endpoint with no duplicate launch.
    let reused = body(&mcp.tool(
        "open_board",
        json!({ "board": board_str, "timeout_seconds": 30 }),
    ));
    assert_eq!(reused["requested_open"], true, "reuse: {reused}");
    assert_eq!(reused["launched"], false, "reuse: {reused}");

    // 3. Lock-file refusal when no endpoint reports the board.
    drop(mcp);
    KillKiCadOnDrop::kill_all();
    std::thread::sleep(std::time::Duration::from_secs(2));
    let lock_path = spaced_dir.join("~test board.kicad_pcb.lck");
    std::fs::write(&lock_path, "stale-lock").unwrap();
    let mut mcp_lock = Mcp::spawn_with_env(&kicad_cli, &kicad_env);
    mcp_lock.load("verification");
    let lock_resp = mcp_lock.request(
        "tools/call",
        json!({
            "name": "open_board",
            "arguments": { "board": board_str, "timeout_seconds": 15 }
        }),
    );
    let lock_res = &lock_resp["result"];
    assert_eq!(lock_res["isError"], true, "lock must refuse: {lock_res}");
    let lock_body: Value =
        serde_json::from_str(lock_res["content"][0]["text"].as_str().unwrap()).unwrap();
    assert_eq!(
        lock_body["error"]["kind"], "conflict",
        "lock refusal must be conflict: {lock_body}"
    );
    drop(mcp_lock);
    std::fs::remove_file(&lock_path).unwrap();

    // 4. Project Manager running on api.sock + open_board launching pcbnew.exe on api-<pid>.sock.
    // Re-enable api.enable_server in case dismissing KiCad's first-run setup dialog
    // in Scenario 1 overwrote kicad_common.json with defaults.
    ensure_api_server_enabled();
    let mut manager_cmd = Command::new(&kicad_exe);
    manager_cmd.stdout(Stdio::null()).stderr(Stdio::null());
    for (key, value) in &kicad_env {
        manager_cmd.env(key, value);
    }
    let mut manager = manager_cmd
        .spawn()
        .expect("spawn kicad.exe Project Manager");
    let wait_deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
    let mut manager_ready = false;
    while std::time::Instant::now() < wait_deadline {
        let candidates = copperpour_ipc::candidate_endpoints();
        if candidates.iter().any(|addr| {
            copperpour_ipc::KiCadIpcClient::new(addr)
                .ping_outcome()
                .is_responsive()
        }) {
            manager_ready = true;
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(500));
    }
    assert!(
        manager_ready,
        "Project Manager did not expose a responsive IPC pipe within 30s; candidates={:?}",
        copperpour_ipc::candidate_endpoints()
    );

    // Re-enable again before open_board launches pcbnew.exe, in case kicad.exe's
    // startup rewrote kicad_common.json.
    ensure_api_server_enabled();
    let mut mcp_pm = Mcp::spawn_with_env(&kicad_cli, &kicad_env);
    mcp_pm.load("verification");
    mcp_pm.load("pcb_board");
    let opened_beside_pm = body(&mcp_pm.tool(
        "open_board",
        json!({ "board": board_str, "timeout_seconds": 15 }),
    ));
    if opened_beside_pm["requested_open"] == true {
        // KiCad bound a fallback api-<pid>.sock pipe beside Project Manager's api.sock.
        eprintln!(
            "[e2e_windows_open_board_live_kicad] scenario 4: pcbnew bound fallback pipe {}",
            opened_beside_pm["ipc_address"]
        );
        assert_eq!(
            opened_beside_pm["launched"], true,
            "beside Project Manager: {opened_beside_pm}"
        );
        let selected_addr = opened_beside_pm["ipc_address"]
            .as_str()
            .expect("selected ipc_address");
        assert!(
            selected_addr.to_ascii_lowercase().contains("api-"),
            "expected pcbnew.exe fallback pipe api-<pid>.sock beside Project Manager, got: {selected_addr}"
        );
        let info_beside_pm = body(&mcp_pm.tool("get_board_info", json!({ "board": board_str })));
        assert_eq!(
            info_beside_pm["source"], "ipc",
            "subsequent PCB read beside Project Manager must reach live IPC: {info_beside_pm}"
        );
    } else {
        // KiCad 10.0.8 on Windows checks `socket.Exists()` on the filesystem
        // (common/api/api_server.cpp:115), which is always false for an NNG
        // named pipe, so a pcbnew.exe started beside kicad.exe fails to bind
        // api.sock and never creates api-<pid>.sock. Verify open_board reports
        // an honest timeout with the Project Manager recovery guidance rather
        // than returning a false success on api.sock.
        eprintln!(
            "[e2e_windows_open_board_live_kicad] scenario 4: took KiCad 10.0.8 honest-timeout branch"
        );
        assert_eq!(
            opened_beside_pm["launched"], true,
            "beside Project Manager: {opened_beside_pm}"
        );
        assert_eq!(
            opened_beside_pm["timed_out"], true,
            "beside Project Manager: {opened_beside_pm}"
        );
        let note = opened_beside_pm["note"].as_str().unwrap_or("");
        assert!(
            note.contains("Project Manager") && note.contains("api-<pid>.sock"),
            "expected honest Windows Project Manager timeout note, got: {opened_beside_pm}"
        );
    }

    let _ = manager.kill();
    let _ = manager.wait();
}
