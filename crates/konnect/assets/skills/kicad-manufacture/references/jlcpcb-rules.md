# JLCPCB Order-Contract Verification

Use this reference only for a JLCPCB fabrication or assembly branch. JLCPCB
controls its capabilities, prices, part categories, templates, and portal
validation. The current order contract is authoritative; this file deliberately
does not cache those volatile values.

## 1. Pin the selected service

Record the exact selected service before configuring the board:

- fabrication versus fabrication plus assembly;
- economic/standard or other service tier shown by the current portal;
- layer count, stackup, copper weight, thickness, finish, colour, and quantity;
- assembly side(s), stencil choice, panelization, and special processes; and
- controlled-impedance, via, slot, castellated, edge-plating, or other options.

For every copied limit or template requirement, record the source and retrieval date.
A generic capability page does not override the selected service's order-page
constraints.

Completion criterion: the order record names one selected service and contains
the current limits for every feature the design uses.

## 2. Verify parts against the live assembly branch

Use `search_jlcpcb_parts` and `suggest_jlcpcb_alternatives` to produce candidates,
then verify each candidate in the current assembly order:

- exact manufacturer part number and package;
- current category, availability, and quantity;
- feeder/setup or special-handling effects; and
- lifecycle or substitution constraints.

The downloaded catalogue is useful discovery evidence, not a live stock or price
guarantee. Record the catalogue date and recheck the order before payment.

## 3. Bind BOM and CPL to current templates

Generate BOM and position data through Copperpour, then compare the exported headers,
units, side names, origin, delimiter, and designator grouping with the templates
offered by the selected service. Map the project supplier field deliberately;
do not assume one historical column spelling remains mandatory.

In the export preview, account for every placed designator and every intentional
DNP. Inspect pin 1, polarity, side, rotation, and footprint/package agreement for
each orientation-sensitive part. The preview, datasheet, footprint, and physical
pin-map evidence must agree.

Completion criterion: the portal accepts both files and the export preview has
no unexplained missing, extra, rotated, mirrored, or substituted component.

## 4. Apply current fabrication constraints

Copy trace/space, annular-ring, drill, slot, copper-to-edge, mask, silkscreen,
stackup, and impedance constraints from the selected service into project rules
and netclasses. Use the stricter applicable value when the component datasheet,
electrical calculation, or enclosure imposes a stronger requirement.

Re-run KiCad DRC after applying the contract and after every routing or placement
change. Inspect the Gerber and drill outputs in a viewer; a rule table alone does
not prove the exported geometry.

## 5. Accept the package

Create a fresh export destination and apply the manufacturing skill's artifact
acceptance gate. Upload only the accepted manifest. In the order portal:

- confirm every copper, mask, silkscreen, paste, and outline layer;
- confirm plated/non-plated holes, slots, cutouts, dimensions, and layer count;
- review warnings and the rendered board preview;
- verify assembly mapping and rotations when assembly is selected; and
- save the final quote and order configuration as the purchasing record.

Any unexplained difference between the saved design, artifact manifest, and
export preview makes the result `INCOMPLETE`.
