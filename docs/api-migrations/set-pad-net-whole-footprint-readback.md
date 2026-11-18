### `set_pad_net` proves the whole footprint after the update (patch release)

`set_pad_net` sends the pad's whole footprint back to KiCad, as KiCad 10 requires for a pad. Its read-back used to check only that the footprint kept its KIID and item count and that the edited pads held the net. It now shares the footprint update path with `set_teardrop_settings` and compares every field of the footprint, each pad and each other child with what was sent and with what was there before. A footprint that comes back changed anywhere else is reported as `ipc_outcome_unknown` naming the field, where it used to be reported as applied; KiCad reporting an error while the board holds the request is now reported as applied. Arguments and the success response are unchanged.

A target that changes before the write now returns `stale_target`, rather than an uncertain outcome; nothing was written. The public `IpcPadNetOutcome` enum adds `Stale { reason }` for this refusal.
