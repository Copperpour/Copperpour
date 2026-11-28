# Fill-only rule-area Specctra fixture

Generated with KiCad 10.0.8's native `pcbnew` Python API by
`specctra_fill_only_rule_area.generate.py`, starting from
`specctra_four_layers.kicad_pcb`. Run with `/usr/bin/python3` on a machine with
KiCad's Python bindings. The board is KiCad's serialization; only the native
DSN's root output path is normalized to `board.dsn`. UUIDs may differ on
regeneration.

The script adds two zones inside the 80..130 x 30..70 mm outline:

| Name | Kind | Layers | Outline (mm) |
|---|---|---|---|
| `gnd_pour` | copper zone, net `GND` | B.Cu | (82, 32) to (128, 68) |
| `no_pour` | rule area: `tracks`, `vias`, `pads`, `footprints` allowed, `copperpour` not allowed | F.Cu, B.Cu | (112, 42) to (118, 53) |

`no_pour` lies across the direct path from R2 pad 2 (110.5, 50) to J1 pad 1
(120, 45), both on `VCC`.

`specctra_fill_only_rule_area.native-kicad-10.dsn` is KiCad 10.0.8's
`pcbnew.ExportSpecctraDSN(board, path)` of that board. It writes `no_pour` as a
full `(keepout "" ...)` on F.Cu and on B.Cu, which would forbid routing inside
it (KiCad issue [#25959](https://gitlab.com/kicad/code/kicad/-/work_items/25959),
fixed for 10.0.9 by skipping such areas).

`specctra_fill_only_rule_area.native-kicad-10.freerouting-master.ses` is
Freerouting `master` (`12f085e6e`, 2.5.1-SNAPSHOT)'s session for that native
DSN with the two `no_pour` keep-outs removed, which is what `adopt_native`
sends: `--router.max_threads=1 --router.optimizer.max_threads=1
--router.automatic_neckdown=false`, run on `board.dsn`. Neckdown is off because
Freerouting otherwise narrows three short `VCC` segments to 0.12-0.15 mm, below the
board's 0.2 mm minimum track width. The session routes `VCC` across `no_pour`
on F.Cu, with 0 unrouted connections and 0 violations.

`specctra_fill_only_rule_area.check.py` is the KiCad oracle: it imports that
session with KiCad's own `ImportSpecctraSES`, checks that routed copper crosses
`no_pour`, refills the zones, and checks that the `gnd_pour` fill does not enter
`no_pour`. On KiCad 10.0.8:

```
$ /usr/bin/python3 specctra_fill_only_rule_area.check.py /tmp/routed.kicad_pcb
2 routed segment(s) cross the rule area; GND fill stays out of it
$ kicad-cli pcb drc --severity-all --format json -o /tmp/drc.json /tmp/routed.kicad_pcb
```

The DRC reports 0 unconnected items and no `items_not_allowed`; its only
violations are those of `specctra_four_layers.kicad_pcb` itself (2
`lib_footprint_issues`, and 1 `track_dangling` for the locked `GND` stub on
In1.Cu). This uses KiCad's SES importer, not Copperpour's `apply_specctra_ses`.
