### `update_pcb_from_schematic` sets each footprint's symbol-unit mapping (patch release)

The sync now gives each footprint it adds or updates its symbol's unit
mapping, the `(units …)` block that records which pins belong to each unit,
from the exported netlist, as KiCad's Update PCB does
([#682](https://github.com/Copperpour/Copperpour/issues/682)). KiCad's API
carries the mapping from 10.0.8.

- A footprint whose mapping is missing or differs from the netlist's is
  planned as an update. Footprints an earlier sync placed have none, so the
  next sync of such a board plans an update for each of them.
- Each planned `add` and `update` change has a `symbol_units` list, one
  `{name, pin_numbers}` per unit in the netlist's order.
- A footprint that owns groups, reference points or variants and differs only
  in its mapping is left out with a `footprint_sheet_not_updated` note, as one
  that differs only in its sheet is.

On KiCad before 10.0.8 the sync is as before: it neither compares nor sets the
mapping, `symbol_units` is empty, and each footprint it adds or updates gets a
`symbol_units_not_carried` note in `diagnostics`, which refuses nothing.
KiCad's Update PCB from Schematic sets the mapping there.
