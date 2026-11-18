# `repeat_layout` fixtures

Two boards with one sheet placed three times, written by pcbnew 10.0.6's
Python on Linux from `specctra_two_resistors.kicad_pcb`: its R1, a two-pad
`Resistor_SMD:R_0402`, is duplicated for every footprint, so nothing from
KiCad's libraries is in them. Only the `.kicad_pcb` files are committed; the
`.kicad_pro` and `.kicad_prl` that `SaveBoard` writes beside them are not used.
The source's title-block date is cleared.

## `repeat_layout_kicad10.kicad_pcb`

`channel.kicad_sch` is placed three times on the root sheet, as Channel 1 to
3, and places `stage.kicad_sch` once. Each channel has three footprints on its
own sheet and one on its stage, with that channel's nets:

| Symbol | Channel 1 | Channel 2 | Channel 3 |
|---|---|---|---|
| on `channel.kicad_sch` | R1 (110, 100) 0° | R5 (140, 100) 90° | R9 (160.5, 120.25) -30° |
| on `channel.kicad_sch` | R2 (112, 100) 90° | R6 (135, 110) 0° | R10 (155, 130) 0° |
| on `channel.kicad_sch`, B.Cu | R3 (110, 102.5) 180° | R7 (137, 110) 0° | R11 (157, 130) 0° |
| on `stage.kicad_sch` | R4 (113.25, 102.75) 45° | R8 (139, 110) 0° | R12 (159, 130) 0° |

Channel 1 is laid out. R5 and R9 are placed where Channels 2 and 3 belong,
and the rest of those channels sit in rows. R13 is on the root sheet and R14
has no symbol. Each footprint's `(path …)`, `(sheetname …)` and
`(sheetfile …)` are as the sync writes them; the sheet and symbol UUIDs are
the script's, every other UUID KiCad's.

## `repeat_layout_mismatch_kicad10.kicad_pcb`

The board above, with Channels 2 and 3 changed so that they no longer match
Channel 1: R7 flipped back to F.Cu, R8 removed, R6 renamed R2, pad 2 removed
from R10, R12's library identifier set to `Resistor_SMD:R_0603`, R15 added on
Channel 3's sheet with a symbol Channel 1 lacks, and R16 added on R11's path.

## The expected poses

The `oracle` step loads `repeat_layout_kicad10.kicad_pcb` and moves each
target footprint with the calls `MULTICHANNEL_TOOL::copyRuleAreaContents`
makes (`pcbnew/tools/multichannel_tool.cpp` 1262-1284 and 1700-1707 at
10.0.6): the anchors' rotation difference, the displacement that takes the
turned reference anchor onto the target anchor, then each footprint given its
counterpart's side, orientation and position, turned about the origin and
moved. It wrote, in nm and degrees:

| Anchor | Footprint | x | y | Orientation | Side |
|---|---|---|---|---|---|
| R1 | R6 | 140000000 | 98000000 | 180 | F.Cu |
| R1 | R7 | 142500000 | 100000000 | -90 | B.Cu |
| R1 | R8 | 142750000 | 96750000 | 135 | F.Cu |
| R1 | R10 | 162232051 | 121250000 | 60 | F.Cu |
| R1 | R11 | 159250000 | 122415064 | 150 | B.Cu |
| R1 | R12 | 161939583 | 124256570 | 15 | F.Cu |
| R5 | R2 | 100000000 | 95000000 | -90 | F.Cu |
| R5 | R3 | 100000000 | 97000000 | -90 | B.Cu |
| R5 | R4 | 100000000 | 99000000 | -90 | F.Cu |
| R9 | R2 | 110111861 | 111193747 | 30 | F.Cu |
| R9 | R3 | 111843911 | 110193747 | 30 | B.Cu |
| R9 | R4 | 113575962 | 109193747 | 30 | F.Cu |
| R9 | R6 | 151193747 | 99888139 | 120 | F.Cu |
| R9 | R7 | 150193747 | 98156089 | 120 | B.Cu |
| R9 | R8 | 149193747 | 96424038 | 120 | F.Cu |
| R5 | R10 | 154339746 | 110919873 | -120 | F.Cu |
| R5 | R11 | 153339746 | 112651923 | -120 | B.Cu |
| R5 | R12 | 152339746 | 114383974 | -120 | F.Cu |

R5's two rows are two runs: onto R1, then onto R9. The target anchors come
back where they are. Several of the 30° and 120° poses are 1 nm from where
turning each footprint's offset about the anchor would put them: KiCad turns
board coordinates about the origin and rounds each one before it moves them.

## The script

Run as `python3 make_repeat_layout.py specctra_two_resistors.kicad_pcb OUT
STEP`, with STEP `build`, then `oracle`, then `mismatch`. A rerun of `build`
gives KiCad's UUIDs and footprint order afresh; the oracle reads the committed
board.

```python
import json
import sys

import pcbnew
from pcbnew import B_Cu, F_Cu, FromMM, VECTOR2I

SOURCE, OUT = sys.argv[1], sys.argv[2]

# channel.kicad_sch, placed three times on the root sheet.
CHANNELS = {
    1: "6c3f2a1e-5b7d-4e0a-9f21-3d4c5b6a7e81",
    2: "a8e4b2c0-1d3f-4a5b-8c7d-9e0f1a2b3c42",
    3: "0f9e8d7c-6b5a-4f3e-a2d1-c0b9a8f7e623",
}
# stage.kicad_sch, placed once inside channel.kicad_sch.
STAGE = "e9b1d3f5-7a2c-4e6b-9d0f-2c4e6a8b0d46"
# channel.kicad_sch's symbols: (symbol, inside stage, pad 1 net, pad 2 net).
SYMBOLS = [
    ("2b7c9e14-0a3d-4f6b-8e1c-5d2a7f9b3c10", False, "IN", "MID"),
    ("7d1e3f5a-9b2c-4e8d-a6f0-1c3b5d7e9f22", False, "MID", "OUT"),
    ("c4a6e8f0-2b4d-4f6a-8c0e-3a5c7e9b1d34", False, "OUT", None),
    ("5f7a9c1e-3b5d-4f8a-a0c2-4e6b8d0f2a58", True, "MID", None),
]
ROOT_SYMBOL = "9a1c3e5b-7d9f-4b2d-8f4a-6c8e0b2d4f6a"
EXTRA_SYMBOL = "8b0d2f4a-6c8e-4a1c-b3e5-7a9c1e3f5b70"

# reference: (x mm, y mm, degrees, on B.Cu)
POSES = {
    "R1": (110, 100, 0, False),
    "R2": (112, 100, 90, False),
    "R3": (110, 102.5, 180, True),
    "R4": (113.25, 102.75, 45, False),
    "R5": (140, 100, 90, False),
    "R6": (135, 110, 0, False),
    "R7": (137, 110, 0, True),
    "R8": (139, 110, 0, False),
    "R9": (160.5, 120.25, -30, False),
    "R10": (155, 130, 0, False),
    "R11": (157, 130, 0, True),
    "R12": (159, 130, 0, False),
    "R13": (100, 90, 0, False),
    "R14": (100, 130, 0, False),
    "R15": (161, 130, 0, False),
}


def footprint(board, reference):
    found = [fp for fp in board.GetFootprints() if fp.GetReference() == reference]
    assert len(found) == 1, reference
    return found[0]


def build():
    board = pcbnew.LoadBoard(SOURCE)
    board.GetTitleBlock().SetDate("")
    template = footprint(board, "R1")
    originals = list(board.GetFootprints())
    nets = {}

    def net(name):
        if name not in nets:
            nets[name] = pcbnew.NETINFO_ITEM(board, name)
            board.Add(nets[name])
        return nets[name]

    def add(reference, path, sheetname, sheetfile, pad_nets):
        fp = pcbnew.Cast_to_FOOTPRINT(template.Duplicate(False))
        board.Add(fp)
        fp.SetReference(reference)
        fp.SetValue("10k")
        if path:
            fp.SetPath(pcbnew.KIID_PATH(path))
            fp.SetSheetname(sheetname)
            fp.SetSheetfile(sheetfile)
        for pad in fp.Pads():
            name = pad_nets.get(pad.GetNumber())
            if name:
                pad.SetNet(net(name))
        x, y, degrees, back = POSES[reference]
        if back:
            fp.SetLayerAndFlip(B_Cu)
        fp.SetPosition(VECTOR2I(FromMM(x), FromMM(y)))
        fp.SetOrientationDegrees(degrees)
        return fp

    number = 1
    for channel, sheet in CHANNELS.items():
        for symbol, staged, net1, net2 in SYMBOLS:
            prefix = f"/Channel {channel}/"
            pad_nets = {"1": prefix + net1, "2": prefix + net2 if net2 else "GND"}
            if staged:
                add(f"R{number}", f"/{sheet}/{STAGE}/{symbol}", prefix + "Stage/",
                    "stage.kicad_sch", pad_nets)
            else:
                add(f"R{number}", f"/{sheet}/{symbol}", prefix, "channel.kicad_sch", pad_nets)
            number += 1
    add("R13", f"/{ROOT_SYMBOL}", "/", "repeat_layout.kicad_sch",
        {"1": "/Channel 1/IN", "2": "GND"})
    add("R14", None, None, None, {})
    for fp in originals:
        board.Remove(fp)
    return board


def oracle(path, anchor, targets):
    """Each target footprint where KiCad's Repeat Layout puts it, by the calls
    MULTICHANNEL_TOOL::copyRuleAreaContents makes (multichannel_tool.cpp
    1262-1284 and 1700-1707 at 10.0.6), anchored on `anchor`."""
    board = pcbnew.LoadBoard(path)
    by_path = {}
    for fp in board.GetFootprints():
        by_path[fp.GetPath().AsString()] = fp
    ref_anchor = footprint(board, anchor)
    origin = VECTOR2I(0, 0)
    expected = {}
    for target_anchor_name in targets:
        target_anchor = footprint(board, target_anchor_name)
        degrees = (target_anchor.GetOrientationDegrees()
                   - ref_anchor.GetOrientationDegrees())
        rot = pcbnew.EDA_ANGLE(degrees, pcbnew.DEGREES_T)
        ref_anchor.Rotate(origin, rot)
        oldpos = ref_anchor.GetPosition()
        disp = target_anchor.GetPosition() - oldpos
        ref_anchor.Rotate(origin, pcbnew.EDA_ANGLE(-degrees, pcbnew.DEGREES_T))
        ref_sheet = ref_anchor.GetPath().AsString().rsplit("/", 1)[0]
        target_sheet = target_anchor.GetPath().AsString().rsplit("/", 1)[0]
        for ref_path, ref_fp in sorted(by_path.items()):
            if not ref_path.startswith(ref_sheet + "/"):
                continue
            target = by_path[target_sheet + ref_path[len(ref_sheet):]]
            target.SetLayerAndFlip(ref_fp.GetLayer())
            target.SetOrientation(ref_fp.GetOrientation())
            target.SetPosition(ref_fp.GetPosition())
            target.Rotate(origin, rot)
            target.Move(disp)
            position = target.GetPosition()
            expected[target.GetReference()] = [
                position.x, position.y, target.GetOrientationDegrees(),
                board.GetLayerName(target.GetLayer()),
            ]
    return expected


def mismatch(path, out):
    board = pcbnew.LoadBoard(path)
    # Taken before any change: pcbnew's Python hands back untyped objects
    # from the board's list and from Duplicate once an item is removed.
    fps = {name: footprint(board, name) for name in ("R6", "R7", "R8", "R10", "R11", "R12")}
    extra = pcbnew.Cast_to_FOOTPRINT(fps["R12"].Duplicate(False))
    copy = pcbnew.Cast_to_FOOTPRINT(fps["R11"].Duplicate(False))
    board.Add(extra)
    board.Add(copy)
    # Channel 2: R7 back on F.Cu, R8 gone, R6 renamed to R2.
    r7 = fps["R7"]
    r7.SetLayerAndFlip(F_Cu)
    r7.SetPosition(VECTOR2I(FromMM(137), FromMM(110)))
    r7.SetOrientationDegrees(0)
    board.Remove(fps["R8"])
    fps["R6"].SetReference("R2")
    # Channel 3: R10 without pad 2, R12 named R_0603, R15 a symbol Channel 1
    # lacks, R16 a second footprint of R11's symbol.
    r10 = fps["R10"]
    r10.Remove([pad for pad in r10.Pads() if pad.GetNumber() == "2"][0])
    fps["R12"].SetFPID(pcbnew.LIB_ID("Resistor_SMD", "R_0603"))
    extra.SetReference("R15")
    extra.SetPath(pcbnew.KIID_PATH(f"/{CHANNELS[3]}/{EXTRA_SYMBOL}"))
    extra.SetSheetname("/Channel 3/")
    extra.SetSheetfile("channel.kicad_sch")
    x, y, degrees, _ = POSES["R15"]
    extra.SetPosition(VECTOR2I(FromMM(x), FromMM(y)))
    extra.SetOrientationDegrees(degrees)
    copy.SetReference("R16")
    copy.SetPosition(VECTOR2I(FromMM(157), FromMM(135)))
    pcbnew.SaveBoard(out, board)


STEP, MAIN = sys.argv[3], f"{OUT}/repeat_layout_kicad10.kicad_pcb"
if STEP == "build":
    pcbnew.SaveBoard(MAIN, build())
elif STEP == "oracle":
    with open(f"{OUT}/oracle.json", "w") as handle:
        json.dump({
            "R1": oracle(MAIN, "R1", ("R5", "R9")),
            "R5": oracle(MAIN, "R5", ("R1",)),
            "R9": oracle(MAIN, "R9", ("R1", "R5")),
            "R5-R9": oracle(MAIN, "R5", ("R9",)),
        }, handle, indent=1, sort_keys=True)
elif STEP == "mismatch":
    mismatch(MAIN, f"{OUT}/repeat_layout_mismatch_kicad10.kicad_pcb")
```
