"""Generate a KiCad-authored board with footprints at negative angles, with KiCad 10's pcbnew API."""
from pathlib import Path
import pcbnew

fixtures = Path(__file__).resolve().parent
board = pcbnew.LoadBoard(str(fixtures / 'specctra_two_resistors.kicad_pcb'))
footprints = {fp.GetReference(): fp for fp in board.GetFootprints()}
nets = {name: board.FindNet(name) for name in ('VCC', 'GND')}


def clone(reference, x, y, degrees, pad_nets):
    fp = pcbnew.Cast_to_FOOTPRINT(footprints['R2'].Duplicate(False))
    board.Add(fp)
    fp.SetReference(reference)
    fp.SetPosition(pcbnew.VECTOR2I_MM(x, y))
    fp.SetOrientationDegrees(degrees)
    for pad in fp.Pads():
        pad.SetNet(nets[pad_nets[pad.GetNumber()]])


# R1 at -90 degrees, R2 unchanged at 0, R3 at -45 and R4 at -180 (KiCad
# keeps angles in (-180, 180] and saves -180 as 180).
footprints['R1'].SetOrientationDegrees(-90)
clone('R3', 100, 40, -45, {'1': 'VCC', '2': 'GND'})
clone('R4', 110, 40, -180, {'1': 'GND', '2': 'VCC'})

output = fixtures / 'specctra_negative_rotation.kicad_pcb'
pcbnew.SaveBoard(str(output), board)
