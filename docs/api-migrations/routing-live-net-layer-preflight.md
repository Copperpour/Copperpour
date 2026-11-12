### Routing refuses missing live nets and disabled copper layers (patch release)

`route_trace`, `route_pad_to_pad`, and `route_differential_pair` now validate the
requested copper layer and nets on the bound live board before creating tracks.
`add_via` also validates its requested net before creating the via. A missing net
returns `invalid_argument` naming `net_name`, `net_pos`, or `net_neg`; a disabled
copper layer names `layer`. Previously these cases were reported as KiCad being
unreachable, and a missing negative differential-pair net could leave the positive
trace behind. Both pair nets are now checked before creating either trace.

Choose an existing live-board net and enabled copper layer, then retry. IPC read
failures, missing net response bodies, and empty enabled-layer evidence remain
failures, not evidence that a requested net or layer is missing. The IPC client
adds `get_nets_required_in`; existing `get_nets_in` fallback behavior is unchanged. No arguments or
successful response formats change. This preflight does not make separate IPC
creation calls atomic against later errors or intervening editor changes.
