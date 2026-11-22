# Worked example: redraw a schematic without changing connectivity

The [KiCad 10 example][fixture] is a small ATtiny85 circuit with a battery and switch,
a light sensor, a microphone and an LED. The draft has 36 wires and five crossings.
The `+3V3` rail runs through two part fields. The redrawn sheet has five named blocks:
Battery, Microcontroller, Light sensing, Sound sensing and LED.

Compare the [draft plot][before-plot] and the [redrawn plot][after-plot], or open
`worked_example.kicad_pro` in each fixture directory. Work on a copy if you want to try
the steps. Close KiCad's Schematic Editor before using Copperpour to edit the copy.

## From draft to readable sheet

1. **Number the parts.** Copperpour assigns references with `annotate_schematic`.
   Use `dry_run: true` to preview the nine assignments before writing.
2. **Confirm unused pins and fill in values.** Copperpour lists the unconnected pins with
   `find_orphan_items` or `validate_component_connections`. The designer confirms that
   SW1.1 and U1.1, U1.3 and U1.5 are unused. The designer supplies part values, such
   as `10k` for R1, and Copperpour writes them with `batch_edit_schematic_components`.
3. **Choose blocks and place the parts.** The designer decides what belongs together
   and where it should go. The example replaces the old wiring rather than keeping
   it as parts move. On the copy, remove the old wires with
   `batch_delete_schematic_wire` and the two old power symbols with `batch_delete`,
   then move parts with `move_schematic_component` or `move_region`.
4. **Wire within blocks and place local power symbols.** Copperpour draws the requested
   connections with `batch_add_wire` or `connect_pins`. With `add_power_symbol` by
   pin, it places local `+3V3` and `GND` symbols on the chosen pins. Use
   `get_schematic_pin_locations` to find the new endpoints of the four confirmed
   unused pins and pass their coordinates to `batch_add_no_connect`.
5. **Name signals that cross blocks.** The designer chooses which wires to replace with
   labels and what to call them. Copperpour puts `LED`, `LDR` and `MIC` labels on
   matching ends with `batch_connect_to_net` or `batch_add_schematic_net_label`.
   Matching labels on this sheet join those ends and give the net a name.
6. **Lay out text and finish the sheet.** The designer picks Reference and Value
   positions. Copperpour writes them through `field_placements` on
   `edit_schematic_component` or `batch_edit_schematic_components`; there is no
   automatic field placement. The designer chooses the frame extents, block titles,
   page size and title block. Copperpour applies those choices with
   `add_schematic_shapes`, `add_schematic_text`, `set_schematic_page` and
   `set_schematic_title_block`. This example keeps the original page size.

Splitting into labelled blocks suits this small circuit. Most circuits are more
complicated, and too many blocks make a sheet harder to follow. Keep direct wires
inside a block and use labels only for signals that pass between blocks.

## Check that the circuit stayed the same

Annotate a copy of `before` before exporting KiCad netlists. Repeated designators
like `R?` can hide distinct pins in a netlist. Compare which pins share a net before
comparing net names. A layout-only cleanup must keep every pin group and net name
and add no labels. This example goes beyond cleanup: the designer chooses three
signal names for the redraw. All 25 pins keep the same connections, and the four
flagged pins stay unused. Only these three nets get new names:

| Pins | Annotated draft | Redrawn sheet |
|---|---|---|
| U1.6, D1.2 | `Net-(D1-A)` | `/LED` |
| U1.7, R1.2, R2.1 | `Net-(U1-PB2)` | `/LDR` |
| U1.2, R3.2, MK1.2 | `Net-(MK1-+)` | `/MIC` |

KiCad prefixes local net names with `/` on this sheet. ERC no longer reports the four
unconnected pins, but both versions still have two `power_pin_not_driven` errors.
Neither version adds `PWR_FLAG`; this is a readability example, not an ERC-clean design.
See the [fixture README][fixture] for the full pin list, authoring sequence and ERC
comparison.

[fixture]: ../crates/copperpour-core/tests/fixtures/worked_example_kicad10/README.md
[before-plot]: https://gist.githubusercontent.com/dylbertram/6b3de5ae4455dfa3a2458b43a59c64b6/raw/65eecc1cd472d6e29b9a0a7af99f0399adecee5a/11-before.png
[after-plot]: https://gist.githubusercontent.com/dylbertram/6b3de5ae4455dfa3a2458b43a59c64b6/raw/65eecc1cd472d6e29b9a0a7af99f0399adecee5a/11-after.png
