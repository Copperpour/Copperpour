"""Generate a KiCad-authored four-layer fixture with copper zones, with KiCad 10's pcbnew API."""
from pathlib import Path
import pcbnew

fixtures = Path(__file__).resolve().parent
board = pcbnew.LoadBoard(str(fixtures / 'specctra_four_layers.kicad_pcb'))
nets = {name: board.FindNet(name) for name in ('VCC', 'GND')}


def zone(layers, net, corners, name):
    item = pcbnew.ZONE(board)
    layer_set = pcbnew.LSET()
    for layer in layers:
        layer_set.AddLayer(layer)
    item.SetLayerSet(layer_set)
    item.SetNet(nets[net])
    item.SetZoneName(name)
    outline = item.Outline()
    outline.NewOutline()
    for x, y in corners:
        outline.Append(pcbnew.FromMM(x), pcbnew.FromMM(y))
    board.Add(item)


# A GND plane on In1.Cu and a VCC plane on In2.Cu, inside the 80..130 x
# 30..70 mm outline; one GND pour on both outer layers.
zone([pcbnew.In1_Cu], 'GND', [(81, 31), (129, 31), (129, 69), (81, 69)], 'gnd_plane')
zone([pcbnew.In2_Cu], 'VCC', [(81, 31), (129, 31), (129, 69), (81, 69)], 'vcc_plane')
zone([pcbnew.F_Cu, pcbnew.B_Cu], 'GND', [(82, 60), (95, 60), (95, 68), (82, 68)], 'gnd_pour')
pcbnew.ZONE_FILLER(board).Fill(board.Zones())

output = fixtures / 'specctra_zones.kicad_pcb'
pcbnew.SaveBoard(str(output), board)
native = fixtures / 'specctra_zones.native-kicad-10.dsn'
assert pcbnew.ExportSpecctraDSN(pcbnew.LoadBoard(str(output)), str(native))
text = native.read_text()
first, rest = text.split('\n', 1)
native.write_text('(pcb board.dsn\n' + rest)
