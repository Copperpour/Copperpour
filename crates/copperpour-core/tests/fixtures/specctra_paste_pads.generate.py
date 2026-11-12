"""Generate a KiCad-authored fixture with a stock footprint whose paste apertures are unnumbered pads, with KiCad 10's pcbnew API."""
from pathlib import Path
import re
import subprocess
import pcbnew

fixtures = Path(__file__).resolve().parent
out = fixtures / 'specctra_paste_pads.kicad_pcb'
dsn = fixtures / 'specctra_paste_pads.native-kicad-10.dsn'
board = pcbnew.LoadBoard(str(fixtures / 'specctra_two_resistors.kicad_pcb'))
nets = {name: board.FindNet(name) for name in ('VCC', 'GND')}


def add_dpak(reference, name, x_mm, y_mm):
    """A stock TO-252/TO-263 footprint: pins 1 and 3, tab 2, and unnumbered
    F.Paste-only aperture pads over the tab. Pin 1 on VCC, the tab on GND."""
    dpak = pcbnew.FootprintLoad('/usr/share/kicad/footprints/Package_TO_SOT_SMD.pretty', name)
    dpak.SetReference(reference)
    dpak.SetPosition(pcbnew.VECTOR2I_MM(x_mm, y_mm))
    board.Add(dpak)
    for pad in dpak.Pads():
        if pad.GetNumber() == '1':
            pad.SetNet(nets['VCC'])
        elif pad.GetNumber() == '2':
            pad.SetNet(nets['GND'])
    return dpak


# U1, a TO-252-2, on the front; U2, a TO-263-2, flipped to the bottom the
# way pcbnew's F key does, so its aperture pads are on B.Paste. Two
# footprints, not one twice: KiCad names a second image of the same
# footprint `<name>::1`, which Freerouting's session does not keep.
add_dpak('U1', 'TO-252-2', 120, 40)
u2 = add_dpak('U2', 'TO-263-2', 118, 60)
u2.Flip(u2.GetPosition(), pcbnew.FLIP_DIRECTION_LEFT_RIGHT)

pcbnew.SaveBoard(str(out), board)
subprocess.run(['kicad-cli', 'pcb', 'upgrade', '--force', str(out)], check=True)
board = pcbnew.LoadBoard(str(out))
assert pcbnew.ExportSpecctraDSN(board, str(dsn)), 'ExportSpecctraDSN failed'
text = dsn.read_text()
dsn.write_text(re.sub(r"^\(pcb \"[^\"\n]*\"\n", "(pcb board.dsn\n", text, count=1))
