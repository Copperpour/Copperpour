"""Generate #484's 4-layer partial-layer through-hole fixture using KiCad 10 pcbnew.

Run with /usr/bin/python3 and KiCad 10's pcbnew module. Design changes use
the native object API, never serialized-text patches.

KiCad 10.0.6's LoadBoard expands thru_hole pad layer sets back to *.Cu on
reload, while live in-memory pads modified via pad.SetLayerSet(...) retain
partial layer sets through SaveBoard and ExportSpecctraDSN. We therefore export
the native DSN directly from the in-memory board before reloading.
"""

from pathlib import Path
import shutil
import pcbnew

fixtures = Path(__file__).resolve().parent
shutil.copy2(
    fixtures / 'specctra_native_circle.kicad_pro',
    fixtures / 'specctra_native_partial_th.kicad_pro',
)

board = pcbnew.LoadBoard(str(fixtures / 'specctra_four_layers.kicad_pcb'))
for track in list(board.GetTracks()):
    board.Remove(track)

nets = {name: board.FindNet(name) for name in ('VCC', 'GND')}
footprints = {fp.GetReference(): fp for fp in board.GetFootprints()}


def set_copper_layers(pad, copper_layers):
    layer_set = pcbnew.LSET()
    for layer in copper_layers:
        layer_set.AddLayer(layer)
    for layer in (pcbnew.F_Mask, pcbnew.B_Mask):
        layer_set.AddLayer(layer)
    pad.SetLayerSet(layer_set)


j1 = footprints['J1']
for pad in j1.Pads():
    if pad.GetNumber() == '1':
        set_copper_layers(pad, [pcbnew.F_Cu, pcbnew.In1_Cu])
    elif pad.GetNumber() == '2':
        set_copper_layers(pad, [pcbnew.F_Cu, pcbnew.In2_Cu])

j2 = pcbnew.FootprintLoad(
    '/usr/share/kicad/footprints/Connector_PinHeader_2.54mm.pretty',
    'PinHeader_1x02_P2.54mm_Horizontal',
)
j2.SetReference('J2')
j2.SetPosition(pcbnew.VECTOR2I_MM(115, 58))
board.Add(j2)
j2.Flip(j2.GetPosition(), pcbnew.FLIP_DIRECTION_LEFT_RIGHT)
for pad in j2.Pads():
    if pad.GetNumber() == '1':
        pad.SetNet(nets['VCC'])
        set_copper_layers(pad, [pcbnew.B_Cu, pcbnew.In2_Cu])
    elif pad.GetNumber() == '2':
        pad.SetNet(nets['GND'])
        set_copper_layers(pad, [pcbnew.B_Cu, pcbnew.In1_Cu])

out = fixtures / 'specctra_native_partial_th.kicad_pcb'
pcbnew.SaveBoard(str(out), board)

dsn = fixtures / 'specctra_native_partial_th.native-kicad-10.dsn'
assert pcbnew.ExportSpecctraDSN(board, str(dsn))
text = dsn.read_text()
dsn.write_text('(pcb "board.dsn"' + text[text.index('\n'):])
