#!/usr/bin/env python3
"""Regenerate original test fixtures with KiCad's pcbnew, not text edits.
Run with the system Python that includes pcbnew. No external design is opened.
"""
from pathlib import Path
import pcbnew

ROOT = Path(__file__).resolve().parent
LIB = ROOT / "outlines.pretty"
LIB.mkdir(exist_ok=True)

def xy(x, y):
    return pcbnew.VECTOR2I(pcbnew.FromMM(x), pcbnew.FromMM(y))

def graphic(fp, shape, start, end, layer):
    item = pcbnew.PCB_SHAPE(fp)
    item.SetShape(shape)
    item.SetStart(xy(*start))
    item.SetEnd(xy(*end))
    item.SetLayer(layer)
    item.SetWidth(pcbnew.FromMM(0.05))
    fp.Add(item)

def footprint(name, fab=True, unsupported=False):
    fp = pcbnew.FOOTPRINT(None)
    fp.SetFPID(pcbnew.LIB_ID("model-3d", name))
    fp.SetReference("REF**")
    graphic(fp, pcbnew.SHAPE_T_RECT, (1, 2), (5, 8), pcbnew.F_CrtYd)
    if fab:
        graphic(fp, pcbnew.SHAPE_T_RECT, (-1, -0.625), (1, 0.625), pcbnew.F_Fab)
    if unsupported:
        graphic(fp, pcbnew.SHAPE_T_CIRCLE, (0, 0), (1, 0), pcbnew.F_Fab)
    pcbnew.PCB_IO_MGR.FindPlugin(pcbnew.PCB_IO_MGR.KICAD_SEXP).FootprintSave(str(LIB), fp)

footprint("rectangle-fab")
footprint("rectangle-courtyard", fab=False)
footprint("unsupported-fab", fab=False, unsupported=True)
board = pcbnew.BOARD()
board.GetDesignSettings().SetBoardThickness(pcbnew.FromMM(1.6))
for a,b in [((0,0),(20,0)),((20,0),(20,20)),((20,20),(0,20)),((0,20),(0,0))]:
    line = pcbnew.PCB_SHAPE(board)
    line.SetShape(pcbnew.SHAPE_T_SEGMENT)
    line.SetStart(xy(*a)); line.SetEnd(xy(*b))
    line.SetLayer(pcbnew.Edge_Cuts); line.SetWidth(pcbnew.FromMM(0.05))
    board.Add(line)
fp = pcbnew.FOOTPRINT(board)
fp.SetReference("U1"); fp.SetValue("Generated package")
fp.SetAttributes(pcbnew.FP_SMD); fp.SetPosition(xy(10,10))
model = pcbnew.FP_3DMODEL()
model.m_Filename = "${KIPRJMOD}/generated.step"
fp.Models().push_back(model)
board.Add(fp)
pcbnew.SaveBoard(str(ROOT / "assembly.kicad_pcb"), board, True)
print("Generated fixtures with KiCad", pcbnew.Version())
