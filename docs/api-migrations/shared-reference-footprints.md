### `flip_component` and `find_component` refuse a reference several footprints share, and take `kiid` (minor release)

A reference two or more placed footprints carry, such as two `REF**` logos or
mounting holes, no longer flips or finds whichever footprint KiCad lists first.

- `flip_component` and `find_component` take an optional `kiid`, as
  `edit_component` does. Without it, a shared reference is refused with
  `ambiguous_target`; its `candidates` are the footprints' KIIDs, sorted, and
  the message gives each one's position. A `kiid` no footprint carries, or one
  carried under another reference, is refused with `stale_target`. Both are
  refused before anything is written.
- With KiCad closed, `flip_component` reads `kiid` as the footprint's `uuid`
  (or `tstamp`) in the board file and refuses a shared reference the same way;
  that was an `invalid_argument` refusal before, and still is where the
  footprints' UUIDs are missing or repeated.
- `find_component` also returns the footprint's `kiid`.

Pass one of the `candidates` as `kiid`, or give the footprints distinct
references.
