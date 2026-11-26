# Wire through pin text fixture

`wire_pin_text_kicad10.kicad_sch` has wires that enter a drawn pin name or number,
and wires that only come near one, so `check_schematic_overlaps` can be checked for
`wire_pin_text_overlap` (#842). Exactly four wires enter pin text.

## Provenance

Built through Copperpour's served dispatch (`tools/call`) from KiCad 10.0.8's stock
libraries, then parsed and force-resaved by KiCad 10.0.8:

1. `create_schematic` for a new `pin_text.kicad_sch`.
2. `add_schematic_component`, at these points:

   | Reference | `lib_id` | At | Rotation |
   |---|---|---|---|
   | J1 | `Connector_Generic:Conn_01x02` | (152.4, 139.7) | 0 |
   | J2 | `Connector_Generic:Conn_01x02` | (152.4, 160.02) | 0 |
   | J3 | `Connector_Generic:Conn_01x02` | (101.6, 152.4) | 90 |
   | U1 | `Sensor_Temperature:LMT01DQX` | (101.6, 101.6) | 0 |
   | U2 | `Connector:Conn_ARM_SWD_TagConnect_TC2030` | (177.8, 101.6) | 0 |

3. `add_wire` for each wire below.
4. `kicad-cli sch upgrade --force`, which writes the placed `(pin "…" (uuid …))`
   entries.
5. One line added by hand in U2's entry for pin 2: `(alternate "TMS")`, an
   alternate the stock symbol defines on that pin. There is no tool to select an
   alternate.
6. `kicad-cli sch upgrade --force` again, which kept the alternate, then line endings
   normalised to LF.

SHA-256 of the fixture: `f41bdc4e9f512062f744a669b463cb28c0b169760629f57c0587957d543fad71`.

`Conn_01x02` hides its pin names, so J1-J3 draw numbers only. `LMT01DQX` and
`TC2030` have no `(pin_names (offset …))`, so KiCad draws their names inside at
the default 0.508 mm.

## Wires

| UUID | From | To | Case |
|---|---|---|---|
| `6e268212-9084-40c8-8b3d-410dfd5e1ef5` | (147.32, 138.43) | (149.86, 138.43) | Crosses J1's number `1`, clear of the body and pin (#842's second case). |
| `2a0aabab-6354-4676-862b-617011021cd0` | (142.24, 160.02) | (147.32, 160.02) | Ends on J2's pin 1 tip, clear of its number. |
| `c4c16499-790c-403b-aa4f-d77ec381885c` | (100.33, 154.94) | (100.33, 156.21) | Crosses rotated J3's number `1`. |
| `2a9d5eb2-38dc-4e5d-8f01-f8230039412a` | (101.6, 93.98) | (101.6, 99.06) | Leaves U1's pin 1 tip along the pin and runs into its name `VP`. |
| `ed1ae618-79b9-4403-b874-b482aa91c38f` | (177.8, 102.87) | (177.8, 105.41) | Where U2's library name `SWDIO` for pin 2 would be, clear of the drawn alternate `TMS`. |
| `2f5a9655-e572-43ad-9caa-3779cf705f9c` | (180.34, 102.87) | (180.34, 105.41) | Crosses U2's drawn alternate `TMS`. |

## Checked against KiCad's plot

```text
kicad-cli sch export svg --exclude-drawing-sheet -o . wire_pin_text_kicad10.kicad_sch
```

KiCad draws each string as a group of stroked paths. Their points, grown by half the
0.1524 mm stroke, give these boxes in mm, which `reported_runs_match_kicads_plot`
compares with the reported `bounds_b`:

| Text | x | y |
|---|---|---|
| U1 `VP` | 100.8333 .. 102.2557 | 97.2939 .. 99.4420 |
| U2 `TMS` | 178.6880 .. 182.1061 | 103.3733 .. 104.7957 |
| J1 `1` | 148.7860 .. 149.6641 | 137.8347 .. 139.2571 |
| J3 `1` | 99.7347 .. 101.1571 | 155.1358 .. 156.0139 |

The plot draws `TMS`, not `SWDIO`, for U2's pin 2.
