# Schematic texts fixture

`schematic_texts_kicad10.kicad_sch` holds six free text items beside a net
label and a symbol whose library body draws a text, so `list_schematic_texts`,
`edit_schematic_text` and `delete_schematic_text` (#56) can be checked against
what KiCad writes.

## Provenance

Built through Copperpour with `create_project`, `add_schematic_text`,
`add_schematic_net_label` and `add_schematic_component`, against KiCad 10.0.6's
stock `Device` library, then parsed and force-resaved by KiCad 10.0.6 with no
project file beside it, so the symbol's instance reads `(project "")`:

```text
kicad-cli sch upgrade --force schematic_texts_kicad10.kicad_sch
```

What is committed is that Eeschema serialization, `(generator "eeschema")` and
`(version 20260306)`. KiCad added `(exclude_from_sim no)` to each text and
wrote the texts in its own order.

## Items

| Item | Where | Role |
|---|---|---|
| `Plain note` | `(25.4, 25.4)` | a plain text |
| `First line` and `Second line` | `(25.4, 38.1)` | one string, its line break written `\n` |
| `Say "hi" from C:\new` | `(25.4, 50.8)` | a quote and a backslash, each escaped |
| `Grouped note` | `(25.4, 63.5)` | the member of the group a test adds |
| `Turned note` | `(76.2, 38.1)`, turned 90 | 2 mm, bold, justified `right top` |
| `R = 4k7 Ω ±1%` | `(25.4, 76.2)` | characters of two bytes |
| `NET_A` | `(76.2, 63.5)` | a net label, not a text item |
| `MES?`, `Device:Ammeter_AC` | `(127, 50.8)` | its library body draws `(text "A")` inside `lib_symbols`, not a text item |

The group no tool writes is added in the test, before the closing parenthesis.
KiCad 10.0.6's `kicad-cli sch upgrade --force` keeps that sheet byte for byte.
