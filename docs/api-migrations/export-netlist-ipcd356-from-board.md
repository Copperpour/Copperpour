### `export_netlist` exports IPC-D-356 from the board and KiCad netlists from the schematic (patch release)

`export_netlist` ran `kicad-cli sch export netlist` on the given file whatever
the format. That command does not load a `.kicad_pcb` and has no IPC-D-356
format, so both documented formats failed on a board
([#155](https://github.com/Copperpour/Copperpour/issues/155)).

- `format: "ipc"` (also `ipcd356`, `ipc-d-356`, `d356`) now runs
  `pcb export ipcd356` on the board. Given a `.kicad_sch` it is refused as
  `invalid_argument` on `board`.
- A schematic netlist format (`kicad`, `kicadxml`, `cadstar`, `orcadpcb2`,
  `spice`, `spicemodel`, `pads` or `allegro`, or the aliases `kicadsexpr`,
  `sexp`, `xml` and `orcad`) given a `.kicad_pcb` is exported from the
  `.kicad_sch` of the same name beside the board, and the response names that
  file in a new `source_schematic` field. When no such
  schematic exists the call is refused as `invalid_argument` on `board`
  before kicad-cli runs. A `.kicad_sch` path works as before.
- Any other format name is refused as `invalid_argument` on `format` before
  kicad-cli runs. It used to reach kicad-cli, which failed with
  `Invalid format`.
