# Wire through a symbol body fixture

`wire_through_symbol_kicad10.kicad_sch` has wires that run through a placed
symbol's drawn body, and wires that only meet or pass one, so
`check_schematic_overlaps`' `wire_overlap` entries can be checked: exactly the
four wires through a body are reported, and no control is.

## Provenance

Built through Copperpour's served dispatch (`tools/call`) against KiCad's stock
`Device` library and one symbol made here, then parsed and force-resaved by KiCad 10.0.6:

1. `create_project` (`name`: `wire_through_symbol_kicad10`).
2. `create_symbol` into `Reach.kicad_sym` in the project: `ReachPastTip`,
   `reference_prefix` `X`, two passive pins, `1` at (0, 3.81) angle 270 and `2`
   at (0, -3.81) angle 90, both length 1.27, drawn by `graphics`: a rectangle
   (-1.016, 2.54) to (1.016, -2.54) and a line (1.27, 2.54) to (1.27, 4.572).
   The line is the only drawing off the rectangle: it reaches 0.762 mm past pin
   1's tip, 1.27 mm to the side of the pin's axis.
3. `register_symbol_library` (`scope`: `project`) for `Device` and `power`, on KiCad's
   `Device.kicad_sym` and `power.kicad_sym`, and `Reach` on `Reach.kicad_sym`.
4. `add_schematic_component` for `Y1`, `Y2`, `Y3` (`Device:Crystal`), `R1`, `R2`,
   `R4` (`Device:R`), `X1` (`Reach:ReachPastTip`) and `D1` (`Device:LED`) at the points below,
   `rotation` 0, and `add_power_symbol` for `GND` at (292.1, 50.8).
5. `add_wire` for each wire below.
   `add_wire` added a junction dot at (250.19, 50.8) and at (203.2, 46.99), where two wires and a pin tip meet.

```text
kicad-cli sch upgrade --force wire_through_symbol_kicad10.kicad_sch
```

Only the schematic was resaved and committed. What is committed is that
Eeschema serialization: `(generator "eeschema")`, `(version 20260306)`. KiCad
kept every position and UUID.

`add_wire` puts its ends on the 1.27 mm grid, so a wire along a `Device:R` body
edge (x = 1.016 off its centre) cannot be drawn; the edge cases use
`Device:Crystal`, whose body edges are on the grid.

## Cases

Pin tips of `Device:R` are 3.81 mm above and below its centre, of
`Device:Crystal` 3.81 mm left and right. The `Device:R` body spans 2.032 mm by
5.08 mm and the `Device:Crystal` body 5.08 mm by 5.08 mm, both about the
centre.

| Part | At | Wire (x1, y1) to (x2, y2) | Reported | Why |
|---|---|---|---|---|
| `Y1` `Device:Crystal` | (50.8, 50.8) | (46.99, 50.8) to (54.61, 50.8) | yes | from one pin tip through the body to the other, the defect's shape; its ends sit on both pin tips, so KiCad joins both pins to it |
| `R1` `Device:R` | (101.6, 50.8) | (99.06, 50.8) to (104.14, 50.8) | yes | crosses the body and touches no pin |
| `R2` `Device:R` | (50.8, 101.6) | (50.8, 97.79) to (50.8, 92.71) and (50.8, 105.41) to (50.8, 110.49) | no | each starts on a pin tip and runs away from the body |
| `Y2` `Device:Crystal` | (101.6, 101.6) | (96.52, 99.06) to (106.68, 99.06) | no | along the top edge of the body |
| `R4` `Device:R` | (152.4, 50.8) | (154.94, 46.99) to (154.94, 54.61) | no | passes beside the body, clear of it |
| `Y3` `Device:Crystal` | (152.4, 101.6) | (144.78, 100.33) to (149.86, 100.33) | no | ends on the left edge of the body, touching it |
| `X1` `Reach:ReachPastTip` | (203.2, 50.8) | (203.2, 46.99) to (203.2, 41.91) | no | starts on pin 1's tip and leaves it away from the pin's root; the symbol's drawing reaches 0.762 mm past that tip, so the wire clips the body box there, with no drawing across it |
| `X1` | (203.2, 50.8) | (203.2, 46.99) to (203.2, 50.8) | yes | starts on the same tip and runs toward the root, into the body |
| `D1` `Device:LED` | (254, 50.8) | (250.19, 50.8) to (242.57, 50.8) and (257.81, 50.8) to (265.43, 50.8) | no | KiCad's stock LED, wired the usual way: one wire leaves each pin tip outward along the axis. Its emission arrows reach 0.762 mm past the left tip, so that wire clips the body box with no ink across it; the stock symbol is here because it is a real case of a drawing reaching past a pin tip |
| `D1` | (254, 50.8) | (250.19, 50.8) to (250.19, 45.72) | no | leaves the cathode tip square to the pin, inside the body box and clear of the arrows' ink. A square wire is exempt even where it crosses drawing: one from that tip straight down to (250.19, 55.88) would cross the emission-arrow ink near (250.19, 52.324) and is not reported (the limit, covered in the unit test, not drawn here). A wire that heads for the root is the case that reports, as on `X1` |
| `#PWR01` `power:GND` | (292.1, 50.8) | (292.1, 50.8) to (292.1, 53.34) | yes | ends on the symbol's zero-length pin tip and runs down into its own drawing; a zero-length pin has no root direction, so it exempts nothing |

The test reads the committed file and asserts the four reported wires by UUID and
reference.
