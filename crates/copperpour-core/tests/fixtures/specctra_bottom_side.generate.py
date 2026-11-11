"""Generate a KiCad-authored four-layer fixture with bottom-side footprints, with KiCad 10's pcbnew API."""
from pathlib import Path
import re
import subprocess
import pcbnew

fixtures = Path(__file__).resolve().parent
out = fixtures / 'specctra_bottom_side.kicad_pcb'
dsn = fixtures / 'specctra_bottom_side.native-kicad-10.dsn'
board = pcbnew.LoadBoard(str(fixtures / 'specctra_four_layers.kicad_pcb'))
nets = {name: board.FindNet(name) for name in ('VCC', 'GND')}
footprints = {fp.GetReference(): fp for fp in board.GetFootprints()}

# R2 turned 90 degrees, then flipped to the bottom the way pcbnew's F key does.
r2 = footprints['R2']
r2.SetOrientationDegrees(90)
r2.Flip(r2.GetPosition(), pcbnew.FLIP_DIRECTION_LEFT_RIGHT)

# An asymmetric SOT-23W on the bottom at 30 degrees: a mirror error moves pin 3.
u1 = pcbnew.FootprintLoad('/usr/share/kicad/footprints/Package_TO_SOT_SMD.pretty', 'SOT-23W')
u1.SetReference('U1')
u1.SetPosition(pcbnew.VECTOR2I_MM(112, 56))
board.Add(u1)
for pad in u1.Pads():
    pad.SetNet(nets['GND'] if pad.GetNumber() == '2' else nets['VCC'])
u1.SetOrientationDegrees(30)
u1.Flip(u1.GetPosition(), pcbnew.FLIP_DIRECTION_LEFT_RIGHT)

# R3 at 45 degrees with pad 2 turned a quarter more, then flipped: KiCad
# writes that pin's turn mirrored on the bottom footprint's front image.
r3 = pcbnew.FootprintLoad('/usr/share/kicad/footprints/Resistor_SMD.pretty', 'R_0603_1608Metric')
r3.SetReference('R3')
r3.SetPosition(pcbnew.VECTOR2I_MM(104, 56))
board.Add(r3)
for pad in r3.Pads():
    pad.SetNet(nets['VCC'] if pad.GetNumber() == '1' else nets['GND'])
    if pad.GetNumber() == '2':
        pad.SetOrientationDegrees(90)
r3.SetOrientationDegrees(45)
r3.Flip(r3.GetPosition(), pcbnew.FLIP_DIRECTION_LEFT_RIGHT)

# The through-hole header J1 flipped the other way: its pads stay on every layer.
j1 = footprints['J1']
j1.Flip(j1.GetPosition(), pcbnew.FLIP_DIRECTION_TOP_BOTTOM)

pcbnew.SaveBoard(str(out), board)
subprocess.run(['kicad-cli', 'pcb', 'upgrade', '--force', str(out)], check=True)
board = pcbnew.LoadBoard(str(out))
assert pcbnew.ExportSpecctraDSN(board, str(dsn)), 'ExportSpecctraDSN failed'
text = dsn.read_text()
dsn.write_text(re.sub(r"^\(pcb \"[^\"\n]*\"\n", "(pcb board.dsn\n", text, count=1))
