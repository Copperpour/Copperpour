"""Generate #98's stock-footprint half-micrometre fixture using KiCad 10 pcbnew.

Starts from `specctra_stock_roundrect.kicad_pcb` (`R_0603_1608Metric`,
`C_0603_1608Metric`, `R_0402_1005Metric`) and adds KiCad's stock
`Resistor_SMD:R_0805_2012Metric` as `R3`, whose pads sit at +/-0.9125 mm
(+/-912.5 um) and whose courtyard exports as `(outline (polygon ...))`.
"""
from pathlib import Path
import re
import subprocess
import pcbnew

fixtures = Path(__file__).resolve().parent
out = fixtures / 'specctra_stock_half_um.kicad_pcb'
dsn = fixtures / 'specctra_stock_half_um.native-kicad-10.dsn'

board = pcbnew.LoadBoard(str(fixtures / 'specctra_stock_roundrect.kicad_pcb'))
nets = {name: board.FindNet(name) for name in ('VCC', 'GND')}

r3 = pcbnew.FootprintLoad(
    '/usr/share/kicad/footprints/Resistor_SMD.pretty',
    'R_0805_2012Metric',
)
r3.SetReference('R3')
r3.SetPosition(pcbnew.VECTOR2I_MM(115, 60))
board.Add(r3)
for pad in r3.Pads():
    pad.SetNet(nets['VCC'] if pad.GetNumber() == '1' else nets['GND'])

pcbnew.SaveBoard(str(out), board)
subprocess.run(['kicad-cli', 'pcb', 'upgrade', '--force', str(out)], check=True)
board = pcbnew.LoadBoard(str(out))
assert pcbnew.ExportSpecctraDSN(board, str(dsn)), 'ExportSpecctraDSN failed'
text = dsn.read_text()
dsn.write_text(re.sub(r'^\(pcb "[^"\n]*"\n', '(pcb board.dsn\n', text, count=1))
