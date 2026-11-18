### `update_pcb_from_schematic` applies a routed pad net change the caller acknowledges (minor release)

A pad net change on a net that routed copper uses, as a renamed or split net
makes on a routed board, was a `routed_pad_net_change` conflict with no way
past it. A caller can now acknowledge each one:

- Each `routed_pad_net_change` diagnostic carries
  `net_change: { "pad", "from", "to" }`. The key is absent from every other
  diagnostic but the note below.
- `acknowledged_net_changes: [{ "reference", "pad", "from", "to" }]` names
  the changes to plan anyway, each copied from a diagnostic. A named change
  is planned as any other pad net change: the pad takes its new net, and
  Copperpour sends no copper. KiCad's commit then moves a track or via onto
  the new net when every pad it joins is on it, as after its own Update PCB,
  and leaves the rest. A `routed_pad_net_change_acknowledged` diagnostic with
  the same `net_change` notes it and refuses nothing.
- A named change the plan does not make is a
  `net_change_acknowledgement_unmatched` conflict. A routed change left
  unnamed, and every other conflict, still refuses the plan.
- The acknowledgement is part of the plan: dry run with it, then apply with
  the same acknowledgements and that dry run's `plan_revision`. The
  conflict's own revision is stale for the apply.
- The apply's response adds
  `routed_copper: [{ "net", "before_count", "after_count" }]`, each named
  net's tracks, arcs, vias and copper zones before the commit and after, as
  KiCad reads them back, and `drc`, kicad-cli's DRC of the board KiCad then
  holds under the project's rules, with the keys `apply_specctra_ses`
  reports. The board file is not saved. Both keys are absent from every other
  response.
- If the readback or the DRC fails after the commit, or a copper zone reads
  back with no net it can be counted on, the response is still `applied`, is
  an error, and carries `acknowledged_apply_unchecked`: the plan must not be
  applied again.

Without `acknowledged_net_changes` the tool behaves as before, but for
`net_change` on `routed_pad_net_change`.
