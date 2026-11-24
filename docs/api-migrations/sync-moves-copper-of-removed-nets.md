### `update_pcb_from_schematic` moves the vias and zones of a net that leaves the schematic (minor release)

An acknowledged routed pad net change (`acknowledged_net_changes`) used to
move only the pads and the tracks KiCad's commit carried with them. Vias and
copper zones that reach the pads only through a zone stayed on the old net,
even when that net was no longer in the schematic.

The plan now moves each via and copper zone whose net no schematic pin
carries, as KiCad's Update PCB does (`updateCopperZoneNets`):

- A zone takes the new net of a pad connected to it before the update. If no
  such pad changes net, it takes the net its old net's pads move to. The pads
  connected to a zone are read from KiCad 10.0.1+; with an older KiCad the
  second rule alone applies and a `zone_pads_not_read` note says so.
- A via takes the net its old net's pads move to.
- Each move is listed in the new `copper_net_changes`
  (`kind`: `via` or `zone`, `kiid`, `name` for a named zone, `from`, `to`),
  counted in `coverage.copper_net_changes`, and part of the plan revision. The
  apply sends the moves in the sync's commit, reads the board back, and
  reports one KiCad left elsewhere as `board_readback_differs` with its
  `kiid`, not counted in `coverage.copper_net_changes.applied`. A readback
  that cannot finish is an `apply_readback_unchecked` note on the `applied`
  response, which is then an error.
- A via or zone on such a net that no planned pad leads off stays, with a
  `via_net_not_in_schematic` or `zone_net_not_in_schematic` note naming it
  (`kiid`). These notes refuse nothing and can appear on any plan. An item
  on no net is left alone without a note.

No second acknowledgement is needed: the moves follow from the acknowledged
pad changes, and the dry run shows them. Per-net `routed_copper` counts are
unchanged; with the vias and zones following their net, a complete move
shows in them again. Diagnostics gain an optional `kiid`, present only on the
notes above.
