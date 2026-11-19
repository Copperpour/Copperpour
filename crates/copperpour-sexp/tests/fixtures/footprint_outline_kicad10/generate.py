"""Generate the footprint-owned outline fixtures with KiCad's own pcbnew API,
and print KiCad's reading of each saved board.

usage: python3 generate.py OUT_DIR        (KiCad 10 pcbnew and kicad-cli on PATH)

Every board is built at the origin, then placed, rotated and flipped by
pcbnew itself, saved with SaveBoard, and read back with LoadBoard. The oracle
printed for README.md comes from that second load: each Edge.Cuts shape's
board-frame bbox (KiCad's GetBoundingBox less half the stroke), grouped by the
contour this script drew it into; the outline KiCad builds with
GetBoardPolygonOutlines; and the outline errors of `kicad-cli pcb drc`.
"""
import json, os, subprocess, sys
import pcbnew as P

MM = P.FromMM
STROKE = 0.05
def v(x, y): return P.VECTOR2I(MM(x), MM(y))
def mm(iu): return round(P.ToMM(iu), 6)

def shape(parent, kind, layer):
    s = P.PCB_SHAPE(parent, kind); s.SetLayer(layer); s.SetWidth(MM(STROKE))
    parent.Add(s)
    return s

def seg(parent, a, b, layer=P.Edge_Cuts):
    s = shape(parent, P.SHAPE_T_SEGMENT, layer); s.SetStart(v(*a)); s.SetEnd(v(*b))
    return s

def fillet(parent, centre, r, a0, a1, layer=P.Edge_Cuts):
    """An arc about `centre` from angle a0 to a1 (degrees, Y down, 90 points to +y)."""
    import math
    at = lambda t: (centre[0] + r * math.cos(math.radians(t)),
                    centre[1] + r * math.sin(math.radians(t)))
    s = shape(parent, P.SHAPE_T_ARC, layer)
    s.SetArcGeometry(v(*at(a0)), v(*at((a0 + a1) / 2)), v(*at(a1)))
    return s

def rect(parent, layer, x0, y0, x1, y1):
    s = shape(parent, P.SHAPE_T_RECTANGLE, layer); s.SetStart(v(x0, y0)); s.SetEnd(v(x1, y1))
    return s

def footprint(board, ref, name):
    fp = P.FOOTPRINT(board); fp.SetReference(ref); fp.SetValue(name)
    fp.SetFPID(P.LIB_ID("fixture", name)); fp.SetLayer(P.F_Cu)
    board.Add(fp); fp.SetPosition(v(0, 0))
    return fp

def pad(fp, num, x, y, w, h, net=None):
    p = P.PAD(fp); p.SetNumber(num); p.SetShape(P.F_Cu, P.PAD_SHAPE_RECTANGLE)
    p.SetAttribute(P.PAD_ATTRIB_SMD); p.SetLayerSet(P.PAD.SMDMask())
    p.SetSize(P.F_Cu, v(w, h)); p.SetPosition(v(x, y))
    if net is not None:
        p.SetNet(net)
    fp.Add(p)
    return p

def hole(fp, x, y, d):
    p = P.PAD(fp); p.SetNumber(""); p.SetAttribute(P.PAD_ATTRIB_NPTH)
    p.SetShape(P.F_Cu, P.PAD_SHAPE_CIRCLE); p.SetSize(P.F_Cu, v(d, d))
    p.SetDrillSize(v(d, d)); p.SetLayerSet(P.PAD.UnplatedHoleMask())
    p.SetPosition(v(x, y)); fp.Add(p)
    return p

def mech1(board, drop_right_edge=False, gap=0.005, split=False, holes=False):
    """MECH1 built to issue #27's figures: no electrical pads, 27 Edge.Cuts
    primitives, extents x 30..170 and y 50..150 at (100, 100), and a
    24.25 x 7.333 mm cut-out.

    The outer contour spans x -70..70 and y -50..50 about the origin: four
    3 mm corner arcs, a 20 x 8 mm notch in the bottom edge with 1 mm arcs at
    its four corners, and a V-notch with sharp corners in the top edge, 11
    segments and 8 arcs. The cutout is a 24.25 x 7.333 mm rectangle with 1 mm
    corner arcs, 4 segments and 4 arcs, off centre. Neither is symmetric, so a
    wrong mirror or rotation moves the extents. The cut-out's last segment
    stops `gap` mm short of the arc it meets: 0.005 mm closes within KiCad's
    0.01 mm chaining tolerance, 0.02 mm does not. `split` cuts a 0.004 mm
    segment off the top edge, shorter than the tolerance; `holes` adds two
    3.2 mm mounting holes. A courtyard on the outline makes the footprint a
    part to today's overlap check.
    """
    fp = footprint(board, "MECH1", "MECH1")
    top = ([seg(fp, (-67, -50), (-40.004, -50)), seg(fp, (-40.004, -50), (-40, -50))]
           if split else [seg(fp, (-67, -50), (-40, -50))])
    if holes:
        hole(fp, -60, -40, 3.2); hole(fp, 60, 40, 3.2)
    outer = [
        fillet(fp, (-67, -47), 3, 180, 270),
        *top,
        seg(fp, (-40, -50), (-35, -44)),
        seg(fp, (-35, -44), (-30, -50)),
        seg(fp, (-30, -50), (67, -50)),
        fillet(fp, (67, -47), 3, 270, 360),
    ]
    if not drop_right_edge:
        outer.append(seg(fp, (70, -47), (70, 47)))
    outer += [
        fillet(fp, (67, 47), 3, 0, 90),
        seg(fp, (67, 50), (31, 50)),
        fillet(fp, (31, 49), 1, 90, 180),
        seg(fp, (30, 49), (30, 43)),
        fillet(fp, (29, 43), 1, 360, 270),
        seg(fp, (29, 42), (11, 42)),
        fillet(fp, (11, 43), 1, 270, 180),
        seg(fp, (10, 43), (10, 49)),
        fillet(fp, (9, 49), 1, 0, 90),
        seg(fp, (9, 50), (-67, 50)),
        fillet(fp, (-67, 47), 3, 90, 180),
        seg(fp, (-70, 47), (-70, -47)),
    ]
    x0, x1, y0, y1 = -32.125, -7.875, 11.3335, 18.6665
    cutout = [
        fillet(fp, (x0 + 1, y0 + 1), 1, 180, 270),
        seg(fp, (x0 + 1, y0), (x1 - 1, y0)),
        fillet(fp, (x1 - 1, y0 + 1), 1, 270, 360),
        seg(fp, (x1, y0 + 1), (x1, y1 - 1)),
        fillet(fp, (x1 - 1, y1 - 1), 1, 0, 90),
        seg(fp, (x1 - 1, y1), (x0 + 1, y1)),
        fillet(fp, (x0 + 1, y1 - 1), 1, 90, 180),
        seg(fp, (x0, y1 - 1), (x0, y0 + 1 + gap)),
    ]
    rect(fp, P.F_CrtYd, -70, -50, 70, 50)
    return fp, {"MECH1 outer": outer, "MECH1 cutout": cutout}

def slot1(board):
    """A 6 x 2 mm Edge.Cuts rectangle with a pad on each side of it, outside it."""
    fp = footprint(board, "SLOT1", "SLOT1")
    edge = [rect(fp, P.Edge_Cuts, -3, -1, 3, 1)]
    pad(fp, "1", -5, 0, 1, 1); pad(fp, "2", 5, 0, 1, 1)
    rect(fp, P.F_CrtYd, -5.75, -1.25, 5.75, 1.25)
    return fp, {"SLOT1": edge}

def two_slots(board):
    """A 2 mm Edge.Cuts circle and a 2 x 2 mm Edge.Cuts polygon, no pads: more
    than one outline."""
    fp = footprint(board, "SLOTS2", "SLOTS2")
    c = shape(fp, P.SHAPE_T_CIRCLE, P.Edge_Cuts); c.SetStart(v(-4, 0)); c.SetEnd(v(-3, 0))
    p = shape(fp, P.SHAPE_T_POLY, P.Edge_Cuts)
    square = P.SHAPE_POLY_SET(); square.NewOutline()
    for x, y in ((3, -1), (5, -1), (5, 1), (3, 1)):
        square.Append(MM(x), MM(y))
    p.SetPolyShape(square)
    return fp, {"SLOTS2": [c, p]}

def rounded_rect(board):
    """A 40 x 30 mm Edge.Cuts rectangle with 3 mm corners, no pads."""
    fp = footprint(board, "RRECT1", "RRECT1")
    r = rect(fp, P.Edge_Cuts, -20, -15, 20, 15); r.SetCornerRadius(MM(3))
    return fp, {"RRECT1": [r]}

def slot_across_pad(board):
    """A 10 x 0.5 mm Edge.Cuts rectangle across a 6 x 4 mm pad: no slot corner
    lies in the pad's box, and no pad corner or the centre in the slot."""
    fp = footprint(board, "SLOTX", "SLOTX")
    pad(fp, "1", 0, 0, 6, 4)
    return fp, {"SLOTX": [rect(fp, P.Edge_Cuts, -5, 0.5, 5, 1)]}

def slot_off_axis(board):
    """A 6 x 2 mm Edge.Cuts rectangle beside its pad, off the footprint's
    origin, so a mirror or a half turn of the pad alone puts it in the slot."""
    fp = footprint(board, "SLOTB", "SLOTB")
    pad(fp, "1", -4, 0, 1, 1)
    return fp, {"SLOTB": [rect(fp, P.Edge_Cuts, 1, -1, 7, 1)]}

def halves(board):
    """A 40 x 30 mm outline drawn half by HALF_A (top and right) and half by
    HALF_B (bottom and left), no pads, and SLOTN inside it: a 10 x 4 mm
    Edge.Cuts rectangle, no pads, with a courtyard."""
    a = footprint(board, "HALF_A", "HALF_A")
    top_right = [seg(a, (-20, -15), (20, -15)), seg(a, (20, -15), (20, 15))]
    b = footprint(board, "HALF_B", "HALF_B")
    bottom_left = [seg(b, (20, 15), (-20, 15)), seg(b, (-20, 15), (-20, -15))]
    n = footprint(board, "SLOTN", "SLOTN")
    slot = [rect(n, P.Edge_Cuts, -5, -2, 5, 2)]
    rect(n, P.F_CrtYd, -5.5, -2.5, 5.5, 2.5)
    return (a, b, n), {"HALF_A+HALF_B": top_right + bottom_left, "SLOTN": slot}

def slot_in_pad(board):
    """A 1.5 x 1 mm Edge.Cuts rectangle inside a 6 x 4 mm pad, clear of the
    pad's centre and corners."""
    fp = footprint(board, "SLOTPAD", "SLOTPAD")
    pad(fp, "1", 0, 0, 6, 4)
    return fp, {"SLOTPAD": [rect(fp, P.Edge_Cuts, 1, -0.5, 2.5, 0.5)]}

def curve1(board):
    """A 20 mm square, no pads, whose left side is a Bezier curve."""
    fp = footprint(board, "CURVE1", "CURVE1")
    edge = [seg(fp, (-10, -10), (10, -10)), seg(fp, (10, -10), (10, 10)),
            seg(fp, (10, 10), (-10, 10))]
    c = shape(fp, P.SHAPE_T_BEZIER, P.Edge_Cuts)
    c.SetStart(v(-10, 10)); c.SetBezierC1(v(-15, 5)); c.SetBezierC2(v(-15, -5))
    c.SetEnd(v(-10, -10)); c.RebuildBezierToSegmentsPointsList(MM(0.005))
    return fp, {"CURVE1": edge + [c]}

def two_pad(board, ref, nets=(None, None)):
    fp = footprint(board, ref, "R_0603")
    pad(fp, "1", -0.8, 0, 0.8, 0.95, nets[0]); pad(fp, "2", 0.8, 0, 0.8, 0.95, nets[1])
    rect(fp, P.F_CrtYd, -1.5, -0.75, 1.5, 0.75)
    return fp

def connector(board, ref):
    fp = footprint(board, ref, "CONN_1x4")
    for i in range(4):
        pad(fp, str(i + 1), 0, -3 + 2 * i, 1.5, 1)
    rect(fp, P.F_CrtYd, -1.25, -4, 1.25, 4)
    return fp

def card_edge(board):
    """A card's 30 x 40 mm outline of four segments, with its fingers inside it."""
    fp = footprint(board, "J1", "CARD_EDGE")
    edge = [seg(fp, (-15, -20), (15, -20)), seg(fp, (15, -20), (15, 20)),
            seg(fp, (15, 20), (-15, 20)), seg(fp, (-15, 20), (-15, -20))]
    for i in range(5):
        pad(fp, str(i + 1), -8 + 4 * i, 17, 2, 4)
    rect(fp, P.F_CrtYd, -10, 14.5, 10, 19.5)
    return fp, {"J1": edge}

def build(name, out):
    path = os.path.join(out, name + ".kicad_pcb")
    board = P.NewBoard(path)
    groups = {}
    if name in ("mech1", "mech1_rotated", "mech1_back", "open_outline", "gap_outline",
                "short_segment"):
        fp, g = mech1(board, drop_right_edge=(name == "open_outline"),
                      gap=0.02 if name == "gap_outline" else 0.005,
                      split=(name == "short_segment"), holes=(name == "mech1_back"))
        groups.update(g)
        fp.SetPosition(v(100, 100))
        if name == "mech1_rotated":
            fp.SetOrientationDegrees(30)
        if name == "mech1_back":
            fp.Flip(fp.GetPosition(), P.FLIP_DIRECTION_LEFT_RIGHT)
            fp.SetOrientationDegrees(150)
    if name == "mech1":
        fp, g = slot1(board); groups.update(g); fp.SetPosition(v(140, 70))
        n1 = P.NETINFO_ITEM(board, "N1"); board.Add(n1)
        r1 = two_pad(board, "R1", (None, n1)); r1.SetPosition(v(60, 80))
        r2 = two_pad(board, "R2", (n1, None)); r2.SetPosition(v(120, 90))
        connector(board, "J2").SetPosition(v(165, 100))
        t = P.PCB_TRACK(board); t.SetLayer(P.F_Cu); t.SetWidth(MM(0.25)); t.SetNet(n1)
        t.SetStart(v(60.8, 80)); t.SetEnd(v(119.2, 90)); board.Add(t)
    if name == "slot_only":
        fp, g = slot1(board); groups.update(g); fp.SetPosition(v(100, 100))
    if name == "two_slots_only":
        fp, g = two_slots(board); groups.update(g); fp.SetPosition(v(100, 100))
    if name == "curve_outline":
        fp, g = curve1(board); groups.update(g); fp.SetPosition(v(100, 100))
    if name == "rounded_rect":
        fp, g = rounded_rect(board); groups.update(g)
        fp.SetPosition(v(100, 100)); fp.SetOrientationDegrees(90)
    if name == "slot_in_pad":
        fp, g = slot_in_pad(board); groups.update(g); fp.SetPosition(v(100, 100))
    if name == "slot_across_pad":
        fp, g = slot_across_pad(board); groups.update(g); fp.SetPosition(v(100, 100))
    if name == "slot_back":
        fp, g = slot_off_axis(board); groups.update(g)
        fp.SetPosition(v(100, 100))
        fp.Flip(fp.GetPosition(), P.FLIP_DIRECTION_LEFT_RIGHT)
        fp.SetOrientationDegrees(30)
    if name == "split_outline":
        fps, g = halves(board); groups.update(g)
        for fp in fps:
            fp.SetPosition(v(100, 100))
    if name in ("card_edge", "card_edge_hole"):
        fp, g = card_edge(board); groups.update(g)
        fp.SetPosition(v(100, 100))
        fp.Flip(fp.GetPosition(), P.FLIP_DIRECTION_LEFT_RIGHT)
        fp.SetOrientationDegrees(90)
        two_pad(board, "R1").SetPosition(v(100, 100))
        fp, g = slot1(board); groups.update(g)
        fp.SetPosition(v(140, 100) if name == "card_edge" else v(100, 92))
    uuids = {k: [s.m_Uuid.AsString() for s in shapes] for k, shapes in groups.items()}
    P.SaveBoard(path, board)
    return path, uuids

def tidy(out, name):
    for leftover in (".kicad_pro", ".kicad_prl"):
        if os.path.exists(os.path.join(out, name + leftover)):
            os.remove(os.path.join(out, name + leftover))

def describe(s):
    w = s.GetWidth() / 2
    bb = s.GetBoundingBox()
    d = {"bbox": [mm(bb.GetLeft() + w), mm(bb.GetTop() + w),
                  mm(bb.GetRight() - w), mm(bb.GetBottom() - w)]}
    if s.GetShape() in (P.SHAPE_T_SEGMENT, P.SHAPE_T_ARC):
        d["start"] = [mm(s.GetStart().x), mm(s.GetStart().y)]
        d["end"] = [mm(s.GetEnd().x), mm(s.GetEnd().y)]
    if s.GetShape() == P.SHAPE_T_ARC:
        d["mid"] = [mm(s.GetArcMid().x), mm(s.GetArcMid().y)]
    return d

def oracle(path, uuids):
    board = P.LoadBoard(path)
    shapes = {}
    for fp in board.GetFootprints():
        for item in fp.GraphicalItems():
            if item.GetLayer() == P.Edge_Cuts:
                shapes[item.m_Uuid.AsString()] = (fp.GetReference(), item)
    contours = {}
    for name, ids in uuids.items():
        bbs = [describe(shapes[i][1])["bbox"] for i in ids]
        contours[name] = {
            "primitives": len(ids),
            "bbox": [min(b[0] for b in bbs), min(b[1] for b in bbs),
                     max(b[2] for b in bbs), max(b[3] for b in bbs)],
        }
    owners = {}
    for ref, _ in shapes.values():
        owners[ref] = owners.get(ref, 0) + 1
    polys = P.SHAPE_POLY_SET()
    built = board.GetBoardPolygonOutlines(polys, False, None, True)
    outlines = []
    for i in range(polys.OutlineCount()):
        bb = polys.Outline(i).BBox()
        outlines.append({"holes": polys.HoleCount(i),
                         "bbox": [mm(bb.GetLeft()), mm(bb.GetTop()),
                                  mm(bb.GetRight()), mm(bb.GetBottom())]})
    report = path + ".drc.json"
    subprocess.run(["kicad-cli", "pcb", "drc", "--format", "json", "--severity-all",
                    "-o", report, path], check=True, capture_output=True)
    with open(report) as f:
        drc = json.load(f)
    os.remove(report)
    outline_errors = sorted({v["type"] for v in drc.get("violations", [])
                             if "outline" in v["type"]})
    return {"edge_cuts_by_owner": owners, "contours": contours,
            "polygon_outlines_built": built, "polygon_outlines": outlines,
            "drc_outline_errors": outline_errors}

out = sys.argv[1]
for name in ("mech1", "mech1_rotated", "mech1_back", "open_outline", "gap_outline",
             "short_segment", "slot_only", "slot_back", "two_slots_only", "slot_in_pad",
             "slot_across_pad", "split_outline", "curve_outline", "rounded_rect", "card_edge",
             "card_edge_hole"):
    path, uuids = build(name, out)
    print(name, json.dumps(oracle(path, uuids)))
    tidy(out, name)
