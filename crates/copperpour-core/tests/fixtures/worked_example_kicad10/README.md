# Worked example: one sheet redrawn in functional blocks

Two KiCad 10 projects with the same circuit. `before/` is a first draft: long wires that
cross, a supply rail drawn through two parts' field text, `R?` designators, generic
values, and unused pins with no no-connect flag. `after/` is the same symbols redrawn in
five framed, titled blocks. The pair is the shared before/after example for the schematic
readability work: layout cleanup and checks start from `before`, and authoring aims at
`after`.

See the [worked example][walkthrough] for a walkthrough of the redraw and which
decisions belong to the designer.

The circuit follows the public video "How To Prettify Your Schematic with Kicad 7.0"
(https://www.youtube.com/watch?v=iqIRjMkHVjY): an ATtiny85 with a battery cell and slide
switch, an LDR divider, an electret microphone with a bias resistor, and an LED with its
resistor. The circuit was recreated here from KiCad's stock libraries; none of the video's
artwork is used.

## Use with care

Splitting a sheet into labelled blocks suits a small circuit like this one. Over-splitting
is bad, and most circuits are more complicated. Keep direct wires inside a block and use
labels only for the signals that pass between blocks. Each label also gives its net a
name, which is a design decision, not a layout change.

## The circuit

| Net (in `after`) | Pins |
|---|---|
| `+3V3` | BT1.1 (+), R1.1, R3.1, U1.8 (VCC) |
| `GND` | SW1.2, R2.2, R4.2, MK1.1, U1.4 (GND) |
| `Net-(BT1--)` | BT1.2 (-), SW1.3 |
| `/LED` | U1.6 (PB1), D1.2 (A) |
| `Net-(D1-K)` | D1.1 (K), R4.1 |
| `/LDR` | U1.7 (PB2), R1.2, R2.1 |
| `/MIC` | U1.2 (PB3), R3.2, MK1.2 (+) |
| no net | U1.5 (PB0), U1.3 (PB4), U1.1 (PB5), SW1.1 |

Parts: BT1 `Device:Battery_Cell`, SW1 `Switch:SW_SPDT`, U1
`MCU_Microchip_ATtiny:ATtiny85V-10P`, R1, R3 and R4 `Device:R`, R2 `Sensor_Optical:LDR07`,
MK1 `Device:Microphone_Condenser`, D1 `Device:LED`, and `power:+3V3` and `power:GND`.

In the video, the first draft and the finished sheet put the LDR and the microphone on
different pins (LDR on PB4 and the microphone on PB2 in the draft, PB2 and PB3 when
finished). Here both sheets use the finished sheet's pins. The redraw changes layout,
net names and displayed part information without reassigning any pins.

## Provenance

Both sheets were drawn by tool calls, not by hand: Copperpour's served tools (`tools/call`
over stdio) from `main` at `456a8a66`, on Linux with KiCad 10.0.8's stock symbol libraries
installed. Nothing was typed into the files by hand.

`before/`:

1. `create_project` (`name`: `worked_example`).
2. `batch_place_components` with no `reference`, so each part is placed unannotated, as
   eeschema places it: BT? at (38.1, 101.6), SW? at (35.56, 116.84) turned 90°, U? at
   (127, 101.6), R? (`R`) at (190.5, 50.8), R? (`LDR07`) at (190.5, 66.04), R? (`5.6k`) at
   (190.5, 124.46), MK? at (190.5, 142.24), D? at (205.74, 88.9) turned 90° and R? (`R`)
   at (205.74, 101.6). Values are the library's, except `5.6k`.
3. `batch_add_wire` with 36 wires. The tool added the seven junction dots. The `+3V3` rail
   runs down x = 208.28, through D?'s `LED` value text and the `R?` reference text of the
   LED's resistor, both drawn vertically beside their parts.
4. `add_power_symbol` for `+3V3` at (127, 76.2) and `GND` at (127, 129.54).
5. `set_schematic_title_block` (title and comment 1).

`after/` started as a byte copy of the finished `before/` project, so the nine non-power
symbols keep their UUIDs. The two original power symbols were replaced. Then:

1. `annotate_schematic`: BT1, SW1, U1, R1 (10k), R2 (LDR), R3 (mic bias), MK1, D1, R4 (LED
   resistor), in ascending X on `before`'s layout.
2. `batch_delete_schematic_wire` with every wire (it removed the dots), and `batch_delete`
   of the two power symbols.
3. `move_schematic_component` for each part into its block. No part was rotated.
4. `batch_add_wire` for the six wires inside blocks: BT1 to SW1, the divider, the mic
   bias, the LED to R4, and the two taps to the LDR and MIC labels.
5. `add_power_symbol` by `reference` and `pin_number` for nine local `+3V3` and `GND`
   symbols, each placed on its pin and turned away from the body.
6. `batch_connect_to_net` for U1 pins 6, 7 and 2 (`LED`, `LDR`, `MIC`, 10.16 mm stub to
   the right) and D1 pin 2 (`LED`, 5.08 mm stub up); `batch_add_schematic_net_label` for
   `LDR` at (152.4, 83.82) and `MIC` at (200.66, 83.82), turned 180°.
7. `batch_add_no_connect` on U1 pins 5, 3 and 1 and SW1 pin 1.
8. `batch_edit_schematic_components` for values (`3.3 V cell`, `Slide switch`, `10k`,
   `LDR 5 mm`, `Electret mic`, `Yellow LED`, `470`; R3 already had `5.6k`) and for
   Reference and Value text turned horizontal beside each part.
9. `add_schematic_shapes` with five dashed rectangles from y = 59.69 to 118.11 (Battery x
   25.4 to 60.96, Microcontroller 66.04 to 134.62, Light sensing 142.24 to 180.34, Sound
   sensing 187.96 to 228.6, LED 236.22 to 276.86), and `add_schematic_text` for each
   block's title at 2.54 mm.
10. `set_schematic_title_block` (comment 1 changed).

Each sheet was then resaved by KiCad:

```text
kicad-cli sch upgrade --force worked_example.kicad_sch
```

What is committed is that eeschema serialization: `(generator "eeschema")`,
`(version 20260306)`. A structural comparison of each file before and after the resave
found no change to any position, UUID, reference, library ID or property value; KiCad
added pin UUIDs and default tokens. The `.kicad_pro` files are the ones `create_project`
wrote.

## Connectivity: only net names changed

kicad-cli 10.0.8 netlists of an annotated copy of `before` and of `after`, compared by
symbol UUID and pin number: the same 9 symbols and the same 11 nets, each holding the same
pins. Three nets changed name, because `after` labels them:

| Pins | `before` (annotated) | `after` |
|---|---|---|
| U1.6, D1.2 | `Net-(D1-A)` | `/LED` |
| U1.7, R1.2, R2.1 | `Net-(U1-PB2)` | `/LDR` |
| U1.2, R3.2, MK1.2 | `Net-(MK1-+)` | `/MIC` |

Unchanged: `+3V3`, `GND`, `Net-(BT1--)`, `Net-(D1-K)` and the four `unconnected-(...)`
nets of the unused pins. The flags do not put those pins on a net.

The committed `before` cannot be compared by designator: KiCad names its nets after the
unannotated parts with a generated suffix (`Net-(R?-020b9afd-Pad2)`), and its netlist
lists a pin of two `R?` parts on one net as one node.

## ERC

kicad-cli 10.0.8, `sch erc --severity-all`, with KiCad's stock symbol and footprint
library tables:

| Check | `before` | `after` |
|---|---|---|
| `pin_not_connected` (error) | 4: U?.5, U?.3, U?.1, SW?.1 | 0 |
| `power_pin_not_driven` (error) | 2: `#PWR01`, `#PWR02` | 2: `#PWR01`, `#PWR02` |

The no-connect flags remove the four `pin_not_connected` errors. `power_pin_not_driven`
stays in both: the circuit, like the video's, has no `PWR_FLAG`, and adding one is a
design change outside this example. KiCad's ERC reported nothing about `before`'s
unannotated designators.

## Copperpour's checks on these sheets (`main` at `456a8a66`)

| Tool | `before` | `after` |
|---|---|---|
| `find_orphan_items` | 4 `unconnected_pin`: SW?.1, U?.1, U?.3, U?.5 | 0 |
| `validate_component_connections` | the same 4 pins | valid |
| `annotate_schematic` dry run | 9 to number (BT1, SW1, U1, R1 to R4, MK1, D1) | nothing to number |
| `check_schematic_overlaps` | 0 | 0 |
| `find_single_pin_nets` | 0 | 0 |
| `check_bom_health` | 8 findings: no footprint on 7 parts, no MPN on U1; `missing_value` 0 | the same 8 |

None of these tools reports what makes `before` hard to read: the `+3V3` rail through D?'s
value text and R?'s reference text, the five places where wires cross, the two junctions
where four wires meet (at U?'s VCC and GND), or the generic values (`R`, `LED`,
`Battery_Cell`), which `check_bom_health` does not count as missing.

## What a layout comparison of the pair should find

For a layout-only cleanup of `before`, preserve every pin group and net name and add
no labels. For an authoring flow given the block plan, use `after` as the target:
preserve the pin groups and allow exactly the three designer-chosen renames above.

- Annotate a copy of `before`. Each designator then names the same symbol UUID in both
  sheets, all 25 pins keep the same membership, exactly the three nets
  above change name, and the four unused pins are on no net in either.
- Control: deleting the `MIC` label on the microphone's side of `after` splits that net,
  and the comparison must report it.
- `before` has the four unflagged pins and nine unannotated parts; `after` has none. An
  `annotate_schematic` dry run leaves `before` unchanged.

The `schematic_worked_example_tests` module exercises these comparisons through
Copperpour's `tools/call` dispatcher without needing a KiCad installation.

To check the drawings as well, export fresh plots from this directory with KiCad 10:

```sh
kicad-cli sch export svg --output ./before/ ./before/worked_example.kicad_sch
kicad-cli sch export svg --output ./after/ ./after/worked_example.kicad_sch
```

In `before/worked_example.svg`, look for the `+3V3` rail crossing the LED's Value
and its resistor's Reference. In `after/worked_example.svg`, check that Battery,
Microcontroller, Light sensing, Sound sensing and LED each have a frame and title.

[walkthrough]: ../../../../../docs/SCHEMATIC_READABILITY_WORKED_EXAMPLE.md
