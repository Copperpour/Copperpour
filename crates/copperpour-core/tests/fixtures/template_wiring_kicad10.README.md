# Template wiring fixture

`template_wiring_kicad10.kicad_sch` is KiCad 10.0.8's serialization of a
Copperpour 0.16.1 project made through the installed release server over MCP
stdio. The calls were:

1. `load_toolset({"name":["project","templates"]})`
2. `create_project({"path":"<temporary directory>/project","name":"pullups"})`
3. `apply_template({"schematic":"<temporary directory>/project/pullups.kicad_sch","template_id":"i2c_pullups","position_x":101.6,"position_y":101.6,"net_mappings":{"SDA":"I2C_SDA","VCC_3V3":"+3V3"}})`
4. `kicad-cli sch upgrade --force <temporary directory>/project/pullups.kicad_sch`

The project file is the unmodified output of `create_project`. Copy the two files
as `pullups.kicad_sch` and `pullups.kicad_pro`; the instance metadata belongs to
that project name. The original server placed R1/R2 but did not wire them (#89).

Oracle command:

```sh
kicad-cli sch export netlist --format kicadxml --output netlist.xml pullups.kicad_sch
```

KiCad reports four separate singleton nets, `unconnected-(R1-Pad1)`,
`unconnected-(R1-Pad2)`, `unconnected-(R2-Pad1)` and `unconnected-(R2-Pad2)`.
No named supply or signal exists in this fixture.
`kicad-cli sch erc --severity-all --format json` reports exactly four
`pin_not_connected` findings and no structural errors.

The regression adds a three-resistor user template at (180.34, 101.6). Its
intent, independent of the implementation, is:

| Net | New pins | Expected result |
|---|---|---|
| I2C_SDA | R3.1 | SDA mapping executed |
| +3V3 | R3.2, R4.2 | Supply mapping executed; both pins share one net |
| SCL | R4.1 | Unmapped signal executed |
| None | R5.1, R5.2 | Invalid R3.9 and nonexistent Q.1 entries explicitly unresolved |

The second regression exercises the other map forms:

| Net | New pins | Entry form |
|---|---|---|
| INPUT | R3.1 | External GPIO → pin, mapped to INPUT |
| R3_2 | R3.2, R4.1 | Pin → pin |
| R4_2 | R4.2, R5.1 | Pin → series resistor (`via`) |
| GND | R5.2 | Other end of the series resistor |
| WRONG | None | R3.1 already belongs to INPUT; report the conflict without claiming or writing WRONG |

Tests run through saved connectivity, including one served `tools/call`, and
retain R1/R2. Readback of a deliberately false WRONG-net claim must fail.
The first regression also requires at least 10.16 mm between successive symbol
bounds for stub and label space. KiCad CLI netlist export and SVG inspection of
the applied I2C template provide the external end-to-end checks.

Review regressions also join `R3.1–R4.1–R5.1` through two pin-pair
entries, first as generated `R3_1`, then as explicitly mapped `SENSE_BUS`.
All three second pins go to GND; the original R1/R2 symbol trees must remain
unchanged. A separate case pre-labels old R1.1 as SDA and old R2.1 as R3_1:
an unresolved Q.1→SDA must not report a join, while the new R3.1–R4.1 pair
really joins the existing generated-name net R3_1 and must report it.
The alias case gives existing SDA a higher-priority BUS global label; saved
root identity, rather than the winning name, must still verify the connection.

The repeated-name regression reuses `power_rows_kicad10.kicad_sch` and its
KiCad-authored oracle: STM32F103C8Tx pins 24 and 36 are distinct VDD pins at
different positions. A newly placed U9 connects them to separate resistors as
U9_24 and U9_36, with distinct saved net roots. A stock USB-C stdio case similarly
keeps A6 and B6 (both named D+) separate as J1_A6 and J1_B6.

Malformed/unsupported routing cases reuse the original two-resistor fixture.
Each case adds one new resistor with a valid R3.2→GOOD entry and one invalid
entry: series-to-pin, numeric/blank routing fields, missing/blank source, or
competing destinations. Only one GOOD label and one wire may be added; R3.1 must
remain unnamed and unconnected. An invalid `via` must never bypass its part and
short the resistor. Independent valid entries still apply.
