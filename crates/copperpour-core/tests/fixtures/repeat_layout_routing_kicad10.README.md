# `repeat_layout` routing fixtures

Two boards written by pcbnew 10.0.6's Python on Linux from
`repeat_layout_kicad10.kicad_pcb`, whose README says how it was made, so
nothing from KiCad's libraries is in them. Only the `.kicad_pcb` files are
committed.

## `repeat_layout_routing_kicad10.kicad_pcb`

That board with R9 turned from -30° to 45°, so that Channel 3 takes a turn off
the right angles, and routing added: 0.2 mm tracks, and vias of 0.6 mm with a
0.3 mm drill.

| Net | Items | Of the placement's own nets |
|---|---|---|
| Channel 1's MID | a locked track R1.2 to R2.1; a track from R2.1 and an arc to R4.1, on F.Cu | yes |
| Channel 1's OUT | a track from R2.2 to a via; two tracks on B.Cu to R3.1 | yes |
| Channel 1's IN | a track R1.1 to R13.1, R13 on the root sheet | no |
| GND | a track on B.Cu from R3.2 to a via | no |
| Channel 2's MID | a 1.5 mm stub from R6.1 | yes |
| Channel 2's OUT | a track (139.3, 98) to (140.7, 98), where R6 is to go | yes |
| Channel 2's IN | a 0.5 mm stub from R5.1 | yes, but Channel 1's IN is not |

`kicad-cli pcb drc` reports on it the three stubs and the GND via as dangling
and each footprint's library as not found, and nothing else.

## `repeat_layout_routing_mismatch_kicad10.kicad_pcb`

The routed board changed so that Channel 1's nets do not each become one net of
the target's own. On Channel 2, R8's pad 1 is on OUT, so Channel 1's MID
becomes two nets there; and R17, a copy of R13 at (100, 140) on a root symbol
of its own, has pad 1 on Channel 2's OUT, so that net has a pad off Channel 2.
On Channel 3, R10's pad 2 and R11's pad 1 are on MID, so Channel 1's MID and
OUT both become MID there.

## The expected copies

The `oracle` step loads the routed board and lays out R1's placement onto R5's,
then R9's. It takes the anchors' rotation difference and the displacement as
the footprint fixtures' oracle does. It selects each item on a net of Channel
1's own, a net with a pad on Channel 1 and none elsewhere, the rule
`AutogenerateRuleAreas` uses for a channel's nets
(`pcbnew/tools/multichannel_tool.cpp` 2396-2414 at 10.0.6); KiCad's Repeat
Layout itself copies what lies in a rule area. For each, it makes the copy
`copyRuleAreaContents` makes (1448-1475): pcbnew's `Duplicate`; the net of the
matched footprint's pad numbered as a reference pad on that net, which the
script finds as `fixupNet` does (1885-1921); pcbnew's `Rotate` about the
origin, then `Move`. It wrote, in nm:

| Anchor | Item | Net | Layer | Points | Locked |
|---|---|---|---|---|---|
| R5 | track | /Channel 2/MID | F.Cu | (140000000, 99500000), (140500000, 98000000) | yes |
| R5 | track | /Channel 2/MID | F.Cu | (140500000, 98000000), (143103553, 98000000) |  |
| R5 | arc | /Channel 2/MID | F.Cu | (143103553, 98000000), (143403553, 97551777), (143103553, 97103553) |  |
| R5 | track | /Channel 2/OUT | F.Cu | (139500000, 98000000), (139500000, 96800000) |  |
| R5 | via | /Channel 2/OUT | F.Cu | (139500000, 96800000) |  |
| R5 | track | /Channel 2/OUT | B.Cu | (139500000, 96800000), (142500000, 96800000) |  |
| R5 | track | /Channel 2/OUT | B.Cu | (142500000, 96800000), (142500000, 99500000) |  |
| R9 | track | /Channel 3/MID | F.Cu | (160853553, 119896447), (162267767, 119189340) | yes |
| R9 | track | /Channel 3/MID | F.Cu | (162267767, 119189340), (164108757, 121030330) |  |
| R9 | arc | /Channel 3/MID | F.Cu | (164108757, 121030330), (164637831, 120925521), (164742641, 120396446) |  |
| R9 | track | /Channel 3/OUT | F.Cu | (161560660, 118482233), (162409188, 117633705) |  |
| R9 | via | /Channel 3/OUT | F.Cu | (162409188, 117633705) |  |
| R9 | track | /Channel 3/OUT | B.Cu | (162409188, 117633705), (164530509, 119755025) |  |
| R9 | track | /Channel 3/OUT | B.Cu | (164530509, 119755025), (162621320, 121664214) |  |

Each track and arc keeps its 0.2 mm, each via its 0.6 mm. Channel 2's
footprints land as the footprint fixtures' table gives them; Channel 3's, with
R9 at 45°, on F.Cu unless noted: R10 (161914214, 118835787) 135°, R11
(162267767, 122017767) -135° on B.Cu, R12 (164742641, 119896447) 90°.

The same step's `rotate_45` turns two points by 45° with `PCB_TRACK::Rotate`, which is
`RotatePoint` (`libs/kimath/src/trigo.cpp` 229-264): (150000000, 111386035) to
(184827838, -27304197) and (150000001, 134005573) to (200822267, -11309769).
With the sine of 45° in radians for `EDA_ANGLE`'s `M_SQRT1_2`, each `y` comes
out 1 nm higher.

## The script

Run as `python3 make_repeat_layout_routing.py repeat_layout_kicad10.kicad_pcb
OUT STEP`, with STEP `route`, then `oracle`, then `mismatch`. A rerun of
`route` gives KiCad's UUIDs afresh; the oracle reads the board in `OUT`.

```python
import json
import sys

import pcbnew
from pcbnew import B_Cu, F_Cu, FromMM, VECTOR2I

SOURCE, OUT, STEP = sys.argv[1], sys.argv[2], sys.argv[3]
ROUTED = f"{OUT}/repeat_layout_routing_kicad10.kicad_pcb"
ORIGIN = VECTOR2I(0, 0)
# A root symbol for R17, the mismatch board's footprint off Channel 3.
EXTRA_ROOT_SYMBOL = "3e5a7c9f-1b2d-4f6a-8c0e-2d4f6a8c0e19"


def footprint(board, reference):
    found = [fp for fp in board.GetFootprints() if fp.GetReference() == reference]
    assert len(found) == 1, reference
    return found[0]


def pad(board, reference, number):
    return footprint(board, reference).FindPadByNumber(number).GetPosition()


def mm(x, y):
    return VECTOR2I(FromMM(x), FromMM(y))


def route():
    board = pcbnew.LoadBoard(SOURCE)
    # Channel 3's anchor turned to 45 degrees, so that placement takes a turn
    # off the right angles.
    footprint(board, "R9").SetOrientationDegrees(45)

    def net(name):
        found = board.FindNet(name)
        assert found is not None, name
        return found

    def track(name, layer, a, b, locked=False):
        item = pcbnew.PCB_TRACK(board)
        item.SetStart(a)
        item.SetEnd(b)
        item.SetWidth(FromMM(0.2))
        item.SetLayer(layer)
        item.SetNet(net(name))
        item.SetLocked(locked)
        board.Add(item)

    def arc(name, layer, a, mid, b):
        item = pcbnew.PCB_ARC(board)
        item.SetStart(a)
        item.SetMid(mid)
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

    # Channel 1, the reference: MID and OUT are its own nets; IN reaches R13
    # on the root sheet and GND every channel, so neither is.
    r1_2, r2_1, r2_2 = pad(board, "R1", "2"), pad(board, "R2", "1"), pad(board, "R2", "2")
    r3_1, r3_2, r4_1 = pad(board, "R3", "1"), pad(board, "R3", "2"), pad(board, "R4", "1")
    track("/Channel 1/MID", F_Cu, r1_2, r2_1, locked=True)
    bend = VECTOR2I(r2_1.x, r4_1.y)
    track("/Channel 1/MID", F_Cu, r2_1, bend)
    arc("/Channel 1/MID", F_Cu, bend, VECTOR2I((bend.x + r4_1.x) // 2, bend.y + FromMM(0.3)), r4_1)
    drop = VECTOR2I(r2_2.x + FromMM(1.2), r2_2.y)
    track("/Channel 1/OUT", F_Cu, r2_2, drop)
    via("/Channel 1/OUT", drop)
    track("/Channel 1/OUT", B_Cu, drop, VECTOR2I(drop.x, r3_1.y))
    track("/Channel 1/OUT", B_Cu, VECTOR2I(drop.x, r3_1.y), r3_1)
    track("/Channel 1/IN", F_Cu, pad(board, "R1", "1"), pad(board, "R13", "1"))
    ground = VECTOR2I(r3_2.x - FromMM(1.0), r3_2.y)
    track("GND", B_Cu, r3_2, ground)
    via("GND", ground)

    # Channel 2, a target, routed where a sync left it: a stub on MID from R6,
    # OUT under where R6 is to go, and a stub on IN, its own net but Channel
    # 1's IN's counterpart, which is not copied.
    r6_1 = pad(board, "R6", "1")
    track("/Channel 2/MID", F_Cu, r6_1, VECTOR2I(r6_1.x, r6_1.y + FromMM(1.5)))
    track("/Channel 2/OUT", F_Cu, mm(139.3, 98), mm(140.7, 98))
    r5_1, r5 = pad(board, "R5", "1"), footprint(board, "R5").GetPosition()
    track("/Channel 2/IN", F_Cu, r5_1, VECTOR2I(2 * r5_1.x - r5.x, 2 * r5_1.y - r5.y))
    return board


def item_record(board, item):
    kind = item.GetClass()
    record = {
        "kind": kind,
        "net": item.GetNetname(),
        "layer": board.GetLayerName(item.GetLayer()),
        "locked": item.IsLocked(),
    }
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


def local_nets(board, members):
    """KiCad's nets local to a channel (multichannel_tool.cpp 2396-2414)."""
    inside, outside = set(), set()
    for fp in board.GetFootprints():
        nets = inside if fp in members else outside
        for p in fp.Pads():
            if p.GetNetCode() > 0:
                nets.add(p.GetNetname())
    return inside - outside


def oracle(path, anchor, targets):
    """Each target's footprints and routing where KiCad's Repeat Layout puts
    them, by the calls MULTICHANNEL_TOOL::copyRuleAreaContents makes
    (multichannel_tool.cpp 1262-1284, 1448-1475 and 1700-1707 at 10.0.6), for
    the routing on nets local to the reference placement."""
    board = pcbnew.LoadBoard(path)
    by_path = {fp.GetPath().AsString(): fp for fp in board.GetFootprints()}
    ref_anchor = footprint(board, anchor)
    ref_sheet = ref_anchor.GetPath().AsString().rsplit("/", 1)[0]
    reference = [fp for p, fp in sorted(by_path.items()) if p.startswith(ref_sheet + "/")]
    ref_nets = local_nets(board, reference)
    ref_routing = [t for t in board.GetTracks() if t.GetNetname() in ref_nets]
    expected = {}
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
            path = ref_fp.GetPath().AsString()
            matches[ref_fp.GetReference()] = by_path[target_sheet + path[len(ref_sheet):]]
        copies = []
        for item in ref_routing:
            copy = item.Duplicate().Cast()
            # fixupNet (multichannel_tool.cpp 1885-1921): the net of the
            # matched footprint's pad numbered as a reference pad on this net.
            for ref_fp in reference:
                hit = [p for p in ref_fp.Pads() if p.GetNetname() == item.GetNetname()]
                if hit:
                    counterpart = matches[ref_fp.GetReference()].FindPadByNumber(hit[0].GetNumber())
                    copy.SetNet(counterpart.GetNet())
                    break
            copy.Rotate(ORIGIN, rot)
            copy.Move(disp)
            copies.append(item_record(board, copy))
        placed = {}
        for ref_fp in reference:
            target = matches[ref_fp.GetReference()]
            target.SetLayerAndFlip(ref_fp.GetLayer())
            target.SetOrientation(ref_fp.GetOrientation())
            target.SetPosition(ref_fp.GetPosition())
            target.Rotate(ORIGIN, rot)
            target.Move(disp)
            position = target.GetPosition()
            placed[target.GetReference()] = [position.x, position.y,
                                             target.GetOrientationDegrees(),
                                             board.GetLayerName(target.GetLayer())]
        expected[target_anchor_name] = {"footprints": placed, "routing": copies}
    return expected


def rotations():
    """PCB_TRACK::Rotate, which is RotatePoint (trigo.cpp 229-264), at points
    whose 45-degree turn EDA_ANGLE's M_SQRT1_2 rounds differently from the
    sine of the angle in radians."""
    out = {}
    for x, y in ((150000000, 111386035), (150000001, 134005573)):
        item = pcbnew.PCB_TRACK(None)
        item.SetStart(VECTOR2I(x, y))
        item.SetEnd(VECTOR2I(x, y))
        item.Rotate(ORIGIN, pcbnew.EDA_ANGLE(45.0, pcbnew.DEGREES_T))
        out[f"{x},{y}"] = [item.GetStart().x, item.GetStart().y]
    return out


def mismatch(path, out):
    board = pcbnew.LoadBoard(path)
    r13 = footprint(board, "R13")
    extra = pcbnew.Cast_to_FOOTPRINT(r13.Duplicate(False))
    board.Add(extra)
    # Channel 2: R8 pad 1 on OUT, so Channel 1's MID becomes two nets there,
    # and R17, on the root sheet, on OUT, so OUT has a pad off Channel 2.
    footprint(board, "R8").FindPadByNumber("1").SetNet(board.FindNet("/Channel 2/OUT"))
    extra.SetReference("R17")
    extra.SetPath(pcbnew.KIID_PATH(f"/{EXTRA_ROOT_SYMBOL}"))
    extra.SetPosition(VECTOR2I(FromMM(100), FromMM(140)))
    extra.FindPadByNumber("1").SetNet(board.FindNet("/Channel 2/OUT"))
    # Channel 3: R10 pad 2 and R11 pad 1 on MID, so Channel 1's MID and OUT
    # both become MID there.
    mid = board.FindNet("/Channel 3/MID")
    footprint(board, "R10").FindPadByNumber("2").SetNet(mid)
    footprint(board, "R11").FindPadByNumber("1").SetNet(mid)
    pcbnew.SaveBoard(out, board)


if STEP == "route":
    pcbnew.SaveBoard(ROUTED, route())
elif STEP == "oracle":
    with open(f"{OUT}/oracle_routing.json", "w") as handle:
        json.dump({"R1": oracle(ROUTED, "R1", ("R5", "R9")), "rotate_45": rotations()},
                  handle, indent=1, sort_keys=True)
elif STEP == "mismatch":
    mismatch(ROUTED, f"{OUT}/repeat_layout_routing_mismatch_kicad10.kicad_pcb")
```
