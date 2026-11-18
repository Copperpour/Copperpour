# `repeat_layout` area fixtures

Five boards written by pcbnew 10.0.6's Python on Linux from
`repeat_layout_routing_kicad10.kicad_pcb`, whose README says how it was made,
and KiCad's own box of every item of the first. Only the `.kicad_pcb` files and
the boxes are committed. Every item of the routed board keeps its uuid on them.

## `repeat_layout_area_kicad10.kicad_pcb`

The routed board with, 0.2 mm tracks and 0.6 mm vias with a 0.3 mm drill:

| Item | Where | In Channel 1's area |
|---|---|---|
| GND track on B.Cu, R3.2 to a via | (109.5, 102.5) to (109.5, 101.5) | yes |
| GND via | (109.5, 101.5) | yes |
| Channel 1's IN stub, which reaches R13 on the root sheet | R1.1 to (109.5, 100.8) | yes |
| VCC track, on no pad of Channel 1 | (111, 101.6) to (111, 102) | yes |
| a group, `R4 block`, of R4 and a rectangle on Dwgs.User | (114.6, 102) to (116.6, 103.6), 0.1 mm stroke | |
| GND via, inside only because of that rectangle | (115.6, 102.8) | yes |
| GND via in a corner of the hull KiCad 10.0.8's own outline cuts off | (114.4, 101) | yes |
| GND via in Channel 2's area | (137, 104) | |

The routed board's GND track from R3.2 to (108.5, 102.5) and its via leave the
area, as does Channel 1's IN track to R13.

`kicad-cli pcb drc` reports on it each footprint's library as not found, the
stubs, the VCC track and the lone vias as dangling, and nothing else.

## `repeat_layout_area_overlap_kicad10.kicad_pcb`

The area board with R9 at (106.5, 100) at 0°, beside R1, and R10 at (137, 103),
above Channel 2's GND via. Channel 3's areas hold nine of the items Channel 1
copies, and Channel 2's IN stub, which Channel 2's area takes first.

## `repeat_layout_area_nested_kicad10.kicad_pcb`

The area board with R4's group inside a group `R4 outer`, which also holds a
group `R4 side` of a second rectangle on Dwgs.User, (114.6, 104) to
(116.6, 105.2), and a GND via at (115.6, 104.6) that only this rectangle brings
inside Channel 1's area. The wider hull has a slanted edge from (114.45, 105.35)
to (109.1, 102.85), where KiCad 10.0.8's own outline would cut off Channel 1's
MID track to (112, 103.1) and its arc to R4, on its own net.

## `repeat_layout_area_merge_kicad10.kicad_pcb`

The area board with R7's and R8's pad 2 on VCC, so that Channel 1's GND becomes
VCC on Channel 2, where the VCC track's copy keeps VCC, its own net.

## `repeat_layout_area_text_kicad10.kicad_pcb`

The area board with a text, `R4 block` on Dwgs.User at (115.6, 104.2), added
to R4's group. A saved board's text has no box without KiCad's fonts.

## `repeat_layout_area_kicad10.boxes.tsv`

The `oracle` step's `board.boxes` for the area board, one line each: for every
footprint, track, arc, via and drawing, the box `GetBoundingBox` answers in
`BBM_ITEM_ONLY` mode, which is `FOOTPRINT::GetBoundingBox( false )` for a
footprint and the item's own `GetBoundingBox()` otherwise
(`pcbnew/api/api_handler_pcb.cpp` 1330-1361 at 10.0.8, the same at 10.0.6):
uuid, x, y, width and height in nm.

## The areas and what they take

The `oracle` step builds each placement's area as `AutogenerateRuleAreas` does
(`pcbnew/tools/multichannel_tool.cpp` 2431-2473 at 10.0.8): KiCad's boxes of
its footprints, of the items in the groups above them, and of the tracks, arcs
and vias on nets every pad of which is on it, each 0.1 mm out; their convex
hull, by `BuildConvexHull`'s algorithm (`libs/kimath/src/geometry/convex_hull.cpp`
87-131); and that hull rectified by `KIGEOM::RectifyPolygon`'s steps
(`libs/kimath/src/geometry/shape_utils.cpp` 362-393) with KiCad's own
`SHAPE_LINE_CHAIN`, its `PointInside`, `Append`, `SetClosed` and `Simplify`.
The hull is closed first, so that `PointInside` answers and the corner outside
the hull is taken, as the function's header says it works (`shape_utils.h`
160-171). `buildRAOutline` hands it the hull open, on which `PointInside`
answers false; the step also builds that outline and lists, as `kicad_leaves`,
the copied items it leaves out: on the area board the via at (114.4, 101), on
the nested board that via, Channel 1's MID track and its arc.

An item is in an area when, as `enclosedByArea` tests it
(`pcbnew/pcbexpr_functions.cpp` 919-1032), its polygon on F.Cu, drawn to the
board's maximum error outside the copper, less the area is empty, with KiCad's
own `TransformShapeToPolygon` and `BooleanSubtract`. Each target's routing is
what lies in its own area or in the reference's area turned and moved onto it
(`newTargetOutline`, 1338-1341, which KiCad makes the target's area once it has
copied, 1929-1930), less what the reference copies and what an earlier target
takes. Each copy is pcbnew's `Duplicate`, `Rotate` and `Move`, on the net
`fixupNet` gives it (1941-1977): the net of the matched footprint's pad numbered
as a pad of the reference on the item's net, or its own.

Anchored on R1, Channel 1's area on the area board is, in nm,
(109100000, 99650000), (109100000, 99100000), (116750000, 99100000),
(116750000, 103750000), (111699999, 103750000), (111699999, 103703555),
(109100000, 103703555).

| Board | Copied from Channel 1 | Taken from Channel 2 (R5) | Taken from Channel 3 (R9) |
|---|---|---|---|
| routed | 7: MID and OUT | MID stub, OUT track, IN stub | none |
| area | 13: those, the three GND vias in the area, the GND track, the VCC track and the IN stub | those three and the GND via | none |
| overlap | the area board's 13 | the area board's four | the GND track and via that leave Channel 1's area |
| nested | the area board's 13 and the via the second rectangle brings in | the area board's four | none |

The copies the area board adds to the routed board's, onto R5 and R9, in nm;
the nested board adds a GND via at (144600000, 94400000) and at
(167712489, 119542893):

| Anchor | Item | Net | Layer | Points |
|---|---|---|---|---|
| R5 | via | GND | | (142800000, 94400000) |
| R5 | via | GND | | (141500000, 100500000) |
| R5 | via | GND | | (141000000, 95600000) |
| R5 | track | GND | B.Cu | (142500000, 100500000), (141500000, 100500000) |
| R5 | track | VCC | F.Cu | (141600000, 99000000), (142000000, 99000000) |
| R5 | track | /Channel 2/IN | F.Cu | (140000000, 100500000), (140800000, 100500000) |
| R9 | via | GND | | (166439697, 118270101) |
| R9 | via | GND | | (161207107, 121664214) |
| R9 | via | GND | | (164318377, 117845837) |
| R9 | track | GND | B.Cu | (161914214, 122371321), (161207107, 121664214) |
| R9 | track | VCC | F.Cu | (162338478, 120674264), (162621320, 120957107) |
| R9 | track | /Channel 3/IN | F.Cu | (160146447, 120603554), (160712132, 121169239) |

## The script

Run as `python3 make_repeat_layout_area.py STEP IN OUT`: `area` with the routed
board, then `overlap`, `nested`, `merge` and `text` with the area board, then `oracle`
with a board and a JSON file to write. A rerun of `area` or `nested` gives its
new items KiCad's UUIDs afresh.

```python
# KiCad lines cited at 10.0.8; each cited function is the same at 10.0.6.
import json
import sys

import pcbnew
from pcbnew import B_Cu, F_Cu, FromMM, VECTOR2I

STEP, IN, OUT = sys.argv[1], sys.argv[2], sys.argv[3]
ORIGIN = VECTOR2I(0, 0)
# buildRAOutline's margin in AutogenerateRuleAreas (multichannel_tool.cpp 2473).
MARGIN = 100000


def footprint(board, reference):
    found = [fp for fp in board.GetFootprints() if fp.GetReference() == reference]
    assert len(found) == 1, reference
    return found[0]


def mm(x, y):
    return VECTOR2I(FromMM(x), FromMM(y))


def area_board(path):
    """The routed board with routing on nets Channel 1 shares, a VCC track, a
    group around R4 and a rectangle, and a GND via in Channel 2's area."""
    board = pcbnew.LoadBoard(path)

    def net(name):
        found = board.FindNet(name)
        assert found is not None, name
        return found

    def track(name, layer, a, b):
        item = pcbnew.PCB_TRACK(board)
        item.SetStart(a)
        item.SetEnd(b)
        item.SetWidth(FromMM(0.2))
        item.SetLayer(layer)
        item.SetNet(net(name))
        board.Add(item)

    def via(name, at):
        item = pcbnew.PCB_VIA(board)
        item.SetPosition(at)
        item.SetLayerPair(F_Cu, B_Cu)
        item.SetWidth(F_Cu, FromMM(0.6))
        item.SetDrill(FromMM(0.3))
        item.SetNet(net(name))
        board.Add(item)

    r3_2 = footprint(board, "R3").FindPadByNumber("2").GetPosition()
    r1_1 = footprint(board, "R1").FindPadByNumber("1").GetPosition()
    # GND, which every channel shares: R3.2 down to a via inside Channel 1.
    ground = VECTOR2I(r3_2.x, FromMM(101.5))
    track("GND", B_Cu, r3_2, ground)
    via("GND", ground)
    # Channel 1's IN, which reaches R13 on the root sheet: a stub inside.
    track("/Channel 1/IN", F_Cu, r1_1, VECTOR2I(r1_1.x, FromMM(100.8)))
    # VCC, on no pad of Channel 1.
    track("VCC", F_Cu, mm(111, 101.6), mm(111, 102))
    # R4 grouped with a rectangle on Dwgs.User, and a GND via that only the
    # rectangle brings inside Channel 1's area.
    rect = pcbnew.PCB_SHAPE(board, pcbnew.SHAPE_T_RECTANGLE)
    rect.SetStart(mm(114.6, 102))
    rect.SetEnd(mm(116.6, 103.6))
    rect.SetLayer(pcbnew.Dwgs_User)
    rect.SetWidth(FromMM(0.1))
    board.Add(rect)
    group = pcbnew.PCB_GROUP(board)
    group.SetName("R4 block")
    board.Add(group)
    group.AddItem(footprint(board, "R4"))
    group.AddItem(rect)
    via("GND", mm(115.6, 102.8))
    # Inside the hull of Channel 1's area but in the corner its rectification
    # cuts away.
    via("GND", mm(114.4, 101.0))
    # A GND via inside Channel 2's area.
    via("GND", mm(137, 104))
    return board


def hull_of(points):
    """BuildConvexHull (libs/kimath/src/geometry/convex_hull.cpp 87-131)."""
    poly = sorted(points)
    n = len(poly)
    res = [None] * (2 * n)
    k = 0

    def cross(o, a, b):
        return (a[0] - o[0]) * (b[1] - o[1]) - (a[1] - o[1]) * (b[0] - o[0])

    for p in poly:
        while k >= 2 and cross(res[k - 2], res[k - 1], p) <= 0:
            k -= 1
        res[k] = p
        k += 1
    t = k + 1
    for i in range(n - 2, -1, -1):
        p = poly[i]
        while k >= t and cross(res[k - 2], res[k - 1], p) <= 0:
            k -= 1
        res[k] = p
        k += 1
    if k > 1 and res[0] == res[k - 1]:
        k -= 1
    return res[:k]


def rectify(points, closed=True):
    """KIGEOM::RectifyPolygon (libs/kimath/src/geometry/shape_utils.cpp
    362-393) with KiCad's own SHAPE_LINE_CHAIN. Closed, the hull answers
    PointInside, so the corner outside it is taken, as the function's header
    says (shape_utils.h 160-171). buildRAOutline hands it the hull open
    (closed=False), on which PointInside answers false."""
    hull = pcbnew.SHAPE_LINE_CHAIN()
    for x, y in points:
        hull.Append(VECTOR2I(x, y))
    hull.SetClosed(closed)
    out = pcbnew.SHAPE_LINE_CHAIN()

    def handle(a, b):
        p0 = VECTOR2I(a.x, b.y)
        out.Append(a)
        out.Append(VECTOR2I(b.x, a.y) if hull.PointInside(p0) else p0)

    for i in range(hull.SegmentCount()):
        seg = hull.CSegment(i)
        handle(seg.A, seg.B)
    if not hull.IsClosed() and hull.PointCount() >= 2:
        handle(hull.CLastPoint(), hull.CPoint(0))
    out.SetClosed(True)
    out.Simplify()
    return out


def chain_points(chain):
    return [[chain.CPoint(i).x, chain.CPoint(i).y] for i in range(chain.PointCount())]


def box(item):
    if item.GetClass() == "FOOTPRINT":
        return pcbnew.Cast_to_FOOTPRINT(item).GetBoundingBox(False)
    return item.GetBoundingBox()


def corners(bbox):
    b = pcbnew.BOX2I(bbox.GetPosition(), bbox.GetSize())
    b.Inflate(MARGIN)
    return [(b.GetLeft(), b.GetTop()), (b.GetRight(), b.GetTop()),
            (b.GetRight(), b.GetBottom()), (b.GetLeft(), b.GetBottom())]


def uuid(item):
    return item.m_Uuid.AsString()


def items_by_uuid(board):
    found = {}
    for collection in (board.GetFootprints(), board.GetTracks(), board.GetDrawings(), board.Zones()):
        for item in collection:
            found[uuid(item)] = item
    return found


def groups_of(board):
    """Each group's member uuids, by the group's uuid."""
    return {uuid(g): [uuid(m) for m in g.GetItems()] for g in board.Groups()}


def outline(board, members):
    """The outline AutogenerateRuleAreas builds for a sheet's footprints
    (multichannel_tool.cpp 2431-2473): their boxes, everything in the groups
    above them, and the tracks on nets whose every pad is theirs."""
    groups, items = groups_of(board), items_by_uuid(board)
    parent = {m: g for g, ms in groups.items() for m in ms}
    outline_items = {uuid(fp): fp for fp in members}
    above = set()
    for fp in members:
        g = parent.get(uuid(fp))
        while g is not None:
            above.add(g)
            g = parent.get(g)

    def collect(g):
        for m in groups[g]:
            if m in groups:
                collect(m)
            else:
                outline_items[m] = items[m]

    for g in above:
        collect(g)
    own = {p.GetNetCode() for fp in members for p in fp.Pads()}
    names = {uuid(fp) for fp in members}
    foreign = {p.GetNetCode() for fp in board.GetFootprints() if uuid(fp) not in names for p in fp.Pads()}
    for t in board.GetTracks():
        if t.GetNetCode() > 0 and t.GetNetCode() in own and t.GetNetCode() not in foreign:
            outline_items[uuid(t)] = t
    points = []
    for item in outline_items.values():
        points += corners(box(item))
    hull = hull_of(points)
    return rectify(hull), sorted(outline_items), rectify(hull, closed=False)


def enclosed(board, item, chain):
    """enclosedByArea (pcbnew/pcbexpr_functions.cpp 919-1032) on the default
    context layer, F.Cu (pcbexpr_evaluator.h 65)."""
    area = pcbnew.SHAPE_POLY_SET()
    area.AddOutline(chain)
    shape = pcbnew.SHAPE_POLY_SET()
    item.TransformShapeToPolygon(shape, F_Cu, 0, board.GetDesignSettings().m_MaxError,
                                 pcbnew.ERROR_OUTSIDE)
    if shape.IsEmpty():
        return False
    shape.ClearArcs()
    shape.BooleanSubtract(area)
    return shape.IsEmpty()


def item_record(board, item):
    kind = item.GetClass()
    record = {"kind": kind, "net": item.GetNetname(),
              "layer": board.GetLayerName(item.GetLayer()), "locked": item.IsLocked()}
    if kind == "PCB_VIA":
        cast = pcbnew.Cast_to_PCB_VIA(item)
        position = cast.GetPosition()
        record.update(at=[position.x, position.y], width=cast.GetWidth(F_Cu),
                      drill=cast.GetDrillValue())
    else:
        cast = pcbnew.Cast_to_PCB_ARC(item) if kind == "PCB_ARC" else pcbnew.Cast_to_PCB_TRACK(item)
        record.update(start=[cast.GetStart().x, cast.GetStart().y],
                      end=[cast.GetEnd().x, cast.GetEnd().y], width=cast.GetWidth())
        if kind == "PCB_ARC":
            record["mid"] = [cast.GetMid().x, cast.GetMid().y]
    return record


def oracle(path, anchor, targets):
    board = pcbnew.LoadBoard(path)
    by_path = {fp.GetPath().AsString(): fp for fp in board.GetFootprints()}
    ref_anchor = footprint(board, anchor)
    ref_sheet = ref_anchor.GetPath().AsString().rsplit("/", 1)[0]
    reference = [fp for p, fp in sorted(by_path.items()) if p.startswith(ref_sheet + "/")]
    ref_chain, ref_items, kicad_chain = outline(board, reference)
    ref_routing = [t for t in board.GetTracks() if enclosed(board, t, ref_chain)]
    copied = {uuid(t) for t in ref_routing}
    removed_already = set()
    # What the outline KiCad 10.0.8 draws, from the open chain, leaves out.
    kicad_leaves = [uuid(t) for t in ref_routing if not enclosed(board, t, kicad_chain)]
    result = {"outline": chain_points(ref_chain), "outline_items": ref_items,
              "kicad_outline": chain_points(kicad_chain), "kicad_leaves": sorted(kicad_leaves),
              "copied": sorted(copied), "targets": {}}
    for target_anchor_name in targets:
        target_anchor = footprint(board, target_anchor_name)
        degrees = target_anchor.GetOrientationDegrees() - ref_anchor.GetOrientationDegrees()
        rot = pcbnew.EDA_ANGLE(degrees, pcbnew.DEGREES_T)
        ref_anchor.Rotate(ORIGIN, rot)
        disp = target_anchor.GetPosition() - ref_anchor.GetPosition()
        ref_anchor.Rotate(ORIGIN, pcbnew.EDA_ANGLE(-degrees, pcbnew.DEGREES_T))
        target_sheet = target_anchor.GetPath().AsString().rsplit("/", 1)[0]
        matches = {}
        for ref_fp in reference:
            path_ = ref_fp.GetPath().AsString()
            matches[uuid(ref_fp)] = by_path[target_sheet + path_[len(ref_sheet):]]
        target_members = [fp for p, fp in sorted(by_path.items()) if p.startswith(target_sheet + "/")]
        own_chain, own_items, _ = outline(board, target_members)
        # newTargetOutline (multichannel_tool.cpp 1338-1341), which becomes the
        # target's area once KiCad has copied (1929-1930).
        carried = pcbnew.SHAPE_LINE_CHAIN(ref_chain)
        carried.Rotate(rot, ORIGIN)
        carried.Move(disp)
        removed, kept = [], []
        for t in board.GetTracks():
            key = uuid(t)
            if not (enclosed(board, t, own_chain) or enclosed(board, t, carried)):
                continue
            if key in copied or key in removed_already:
                kept.append(key)
                continue
            removed.append(key)
            removed_already.add(key)
        copies = []
        for item in ref_routing:
            copy = item.Duplicate().Cast()
            # fixupNet (multichannel_tool.cpp 1941-1977): the net of the matched
            # footprint's pad numbered as a pad of a matched reference footprint
            # on this net; the item's own net when there is none. Every such pad
            # is asked, so that a fixture whose pads disagree fails here.
            images = set()
            for ref_fp in reference:
                for p in ref_fp.Pads():
                    if p.GetNetCode() == item.GetNetCode():
                        images.add(matches[uuid(ref_fp)].FindPadByNumber(p.GetNumber()).GetNetname())
            assert len(images) <= 1, (item.GetNetname(), images)
            if images:
                copy.SetNet(board.FindNet(images.pop()))
            copy.Rotate(ORIGIN, rot)
            copy.Move(disp)
            copies.append(item_record(board, copy))
        placed = {}
        for ref_fp in reference:
            target = matches[uuid(ref_fp)]
            if target.GetReference() == target_anchor_name:
                continue
            target.SetLayerAndFlip(ref_fp.GetLayer())
            target.SetOrientation(ref_fp.GetOrientation())
            target.SetPosition(ref_fp.GetPosition())
            target.Rotate(ORIGIN, rot)
            target.Move(disp)
            position = target.GetPosition()
            placed[target.GetReference()] = [position.x, position.y,
                                             target.GetOrientationDegrees(),
                                             board.GetLayerName(target.GetLayer())]
        result["targets"][target_anchor_name] = {
            "outline": chain_points(own_chain), "outline_items": own_items,
            "carried": chain_points(carried), "removed": sorted(removed), "kept": sorted(kept),
            "copies": copies, "footprints": placed}
    return result


def boxes(path):
    """KiCad's own box for each footprint (without text), track, arc, via and
    drawing, as GetBoundingBox in BBM_ITEM_ONLY mode answers it
    (pcbnew/api/api_handler_pcb.cpp 1330-1361): [x, y, width, height]."""
    board = pcbnew.LoadBoard(path)
    out = {}
    for key, item in items_by_uuid(board).items():
        b = box(item)
        out[key] = [b.GetX(), b.GetY(), b.GetWidth(), b.GetHeight()]
    return {"boxes": out, "groups": groups_of(board)}


def overlap_board(path):
    """The area board with Channel 3 laid over the others: R9 at 0° beside R1,
    so that Channel 1's area carried onto it holds some of Channel 1's own
    routing, and R10 above Channel 2's GND via, so that Channel 3's area holds
    items of Channel 2's too."""
    board = pcbnew.LoadBoard(path)
    r9 = footprint(board, "R9")
    r9.SetOrientationDegrees(0)
    r9.SetPosition(mm(106.5, 100))
    footprint(board, "R10").SetPosition(mm(137, 103))
    return board


def text_board(path):
    """The area board with a text in R4's group."""
    board = pcbnew.LoadBoard(path)
    group = [g for g in board.Groups() if g.GetName() == "R4 block"][0]
    text = pcbnew.PCB_TEXT(board)
    text.SetText("R4 block")
    text.SetPosition(mm(115.6, 104.2))
    text.SetLayer(pcbnew.Dwgs_User)
    board.Add(text)
    group.AddItem(text)
    return board


def nested_board(path):
    """The area board with R4's group in a second group, which also holds a
    third group of a rectangle on Dwgs.User below the first, and a GND via
    that only this rectangle brings inside Channel 1's area."""
    board = pcbnew.LoadBoard(path)
    inner = [g for g in board.Groups() if g.GetName() == "R4 block"][0]
    rect = pcbnew.PCB_SHAPE(board, pcbnew.SHAPE_T_RECTANGLE)
    rect.SetStart(mm(114.6, 104))
    rect.SetEnd(mm(116.6, 105.2))
    rect.SetLayer(pcbnew.Dwgs_User)
    rect.SetWidth(FromMM(0.1))
    board.Add(rect)
    side = pcbnew.PCB_GROUP(board)
    side.SetName("R4 side")
    board.Add(side)
    side.AddItem(rect)
    outer = pcbnew.PCB_GROUP(board)
    outer.SetName("R4 outer")
    board.Add(outer)
    outer.AddItem(inner)
    outer.AddItem(side)
    via = pcbnew.PCB_VIA(board)
    via.SetPosition(mm(115.6, 104.6))
    via.SetLayerPair(F_Cu, B_Cu)
    via.SetWidth(F_Cu, FromMM(0.6))
    via.SetDrill(FromMM(0.3))
    via.SetNet(board.FindNet("GND"))
    board.Add(via)
    return board


def merge_board(path):
    """The area board with R7's and R8's pad 2 on VCC, so that Channel 1's GND
    becomes VCC on Channel 2, where the VCC track copied stays on VCC."""
    board = pcbnew.LoadBoard(path)
    vcc = board.FindNet("VCC")
    for reference in ("R7", "R8"):
        footprint(board, reference).FindPadByNumber("2").SetNet(vcc)
    return board


if STEP == "area":
    pcbnew.SaveBoard(OUT, area_board(IN))
elif STEP == "merge":
    pcbnew.SaveBoard(OUT, merge_board(IN))
elif STEP == "nested":
    pcbnew.SaveBoard(OUT, nested_board(IN))
elif STEP == "overlap":
    pcbnew.SaveBoard(OUT, overlap_board(IN))
elif STEP == "text":
    pcbnew.SaveBoard(OUT, text_board(IN))
elif STEP == "oracle":
    with open(OUT, "w") as handle:
        json.dump({"R1": oracle(IN, "R1", ("R5", "R9")), "board": boxes(IN)},
                  handle, indent=1, sort_keys=True)
```
