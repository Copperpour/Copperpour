# Embedded library definition replacement

Created project `replace_embedded_kicad10` through `create_project` and added stock
`Device:R` as R1 at (100, 100) and R2 at (125, 100), both with value `10k`. Resaved
the original sheet without edits with KiCad 10.0.8:

```sh
kicad-cli sch upgrade --force replace_embedded_kicad10.kicad_sch
kicad-cli sch export netlist replace_embedded_kicad10.kicad_sch
kicad-cli sch erc --severity-all replace_embedded_kicad10.kicad_sch
```

For the replacement oracle, ran `replace_component` over stdio with `new_lib_id:
"Device:C"` on R1, then on R2, and exported the netlist and ran ERC at each step.
The exported component `libsource` entries and ERC pin findings were as follows.

| Sheet state | R1 library part | R2 library part | ERC findings about pins |
| --- | --- | --- | --- |
| Original | Device:R | Device:R | Two unconnected pins per part |
| R1 replaced | Device:C | Device:R | Two unconnected pins per part |
| R2 replaced | Device:C | Device:C | Two unconnected pins per part |

[KiCad 10.0.8's screen removal](https://gitlab.com/kicad/code/kicad/-/blob/10.0.8/eeschema/sch_screen.cpp#L316)
checks the effective embedded name of every remaining placed symbol before dropping
an entry. The served test checks both keeping and dropping `Device:R`, including a
variant where R2 uses it through `lib_name`. Additional variants exercise an empty
`lib_name`, the legacy `{slash}` spelling, an empty nickname, and the last value of
repeated lookup fields. KiCad 10.0.8 exported R2 for the empty-name and slash variants
with the old definition present. Its parser applies lookup fields in file order.

The test sources the capacitor definition from the KiCad-resaved
[DNP fixture](dnp_kicad10.README.md) to check the layout without system libraries.
