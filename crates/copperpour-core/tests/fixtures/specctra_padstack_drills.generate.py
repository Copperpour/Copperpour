"""Generate a KiCad-authored board whose two connectors share a pad shape but not a drill, with KiCad 10's pcbnew API."""
from pathlib import Path
import pcbnew

fixtures = Path(__file__).resolve().parent
board = pcbnew.LoadBoard(str(fixtures / 'specctra_two_resistors.kicad_pcb'))
library = '/usr/share/kicad/footprints/Connector_PinHeader_2.54mm.pretty'


def header(reference, x, y, drill_mm):
    footprint = pcbnew.FootprintLoad(library, 'PinHeader_1x02_P2.54mm_Vertical')
    footprint.SetReference(reference)
    footprint.SetPosition(pcbnew.VECTOR2I(pcbnew.FromMM(x), pcbnew.FromMM(y)))
    for pad, net in zip(sorted(footprint.Pads(), key=lambda pad: pad.GetNumber()), ('VCC', 'GND')):
        # The stock pad is 1.7 mm with a 1.0 mm drill: keep the copper, change the hole.
        pad.SetDrillSize(pcbnew.VECTOR2I(pcbnew.FromMM(drill_mm), pcbnew.FromMM(drill_mm)))
        pad.SetNet(board.FindNet(net))
    board.Add(footprint)


# Same 1.7 mm copper, two drills: J1 keeps the stock 1.0 mm, J2 gets 0.8 mm.
header('J1', 120, 40, 1.0)
header('J2', 125, 40, 0.8)

output = fixtures / 'specctra_padstack_drills.kicad_pcb'
pcbnew.SaveBoard(str(output), board)
native = fixtures / 'specctra_padstack_drills.native-kicad-10.dsn'
assert pcbnew.ExportSpecctraDSN(pcbnew.LoadBoard(str(output)), str(native))
text = native.read_text()
first, rest = text.split('\n', 1)
native.write_text('(pcb board.dsn\n' + rest)
