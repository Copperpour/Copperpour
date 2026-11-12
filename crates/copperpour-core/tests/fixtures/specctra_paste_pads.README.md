# Specctra fixture with paste-aperture pads

`specctra_paste_pads.kicad_pcb` is `specctra_two_resistors.kicad_pcb` with two
of KiCad's stock power footprints, made and saved by KiCad 10.0.8's `pcbnew`
API with `specctra_paste_pads.generate.py`, then re-saved with
`kicad-cli pcb upgrade --force`:

- `Package_TO_SOT_SMD:TO-252-2` added as `U1` at (120, 40) mm on the front;
- `Package_TO_SOT_SMD:TO-263-2` added as `U2` at (118, 60) mm, then flipped
  left/right (pcbnew's `F` key).

On each, pad 1 is on `VCC`, the tab (pad 2) on `GND` and pad 3 has no net.
Each also has four unnumbered pads over its tab on `F.Paste` only (`U2`:
`B.Paste` once flipped): the paste apertures, with no copper (#458). The two
footprints were chosen with whole-micrometre pads, which the native-DSN
reader requires (#841), and differ from each other: KiCad names a second
image of the same footprint `<name>::1`, and Freerouting's session gives
both components the first name.

`specctra_paste_pads.native-kicad-10.dsn` was exported from that board by
KiCad 10.0.8's `pcbnew.ExportSpecctraDSN(board, path)`. Only the absolute
output path in the root `(pcb ...)` identifier was normalized to `board.dsn`.
KiCad leaves the aperture pads out: each image has its three copper pins.

`specctra_paste_pads.native-kicad-10.freerouting-2.5.0.ses` is Freerouting
2.5.0's session for that native DSN (`-mp 2`): no connection unrouted, no
violation, two vias.
