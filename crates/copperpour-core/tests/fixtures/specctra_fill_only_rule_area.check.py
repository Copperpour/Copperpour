"""Check the fill-only rule-area fixture with KiCad 10's pcbnew API.

Imports the committed Freerouting session into the committed board with KiCad's own
SES importer, refills the zones, and checks that a track crosses the rule area while
the GND fill stays out of it. Writes the result board to the path given as argument,
for `kicad-cli pcb drc`.
"""
import sys
from pathlib import Path
import pcbnew

fixtures = Path(__file__).resolve().parent
board = pcbnew.LoadBoard(str(fixtures / 'specctra_fill_only_rule_area.kicad_pcb'))
ses = fixtures / 'specctra_fill_only_rule_area.native-kicad-10.freerouting-master.ses'
assert pcbnew.ImportSpecctraSES(board, str(ses))

area = next(zone for zone in board.Zones() if zone.GetIsRuleArea())
assert area.GetZoneName() == 'no_pour'
outline = area.Outline()

crossing = [
    track for track in board.GetTracks()
    if track.GetLayer() in (pcbnew.F_Cu, pcbnew.B_Cu)
    and outline.Collide(pcbnew.SEG(track.GetStart(), track.GetEnd()))
]
assert crossing, 'no routed copper crosses the fill-only rule area'

pcbnew.ZONE_FILLER(board).Fill(board.Zones())
gnd = next(zone for zone in board.Zones() if zone.GetZoneName() == 'gnd_pour')
fill = gnd.GetFilledPolysList(pcbnew.B_Cu)
assert fill.OutlineCount() > 0, 'the GND zone did not fill'
overlap = fill.CloneDropTriangulation()
overlap.BooleanIntersection(outline)
assert overlap.Area() == 0, 'the GND fill entered the fill-only rule area'

print(f'{len(crossing)} routed segment(s) cross the rule area; GND fill stays out of it')
if len(sys.argv) > 1:
    pcbnew.SaveBoard(sys.argv[1], board)
