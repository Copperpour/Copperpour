### `estimate_cost` prices JLCPCB parts from the local catalog (minor release)

With `fab_house: "jlcpcb"`, parts whose footprint has an `LCSC` field are now
priced at the local JLCPCB catalog's unit price instead of a flat $0.05. Parts
without one, parts the catalog lacks, and parts it has no price for still use
the $0.05 average. Quantity price breaks are not applied yet. Footprints the BOM
leaves out (`exclude_from_bom` or `dnp`) are no longer priced. For each unique
Extended part, the estimate adds JLCPCB's Economic PCBA fee of $3.07 once per
order.

The response adds:
- `cost_estimate.extended_parts_fee`, which `total_estimate` and `per_board`
  include. It is `$0.00` for other fab houses.
- `components`, which lists the parts `priced_from_catalog` and those
  `estimated` at the average, each with a `reason`. It also gives the
  `catalog` that was read or why none was, `average_unit_price`,
  `extended_unique_parts`, and the references `excluded_from_bom`. Other fab
  houses get `priced_from_catalog: []` and `excluded_from_bom`.

`components_estimate` and the totals change for JLCPCB boards. The note that
named a nonexistent `generate_bom` tool is gone. PCBWay and `generic` estimates
are unchanged.
