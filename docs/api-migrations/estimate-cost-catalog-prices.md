### `estimate_cost` prices JLCPCB parts from the local catalog (minor release)

With `fab_house: "jlcpcb"`, parts whose footprint has an `LCSC` field are now
priced from the local JLCPCB catalog instead of at a flat $0.05. Each part is
priced at the catalog's price break for the order's quantity of it: its count
per board times `quantity`. Below the first break, the first break's price
applies. A part sold in steps (`10-90:…,100-590:…` sells in tens) rounds an
order between two breaks up to the next one. A catalog built before price breaks were kept, or a part whose breaks
can't be read, gives the first break's price. The feed rounds prices to 3
decimals and drops breaks below $0.01 except the first, so a cheap part has one
price. A price under $0.0005 rounds to `0.000`, and a catalog with price breaks
prices such a part at $0. In a catalog from before #63, 0 meant unknown, so
those parts still get the average. Parts without an `LCSC` field,
parts the catalog lacks, and parts it has no price for still use the $0.05
average. For each unique Extended part, the estimate adds JLCPCB's Economic
PCBA fee of $3.07 once per order.

Footprints the BOM leaves out (`exclude_from_bom` or `dnp`) are no longer
bought or placed, for JLCPCB and PCBWay: `components_estimate` and
`smt_assembly` count only the parts the BOM lists.

The response adds:
- `cost_estimate.extended_parts_fee`, which `total_estimate` and `per_board`
  include. It is `$0.00` for other fab houses.
- `cost_estimate.approximate`, true when the fab house's PCB and assembly
  prices have no cited source, as all of them have none today. `notes` then
  says so.
- `components`, which lists the parts `priced_from_catalog` and those
  `estimated` at the average, each with a `reason`. A part priced from the
  catalog gives its `order_quantity`, its `unit_price` at that quantity, and
  the `price_break` that gave it, as the feed writes it (`10-99:0.002`), or
  null when the catalog has no breaks it can read for the part. When the
  order is below the part's first break, `below_minimum_order` gives that
  break's lower end: JLCPCB sells at least that many, but the estimate prices
  only the parts the boards use. A part priced at $0 has a `note` saying its
  price rounds to $0.000. It also gives the `catalog` that
  was read or why none was, with `price_breaks` saying whether it keeps them
  and, when it doesn't, a `reason`. Then `average_unit_price`,
  `extended_unique_parts`, and the references `excluded_from_bom`. Other fab
  houses get `priced_from_catalog: []` and `excluded_from_bom`.

`components_estimate`, `smt_assembly` and the totals change for JLCPCB boards,
and for PCBWay boards with footprints the BOM leaves out. The note that named a
nonexistent `generate_bom` tool is gone. `generic` estimates are unchanged.
