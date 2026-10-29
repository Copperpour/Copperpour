### `search_jlcpcb_parts` ranks results instead of ordering them by LCSC number (minor release)

Results now rank in-stock parts first, then as `suggest_jlcpcb_alternatives`
ranks them: Basic, then Preferred, then Extended parts, then known price before
unknown, price ascending, stock descending, and the LCSC number last, so a page
is still the same on every run. The response states this order in a new
`ranking` field. The in-stock tier matters only with `in_stock: false`; the
default already leaves out parts with no stock.

`limit` applies after ranking, so a limited search can now return different
parts than before: the best-ranked ones rather than the lowest LCSC numbers.
Callers that relied on LCSC order should sort the results themselves.
