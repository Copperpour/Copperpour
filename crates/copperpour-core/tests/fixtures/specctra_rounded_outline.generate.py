"""Generate KiCad-authored outline fixtures with KiCad 10.0.6's pcbnew API."""
import math
from pathlib import Path
import pcbnew

fixtures = Path(__file__).resolve().parent
board = pcbnew.LoadBoard(str(fixtures / 'specctra_two_resistors.kicad_pcb'))
for drawing in list(board.GetDrawings()):
    if drawing.GetLayer() == pcbnew.Edge_Cuts:
        board.Remove(drawing)

def point(x, y):
    return pcbnew.VECTOR2I(pcbnew.FromMM(x), pcbnew.FromMM(y))

def line(start, end):
    shape = pcbnew.PCB_SHAPE(board, pcbnew.SHAPE_T_SEGMENT)
    shape.SetStart(point(*start))
    shape.SetEnd(point(*end))
    shape.SetLayer(pcbnew.Edge_Cuts)
    shape.SetWidth(pcbnew.FromMM(0.05))
    board.Add(shape)

def arc(start, mid, end):
    shape = pcbnew.PCB_SHAPE(board, pcbnew.SHAPE_T_ARC)
    shape.SetArcGeometry(point(*start), point(*mid), point(*end))
    shape.SetLayer(pcbnew.Edge_Cuts)
    shape.SetWidth(pcbnew.FromMM(0.05))
    board.Add(shape)

k = 2 * (1 - math.sqrt(0.5))
notch = 5 * math.sqrt(0.5)
line((82, 30), (128, 30))
arc((128, 30), (130 - k, 30 + k), (130, 32))
line((130, 32), (130, 65))
arc((130, 65), (130 - notch, 70 - notch), (125, 70))
line((125, 70), (82, 70))
arc((82, 70), (80 + k, 70 - k), (80, 68))
line((80, 68), (80, 32))
arc((80, 32), (80 + k, 30 + k), (82, 30))
output = fixtures / 'specctra_rounded_outline.kicad_pcb'
pcbnew.SaveBoard(str(output), board)
dsn = fixtures / 'specctra_rounded_outline.native-kicad-10.dsn'
assert pcbnew.ExportSpecctraDSN(pcbnew.LoadBoard(str(output)), str(dsn))
# Only normalize the export path; leave all native geometry intact.
source = dsn.read_text()
dsn.write_text('(pcb "board.dsn"' + source[source.index('\n'):])
# A disjoint closed triangle whose smallest vertex precedes the outer boundary.
for start, end in [((70, 40), (75, 40)), ((75, 40), (72, 45)), ((72, 45), (70, 40))]:
    line(start, end)
pcbnew.SaveBoard(str(fixtures / 'specctra_multiple_outline_loops.kicad_pcb'), board)
