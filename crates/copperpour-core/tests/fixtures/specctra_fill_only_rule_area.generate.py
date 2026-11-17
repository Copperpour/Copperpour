"""Generate a KiCad-authored four-layer fixture with a fill-only rule area, with KiCad 10's pcbnew API.

The rule area allows tracks and vias and forbids only copper fills. KiCad 10.0.8's
Specctra exporter still writes it as a full `keepout` on each layer (KiCad#25959).
"""
from pathlib import Path
import pcbnew

fixtures = Path(__file__).resolve().parent
board = pcbnew.LoadBoard(str(fixtures / 'specctra_four_layers.kicad_pcb'))


def rectangle(item, corners):
    outline = item.Outline()
    idx = outline.NewOutline()
    for x, y in corners:
        outline.Append(pcbnew.FromMM(x), pcbnew.FromMM(y), idx)


# GND copper zone on B.Cu, inside the 80..130 x 30..70 mm outline.
gnd = pcbnew.ZONE(board)
gnd.SetLayer(pcbnew.B_Cu)
gnd.SetNetCode(board.GetNetcodeFromNetname('GND'))
gnd.SetZoneName('gnd_pour')
rectangle(gnd, [(82, 32), (128, 32), (128, 68), (82, 68)])
board.Add(gnd)

# Fill-only rule area on F.Cu and B.Cu across the direct path from R2.2 (110.5, 50)
# to J1.1 (120, 45): tracks, vias, pads and footprints allowed, copper fills not.
area = pcbnew.ZONE(board)
area.SetIsRuleArea(True)
layers = pcbnew.LSET()
layers.AddLayer(pcbnew.F_Cu)
layers.AddLayer(pcbnew.B_Cu)
area.SetLayerSet(layers)
area.SetZoneName('no_pour')
area.SetDoNotAllowTracks(False)
area.SetDoNotAllowVias(False)
area.SetDoNotAllowPads(False)
area.SetDoNotAllowZoneFills(True)
area.SetDoNotAllowFootprints(False)
rectangle(area, [(112, 42), (118, 42), (118, 53), (112, 53)])
board.Add(area)

output = fixtures / 'specctra_fill_only_rule_area.kicad_pcb'
pcbnew.SaveBoard(str(output), board)
native = fixtures / 'specctra_fill_only_rule_area.native-kicad-10.dsn'
assert pcbnew.ExportSpecctraDSN(pcbnew.LoadBoard(str(output)), str(native))
text = native.read_text()
_, rest = text.split('\n', 1)
native.write_text('(pcb board.dsn\n' + rest)
