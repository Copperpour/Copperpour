"""Generate a KiCad-authored four-layer fixture with rule-area keep-outs, with KiCad 10's pcbnew API."""
from pathlib import Path
import pcbnew

fixtures = Path(__file__).resolve().parent
board = pcbnew.LoadBoard(str(fixtures / 'specctra_four_layers.kicad_pcb'))


def rule_area(layers, corners, name, *, tracks, vias, pads=False, copperpour=False, footprints=False):
    item = pcbnew.ZONE(board)
    item.SetIsRuleArea(True)
    layer_set = pcbnew.LSET()
    for layer in layers:
        layer_set.AddLayer(layer)
    item.SetLayerSet(layer_set)
    item.SetZoneName(name)
    item.SetDoNotAllowTracks(tracks)
    item.SetDoNotAllowVias(vias)
    item.SetDoNotAllowPads(pads)
    item.SetDoNotAllowZoneFills(copperpour)
    item.SetDoNotAllowFootprints(footprints)
    outline = item.Outline()
    idx = outline.NewOutline()
    for x, y in corners:
        outline.Append(pcbnew.FromMM(x), pcbnew.FromMM(y), idx)
    board.Add(item)


# Full keep-out on outer copper layers between R2 (110, 50) and J1 (120, 45/47.54).
rule_area(
    [pcbnew.F_Cu, pcbnew.B_Cu],
    [(113, 44), (117, 44), (117, 52), (113, 52)],
    'antenna_keepout',
    tracks=True,
    vias=True,
    pads=True,
    copperpour=True,
    footprints=True,
)
# Wire-only keep-out on F.Cu between the locked In1.Cu stub (105, 55) and R2.
rule_area(
    [pcbnew.F_Cu],
    [(106, 52), (109, 52), (109, 57), (106, 57)],
    'wire_block',
    tracks=True,
    vias=False,
)
# Via-only keep-out on outer copper layers near R1.
rule_area(
    [pcbnew.F_Cu, pcbnew.B_Cu],
    [(96, 44), (99, 44), (99, 48), (96, 48)],
    'via_block',
    tracks=False,
    vias=True,
)

output = fixtures / 'specctra_rule_areas.kicad_pcb'
pcbnew.SaveBoard(str(output), board)
native = fixtures / 'specctra_rule_areas.native-kicad-10.dsn'
assert pcbnew.ExportSpecctraDSN(pcbnew.LoadBoard(str(output)), str(native))
text = native.read_text()
_, rest = text.split('\n', 1)
native.write_text('(pcb board.dsn\n' + rest)
