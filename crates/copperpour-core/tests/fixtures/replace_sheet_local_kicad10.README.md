# Sheet-local symbol replacement

Started with the two stock `Device:R` placements in
`replace_embedded_kicad10.kicad_sch`. Opened a copy in Eeschema 10.0.8 and
selected **Edit with Symbol Editor** on R2. Enabled **Show pin number** on
that schematic instance only, then saved the symbol and the sheet. KiCad
gave R2 a separate embedded definition, `R_1`, without moving its pins or
changing its `lib_id`. R1 still uses `Device:R` directly.

Resaved with `kicad-cli sch upgrade --force replace_sheet_local_kicad10.kicad_sch`
on 10.0.8. The baseline XML netlists from 10.0.6 and 10.0.8 both report
`libsource` part `R_1` for R2 and `Device:R` for R1.
