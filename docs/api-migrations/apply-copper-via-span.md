### `apply_copper` makes blind, buried and micro vias (minor release)

Each item of `vias` takes three optional keys, named as `query_vias` reports
them ([#423](https://github.com/Copperpour/Copperpour/issues/423)): `via_type`,
one of `through` (the default), `blind`, `buried` or `micro`, and `start_layer`
and `end_layer`, the copper layers the drill spans, in either order: both, or
neither for a through via. KiCad tells a blind via from a buried one by its
span, and keeps a through via on F.Cu to B.Cu, so the span must be the type's:
`blind` has exactly one end on F.Cu or B.Cu, `buried` neither, and `through`
spans F.Cu to B.Cu; `micro` may join any two layers. A span that is not its
type's, a layer the board does not have, a missing end, or one layer at both
ends is refused as `invalid_argument` on that item's field before anything is
sent.

Each via in the dry run's plan and in the applied read-back carries `via_type`,
`start_layer` and `end_layer`, the end nearer F.Cu first; the read-back takes
them from KiCad. These keys were refused before as additional properties. Calls
that leave them out make the through vias they made before, though a
`plan_revision` that a dry run returned before this change no longer matches.
