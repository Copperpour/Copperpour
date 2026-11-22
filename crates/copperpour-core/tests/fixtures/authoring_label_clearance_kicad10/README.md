# Authoring label clearance fixture

`amp_stage.kicad_sch` is a small op-amp gain stage authored the way an agent builds a
block: ordinary `tools/call` requests to Copperpour's stdio server, coordinates only where
a tool requires them. It is the state just before the connector's signal pins get their
labels. It backs the proposal that `connect_to_net` keeps the label it places off existing
text, using J1 pin 2.

## Provenance

Drawn by tool calls, not by hand: written by Copperpour `main` at `456a8a66` (debug build,
stdio transport) against KiCad's stock symbol libraries at tag `10.0.0`
(`Amplifier_Operational`, `Device`, `Connector_Generic`, `power`), on Linux
(`(generator "copperpour")`, `(version 20250610)`, SHA-256 `e1b4bce7…4e43e9`), then
resaved with `kicad-cli sch upgrade --force` (KiCad 10.0.8): `(generator "eeschema")`,
`(version 20260306)`, SHA-256 `17f31bcb…5e21dc0`. The resave kept every item, UUID,
coordinate and property; it added KiCad's default fields and pin UUIDs, and wrote the
symbol instances under the project name `""`, as a save without a project does. KiCad
10.0.8's netlist, ERC and plotted drawing are the same before and after the resave.

The calls, in order:

1. `create_schematic` (A4).
2. `batch_place_components`: U1 `Amplifier_Operational:MCP6001-OT` at (127, 88.9); C1, C2
   `Device:C` at (106.68, 68.58), (114.3, 68.58); R1 `Device:R` at (139.7, 101.6) rotation
   90; R2 `Device:R` at (127, 109.22); J1 `Connector_Generic:Conn_01x04` at (88.9, 91.44)
   rotation 180, so its pins face right with pin 1 at the bottom.
3. `connect_pins` U1.1–R1.2, R1.1–U1.4, R2.1–U1.4 (feedback divider).
4. `add_power_symbol` by pin: `+3V3` on U1.5, `GND` on U1.2 and R2.2.
5. `batch_add_power_symbol`: `+3V3` on C1.1 C2.1, `GND` on C1.2 C2.2.
6. `connect_to_net` U1.3 `SIG_IN` and U1.1 `SIG_OUT` (default `auto` stub).
7. `add_power_symbol` by pin: `+3V3` on J1.3 (rotation 270 chosen by the tool) and `GND`
   on J1.4 (rotation 90).

J1's pins end at x 93.98: pin 1 at y 93.98, pin 2 at 91.44, pin 3 at 88.9, pin 4 at 86.36.
The `+3V3` value text on J1.3 stands vertically at x 96.77..98.19, y 86.53..91.21;
`check_schematic_overlaps` already reports it colliding with the `GND` value text on J1.4
(step 7, out of scope here).

## KiCad's view (kicad-cli 10.0.8)

- `sch export netlist`: `+3V3` C1.1 C2.1 J1.3 U1.5; `GND` C1.2 C2.2 J1.4 R2.2 U1.2;
  `/SIG_OUT` R1.2 U1.1; `Net-(U1--)` R1.1 R2.1 U1.4; `/SIG_IN` U1.3 alone; J1.1 and J1.2
  unconnected.
- `sch erc --severity-all`: 5 errors, 1 warning: `pin_not_connected` J1.1 and J1.2,
  `pin_not_driven` U1.3, `power_pin_not_driven` on the two U1 supply symbols (no
  `PWR_FLAG`), `isolated_pin_label` `SIG_IN`.

## Expected results: each J1 label call on a copy

Each call ran after the steps above; the J1.1 `SIG_IN` label is added the same way first.
Netlists are compared as pin sets.

| J1 label calls | Result | `check_schematic_overlaps` | KiCad netlist and ERC |
|---|---|---|---|
| `connect_to_net` J1.1 `SIG_IN`, J1.2 `SIG_OUT`, defaults | `right`, 2.54 mm stubs; labels at (96.52, 93.98) and (96.52, 91.44) | `SIG_OUT` label × `+3V3` value: overlap 1.41 × 1.54 mm; plus the existing `+3V3` × `GND` | `/SIG_IN` J1.1 U1.3; `/SIG_OUT` J1.2 R1.2 U1.1; rest unchanged; ERC 2 errors (`power_pin_not_driven` ×2) |
| the same with `stub_length: 5.08` | `right`; labels at (99.06, 93.98) and (99.06, 91.44) | only the existing `+3V3` × `GND` | the same netlist and ERC |
| J1.2 with `direction: "up"` or `"down"` | `plan_blocked`, nothing written | — | — |

The second row is the shape proposed as the default when the caller leaves the stub length
to the tool.
