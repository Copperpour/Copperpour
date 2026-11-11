"""Generate a KiCad-authored D-shaped outline fixture with KiCad 10's pcbnew API."""
from pathlib import Path
import pcbnew

fixtures = Path(__file__).resolve().parent
board = pcbnew.LoadBoard(str(fixtures / 'specctra_two_resistors.kicad_pcb'))
for drawing in list(board.GetDrawings()):
    if drawing.GetLayer() == pcbnew.Edge_Cuts:
        board.Remove(drawing)

def point(x, y):
    return pcbnew.VECTOR2I(pcbnew.FromMM(x), pcbnew.FromMM(y))

def shape(kind):
    item = pcbnew.PCB_SHAPE(board, kind)
    item.SetLayer(pcbnew.Edge_Cuts)
    item.SetWidth(pcbnew.FromMM(0.05))
    return item

# One straight edge and one half circle (center (90, 50), radius 25 mm). Both
# resistors, at (100, 50) and (110, 50), stay inside the board.
line = shape(pcbnew.SHAPE_T_SEGMENT)
line.SetStart(point(90, 25))
line.SetEnd(point(90, 75))
board.Add(line)
arc = shape(pcbnew.SHAPE_T_ARC)
arc.SetArcGeometry(point(90, 75), point(115, 50), point(90, 25))
board.Add(arc)

output = fixtures / 'specctra_d_outline.kicad_pcb'
pcbnew.SaveBoard(str(output), board)
dsn = fixtures / 'specctra_d_outline.native-kicad-10.dsn'
assert pcbnew.ExportSpecctraDSN(pcbnew.LoadBoard(str(output)), str(dsn))
# Only normalize the export path; leave all native geometry intact.
source = dsn.read_text()
dsn.write_text('(pcb "board.dsn"' + source[source.index('\n'):])
