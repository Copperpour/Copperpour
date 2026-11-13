"""Generate #72's native-only circular Edge.Cuts fixture using KiCad 10 pcbnew.

Run with /usr/bin/python3 and KiCad 10’s pcbnew module. Design changes use
the native object API, never serialized-text patches.
"""
from pathlib import Path
import pcbnew

fixtures = Path(__file__).resolve().parent
board = pcbnew.LoadBoard(str(fixtures / 'specctra_two_resistors.kicad_pcb'))
hole = pcbnew.PCB_SHAPE(board, pcbnew.SHAPE_T_CIRCLE)
hole.SetCenter(pcbnew.VECTOR2I_MM(120, 60))
hole.SetEnd(pcbnew.VECTOR2I_MM(122, 60))
hole.SetLayer(pcbnew.Edge_Cuts)
hole.SetWidth(pcbnew.FromMM(0.05))
board.Add(hole)
out = fixtures / 'specctra_native_circle.kicad_pcb'
pcbnew.SaveBoard(str(out), board)
native = pcbnew.LoadBoard(str(out))
dsn = fixtures / 'specctra_native_circle.native-kicad-10.dsn'
assert pcbnew.ExportSpecctraDSN(native, str(dsn))
# Normalize only the exported DSN's environment-dependent design name.
text = dsn.read_text()
dsn.write_text('(pcb "board.dsn"' + text[text.index('\n'):])
