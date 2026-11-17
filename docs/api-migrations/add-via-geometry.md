### `add_via` validates drill and pad geometry before IPC (patch release)

`drill` and `pad_size` must be numbers that round to positive nanometre sizes,
with `drill` strictly smaller than `pad_size` after rounding. Invalid sizes and
malformed present values (including null) now return `invalid_argument` naming
the field before any IPC call. Previously these could reach KiCad or silently
select defaults. Omit the fields to keep the 0.4 mm drill and 0.8 mm pad defaults;
otherwise supply valid geometry. Board design rules may require larger sizes
or annular rings; this check does not replace DRC.
