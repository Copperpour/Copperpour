# Project Design-Rule Workflow

Design rules are project evidence, not generic prose defaults. Derive them from
the exact design requirements, component datasheets, the selected fabricator's current contract,
ordered stackup, and accepted electrical calculations.

## 1. Capture rule provenance

Record the source and retrieval date for:

- trace, space, annular-ring, drill, slot, and copper-to-edge limits;
- mask, paste, silkscreen, and courtyard constraints;
- layer count, copper thickness, dielectric stackup, and impedance service;
- voltage-clearance, creepage, current, thermal, and mechanical requirements;
- assembly, test, panelization, and enclosure constraints.

Use the strictest applicable requirement. A capability advertised for another
service tier or stackup does not authorize the selected order.

Completion criterion: every configured rule has a current source or calculation,
and every applicable requirement has a project rule or explicit review check.

## 2. Encode the accepted values

Use `set_design_rules` for board-wide minima and `get_design_rules` to read back
what was stored. Use `create_netclass` for electrical groups and
`assign_net_to_class` for exact net membership. Use `set_predefined_sizes` for
the accepted trace/via palette.

The project netclasses are the source of truth for routing widths, clearances, and
via geometry. Name classes by purpose—ordinary signal, current-carrying rail,
controlled-impedance interface, high-voltage isolation—rather than copying an
undated vendor table.

Completion criterion: readback matches the accepted rule record, and every
special net is assigned to the intended class.

## 3. Verify the effective design

After encoding the values, re-run DRC after rule changes and after placement, routing, zone, or outline
changes. Resolve every error or record a deliberate waiver tied to the governing
requirement. For controlled impedance and current-carrying nets, reconcile DRC
with the calculation record; DRC only proves compliance with the values it was
given.

Before manufacturing, compare project rules with the final selected order and
stackup again. Any missing source, mismatched readback, unreviewed DRC result, or
contract drift makes the rule set `INCOMPLETE`.
