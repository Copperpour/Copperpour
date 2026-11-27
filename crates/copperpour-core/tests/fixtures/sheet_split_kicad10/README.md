# Sheet split fixture

One crowded A4 sheet with three candidate blocks. It backs the proposal to report a
block's sheet interface before splitting it onto a child sheet, and the split itself.

## Provenance

`split/split.kicad_sch` and `split/split.kicad_pro` were drawn by tool calls, not by hand
(`create_project`, `batch_place_components`, `add_schematic_component`, `batch_add_wire`,
`add_junction`, `add_power_symbol`, `add_no_connect`, `add_schematic_net_label`,
`batch_edit_schematic_components`) against KiCad 10's stock `74xx`, `Connector_Generic`,
`Device`, `Transistor_BJT` and `power` libraries, then resaved with
`kicad-cli sch upgrade --force` (KiCad 10.0.8), so the sheet carries
`(generator "eeschema")` and `(version 20260306)`. Nothing was edited by hand. The resave
kept every UUID and coordinate; kicad-cli 10.0.8 gives the same netlist (pin to net and
component paths) and the same ERC result before and after it.

`split_populated.kicad_pcb` is the board that schematic was last synced to. KiCad 10.0.8's
`pcbnew` Python API wrote it from the resaved sheet's kicad-cli netlist: each component's
footprint from the stock libraries, with its reference, value, symbol path
(`/<symbol UUID>`) and pad nets, on a simple grid. No tracks. Copy it beside the sheet as
`split.kicad_pcb` for a board that links the symbols: a split must then refuse, because
four footprint paths and two pad net names would go stale.

## The sheet

| Block | Parts | What it does |
|---|---|---|
| Shift register | `U1` 74HC595, `J1` 1x06 header | J1 feeds SER, SRCLK, SRCLR and RCLK; QB to QH' carry no-connect flags; OE to GND. |
| Pull-ups | `R3`–`R6` 10k | One per input, from `+5V` to each J1-to-U1 line; the lines cross on the way. |
| Decoupling | `C1` 100n | On U1's `+5V`; its `+5V` symbol and a wire carry the rail on to R2. |
| LED driver | `R1` 1k, `Q1` BC547, `D1`, `R2` 330 | QA drives Q1's base through R1 on the `LED_EN` label's run; Q1 sinks D1 through R2 from `+5V`; Q1's emitter has its own `GND` symbol; `LED_K` labels D1 to Q1. |

`#FLG01` and `#FLG02` give the rails a driver so ERC stays quiet.

## KiCad's answer (kicad-cli 10.0.8)

| Net | Pins |
|---|---|
| `+5V` | C1.1 J1.1 R2.1 R3.1 R4.1 R5.1 R6.1 U1.16 |
| `GND` | C1.2 J1.2 Q1.3 U1.13 U1.8 |
| `/LED_EN` | R1.1 U1.15 |
| `/LED_K` | D1.1 Q1.1 |
| `Net-(D1-A)` | D1.2 R2.2 |
| `Net-(Q1-B)` | Q1.2 R1.2 |
| `Net-(J1-Pin_3)` … `Net-(J1-Pin_6)` | J1.n, R3–R6 pin 2, U1.14 / U1.11 / U1.10 / U1.12 |

The other eight U1 outputs are `unconnected-(U1-…)`. `kicad-cli sch erc --severity-all`
reports nothing.

## What the blocks are for

- The LED driver hides three nets (`Net-(Q1-B)`, `/LED_K`, `Net-(D1-A)`) behind one signal
  (`LED_EN`) and two rails: the positive case for a split.
- The pull-ups hide nothing and would expose four signals: the "leave it as it is"
  control.
- C1 alone hides nothing: a second control.

Selecting just the four LED symbols leaves C1 alone on the parent. C1.1 shares the drawn
`+5V` island with R2.1, but nothing on the sheet says which part the capacitor was meant
to support, and U1 reaches those rails by power names alone. A split that includes C1
keeps the whole drawn supply branch, and the sheet interface is still one signal and two
global rails. After a split of the LED driver, KiCad names `/LED_K` as `/LED/LED_K`.

## Planner controls

The served split tests reconstruct variants and check each SHA-256 against the file
KiCad 10.0.8 wrote with `kicad-cli sch upgrade --force`. This avoids storing nearly
identical copies. The variants keep the `split` project file and sheet basename:

- Replace `LED_EN` with the bus label `DATA[0..7]`, without adding bus segments.
- Change the embedded `+5V` power symbol from global to sheet-local.
- Rename the internal `LED_K` local label to `SER`. That is the name the planner
  would otherwise suggest for the separate signal on `R3.2`.
- Change the embedded electrical type of `U1.15` from `tri_state` to
  `bidirectional`, then select the two sides of the `LED_EN` boundary separately.
- Change C1's first embedded pin from `passive` to `input`. The drawn branch remains.
- Replace the drawn link from C1 to R2 with a separate `+5V` placement at the R2
  end. The supply name still joins their pins, but the drawn islands are separate.
- Make `LED_EN` global without changing the wires, to check the scope decision.
- Add another `LED_EN` label on an island with no component pins. The existing
  drawn crossing remains.
- Set `#PWR08`'s value to `${LED_RAIL}`, with the saved project variable set to
  `+5V`. The planner refuses the expression rather than treating it as a net name.
- Add an output alternate `SER_OUT` to `U1.14`, then activate it in a second copy.
- Use `A/B` and `A{slash}B`, then `A|B` and `A{bar}B`, as an embedded pin and a
  local label or as two embedded pin names. The planner refuses equivalent ports.
- Hide `U1.16` and name its library pin `+5V`, then activate its `VDD_ALT`
  alternate in a second copy. The alternate changes the pin display, not the rail.
- Add a drawn wire from the `U1.16` supply island to the C1 and R2 branch.
  `U1.16` is no longer a name-only alternative, but the `+5V` pin set is unchanged.
- On that wired variant, extend the `U1.16` pin and adjust its wire and power
  symbol. C1.1 and U1.16 are within 25.4 mm even though their symbol origins
  are farther apart. Both KiCad versions export the same `+5V` members.
- Change R3 and R4 to `R?` in their properties and instance records to check
  that unannotated pull-ups do not block a plan for the LED driver.
- Remove C1's embedded pins and its matching placed pin records. A genuine
  pinless symbol differs from a placed symbol whose saved geometry is unresolved.
- Rename the embedded `SER` pin `DATA[0..7]` or `${SER}`. KiCad still exports
  the existing scalar net; neither spelling is safe to suggest as a new port.

The sheet-exit test similarly adds a `SPLIT` local label to the retained sheet-pin
wire in a copy of `single_pin_nets.kicad_sch` and checks the KiCad-resaved bytes.
Tests that deliberately corrupt fields start from saved fixtures, but their invalid
copies are not KiCad output.
