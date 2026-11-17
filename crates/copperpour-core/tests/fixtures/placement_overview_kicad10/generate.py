"""Generate the get_placement_overview fixture and its expected values with
KiCad's own pcbnew API and DRC.

usage: python3 generate.py OUT_DIR        (KiCad 10 pcbnew and kicad-cli on PATH)

Writes OUT_DIR/placement_overview.kicad_pcb and .kicad_pro, then prints the
oracle recorded in README.md: each net's netclass as pcbnew resolves it, and
the ratsnest as `kicad-cli pcb drc` reports it (one unconnected item per
airwire), with lengths and crossings computed here from KiCad's endpoints.

Then, with `probe` as a second argument, prints which scratch nets each
pattern in PROBE_PATTERNS names, one pcbnew process per pattern.
"""
import json, math, os, subprocess, sys, tempfile
import pcbnew as P

MM = P.FromMM
def v(x, y): return P.VECTOR2I(MM(x), MM(y))

# reference, x, y, net of pad 1, net of pad 2. Pads sit at x -0.8 and +0.8.
PARTS = [
    ("R1", 20, 20, "SIG_A", "GND"),
    ("R2", 40, 20, "SIG_A", "GND"),
    ("R3", 28, 40, "SIG_A", "SIG_B"),
    ("R4", 30, 14, "SIG_B", "/power/VCC"),
    ("R5", 60, 20, "VCC", "/power/VCC"),
    ("R6", 60, 40, "VCC", "HV"),
    ("R7", 50, 45, "HV", "/drv/HV_OUT"),
    ("R8", 50, 30, "/drv/HV_OUT", "GND"),
    ("R9", 90, 30, "SIG_B", "GND"),   # outside the 10..70 x 10..50 outline
]
CLASSES = {
    "Power": "VCC", "HighVoltage": "HV*", "Rails": "*VCC",
    # KiCad also reads a pattern as a regular expression over the whole name.
    "Signals": "SIG_[AB]", "AnyChar": "S.G_A", "Drive": "/drv/HV_(OUT|IN)",
    "Unanchored": "IG_A",
}

# Patterns probed on a scratch board to pin how KiCad reads one.
PROBE_NETS = ["HV1", "HV22", "HVX", "/a/HV1", "hv1", "D_1", "D_12", "A.B", "AxB",
              "+3V3", "3V3", "A+B", "AAB", "NN", "N"]
PROBE_PATTERNS = ["HV", "HV*", "HV?", "D_?", "hv1", "/a/*", "*HV1", "V1", "A.B", "A\\.B",
                  "HV.*", "HV[0-9]+", "HV[^0-9]", "HV(1|X)", "HV1|HVX", "^HV1$",
                  "HV\\d+", "HV\\w+", "HV[[:digit:]]+", "(?i)hv1", "+3V3", "A+B",
                  # Read differently by the tool; see README.
                  "N{2}", "HV\\d{2}", "HV[0-9]{2}", "HV(?=1)1", "\\QA.B\\E"]

def rect(parent, layer, x0, y0, x1, y1, width):
    s = P.PCB_SHAPE(parent, P.SHAPE_T_RECTANGLE); s.SetLayer(layer); s.SetWidth(MM(width))
    s.SetStart(v(x0, y0)); s.SetEnd(v(x1, y1)); parent.Add(s)

def resistor(board, ref, x, y, nets):
    fp = P.FOOTPRINT(board); fp.SetReference(ref); fp.SetValue("10k")
    fp.SetFPID(P.LIB_ID("fixture", "R_0603")); fp.SetLayer(P.F_Cu)
    board.Add(fp); fp.SetPosition(v(0, 0))
    for num, px, net in (("1", -0.8, nets[0]), ("2", 0.8, nets[1])):
        p = P.PAD(fp); p.SetNumber(num); p.SetShape(P.F_Cu, P.PAD_SHAPE_RECTANGLE)
        p.SetAttribute(P.PAD_ATTRIB_SMD); p.SetLayerSet(P.PAD.SMDMask())
        p.SetSize(P.F_Cu, v(0.8, 0.95)); p.SetPosition(v(px, 0)); p.SetNet(net); fp.Add(p)
    rect(fp, P.F_CrtYd, -1.5, -0.75, 1.5, 0.75, 0.05)
    fp.SetPosition(v(x, y))

def probe(pattern):
    with tempfile.TemporaryDirectory() as tmp:
        path = os.path.join(tmp, "probe.kicad_pcb")
        board = P.NewBoard(path)
        fp = P.FOOTPRINT(board); fp.SetReference("J1"); board.Add(fp)
        for i, name in enumerate(PROBE_NETS):
            net = P.NETINFO_ITEM(board, name); board.Add(net)
            p = P.PAD(fp); p.SetNumber(str(i + 1)); p.SetAttribute(P.PAD_ATTRIB_SMD)
            p.SetLayerSet(P.PAD.SMDMask()); p.SetSize(P.F_Cu, v(1, 1))
            p.SetPosition(v(2 * i, 0)); p.SetNet(net); fp.Add(p)
        settings = board.GetDesignSettings().m_NetSettings
        settings.SetNetclass("C", P.NETCLASS("C"))
        settings.SetNetclassPatternAssignment(pattern, "C")
        P.SaveBoard(path, board); P.GetSettingsManager().SaveProject()
        named = [str(k) for k, n in P.LoadBoard(path).GetNetsByName().items()
                 if n.GetNetClassName() == "C"]
        print(f"  {pattern:16} {' '.join(sorted(named)) or '-'}")

if len(sys.argv) > 2 and sys.argv[2] == "probe-one":
    probe(sys.argv[3]); sys.exit(0)

out = sys.argv[1]
pcb = os.path.join(out, "placement_overview.kicad_pcb")
board = P.NewBoard(pcb)
nets = {}
for _, _, _, a, b in PARTS:
    for name in (a, b):
        if name not in nets:
            nets[name] = P.NETINFO_ITEM(board, name); board.Add(nets[name])
rect(board, P.Edge_Cuts, 10, 10, 70, 50, 0.1)
for ref, x, y, a, b in PARTS:
    resistor(board, ref, x, y, (nets[a], nets[b]))
settings = board.GetDesignSettings().m_NetSettings
for name, pattern in CLASSES.items():
    settings.SetNetclass(name, P.NETCLASS(name))
    settings.SetNetclassPatternAssignment(pattern, name)
P.SaveBoard(pcb, board)
P.GetSettingsManager().SaveProject()

# ── Oracle ───────────────────────────────────────────────────────────────────
loaded = P.LoadBoard(pcb)
print("netclass of each net (pcbnew GetNetClassName):")
for name, net in sorted((str(k), n) for k, n in loaded.GetNetsByName().items()):
    if name:
        print(f"  {name:14} {net.GetNetClassName()}")

report = os.path.join(out, "drc.json")
subprocess.run(["kicad-cli", "pcb", "drc", "--format", "json", "--severity-all",
                "--units", "mm", "-o", report, pcb], check=True, stdout=subprocess.DEVNULL,
               env={**os.environ, "LC_ALL": "C", "LANGUAGE": "en"})
def pad(description):
    # "Pad 1 [SIG_B] of R3 on F.Cu" -> "R3.1"
    words = description.split()
    return f"{words[4]}.{words[1]}"

edges = []
for item in json.load(open(report))["unconnected_items"]:
    a, b = item["items"]
    net = a["description"].split("[")[1].split("]")[0]
    edges.append((net, (a["pos"]["x"], a["pos"]["y"]), (b["pos"]["x"], b["pos"]["y"]),
                  pad(a["description"]), pad(b["description"])))
os.remove(report)

def length(e): return math.dist(e[1], e[2])
def crosses(e, f):
    o = lambda a, b, c: (b[0]-a[0])*(c[1]-a[1]) - (b[1]-a[1])*(c[0]-a[0])
    return (o(f[1], f[2], e[1]) * o(f[1], f[2], e[2]) < 0
            and o(e[1], e[2], f[1]) * o(e[1], e[2], f[2]) < 0)

print("ratsnest (kicad-cli pcb drc, unconnected items):")
per_net = {}
for e in sorted(edges, key=lambda e: -length(e)):
    per_net[e[0]] = per_net.get(e[0], 0) + length(e)
    print(f"  {e[0]:14} {e[3]} {e[1]} - {e[4]} {e[2]}  {length(e):.2f} mm")
for net, total in sorted(per_net.items()):
    print(f"  net {net:14} {total:.2f} mm")
signal = [e for e in edges if e[0] != "GND"]
print(f"total without GND: {sum(map(length, signal)):.2f} mm in {len(signal)} airwires")
per_part = {}
for e in signal:
    for ref in {e[3].split(".")[0], e[4].split(".")[0]}:
        count, total = per_part.get(ref, (0, 0.0))
        per_part[ref] = (count + 1, total + length(e))
print("airwires per footprint without GND:")
for ref, (count, total) in sorted(per_part.items(), key=lambda kv: -kv[1][1]):
    print(f"  {ref} {count} airwires {total:.2f} mm")
print("crossings without GND:",
      sum(crosses(e, f) for i, e in enumerate(signal) for f in signal[i+1:] if e[0] != f[0]))

if len(sys.argv) > 2 and sys.argv[2] == "probe":
    print("netclass pattern probe (nets: " + " ".join(PROBE_NETS) + "):")
    sys.stdout.flush()
    for pattern in PROBE_PATTERNS:
        subprocess.run([sys.executable, __file__, out, "probe-one", pattern],
                       check=True, stderr=subprocess.DEVNULL)
