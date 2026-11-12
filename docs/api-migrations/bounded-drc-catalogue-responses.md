### DRC and library responses expose consistent truncation metadata (minor release)

`get_drc_violations` now accepts `limit` (default **50**), like `run_drc`.
Both return `shown` (the number of entries in `violations`) and `truncated`
(whether more severity-filtered violations exist). `limit: 0` returns counts
without violation rows. Raise `limit` when a larger sample is needed, or supply
`output` to publish the complete JSON report, including all reported categories
and severities. The limit affects presentation only: the complete DRC still runs,
`filtered_count` and existing total/category/error/warning counts remain complete,
and missing categories remain unchecked (`null`), not checked zeroes.

`list_symbols_in_library` adds `shown` alongside its existing `truncated` flag.
Its `count` remains the full number of symbols; the default limit remains **100**.

`search_symbols` and `search_footprints` add `shown` and `truncated`, with the
existing default limit of **50**. Their existing `count` still counts returned
matches and equals `shown`; it is **not** a full catalogue total. Search looks
for one extra match to establish `truncated`, including across libraries, then
stops. If no extra match exists, proving that may visit all remaining registered
libraries. `limit: 0` returns no matches and reports whether any match was found.
Search metadata covers the libraries successfully searched; unreadable or missing
libraries retain the existing skip behavior. Result order and search scope are
unchanged; this adds neither pagination nor exhaustive catalogue accounting.

All five tools accept non-negative integer limits, including JSON spellings such
as `1.0`. Negative, fractional, null, boolean and string limits produce `invalid_argument`
at both the served schema and direct handler boundaries. In particular, negative
limits no longer fall back to a default, and `1.0` applies a one-entry limit.
Limits bound entry counts,
not response bytes or nested violation items.
