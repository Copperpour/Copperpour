### JLCPCB catalog tools report unknown prices and stock as `null` (minor release)

A price or stock that the JLCPCB feed doesn't give, or gives as something other
than a number, used to be stored and reported as `0`. A caller couldn't tell
"unknown" from free or out of stock. Unknown values now survive the import:

- `download_jlcpcb_database` stores an empty or non-numeric price or stock as
  NULL. It also keeps the feed's price breaks in a new `Price_Breaks` column,
  and marks the catalog as schema version 2.
- In `search_jlcpcb_parts`, `get_jlcpcb_part` and `suggest_jlcpcb_alternatives`
  results, `price` and `stock` are now a number or `null`. So are `mpn`,
  `package`, `manufacturer`, `library_type` and `description`, which are `null`
  when the catalog holds no value. A text price that holds a number is
  reported as that number.
- With `in_stock: true` (the default), `search_jlcpcb_parts` still leaves out
  parts whose stock is unknown, and reports how many in the new
  `unknown_stock_excluded` field.
- `suggest_jlcpcb_alternatives` excludes a part with unknown stock and counts it
  in the new `exclusions.unknown_stock_count`. It used to count such a part in
  `below_min_stock_count`. A part with an unknown price ranks after the parts
  with a known price, as before.
- A catalog row with no LCSC number is now an error instead of a missing result.
- `get_jlcpcb_database_stats` adds `schema_version`. For a version 2 catalog it
  also adds `unknown_price_count` and `unknown_stock_count`.

A catalog built before this change stores unknown values as 0, which can't be
told apart from a real 0. Its zeros are reported as they are. The catalog tools
add a `catalog_note` asking for a rebuild with
`download_jlcpcb_database` and `force: true`, and they don't report
`unknown_stock_excluded`.

Callers that read `price` or `stock` as a number must accept `null`.
