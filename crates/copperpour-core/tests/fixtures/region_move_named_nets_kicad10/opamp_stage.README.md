The root has two sheets and no symbol or sheet pins. Global `A` is at
`(40, 40.02)`, 0.02 mm off the wire named by global `B`. It was built from
a blank sheet with Copperpour tools, linking copies of the original
children and placing one wire and two labels. `kicad-cli sch upgrade --force`
in KiCad 10.0.6 and 10.0.8 resaved the same bytes.

With `opamp_stage.kicad_pro` from the `block_placement_kicad10` project
beside the sheets, KiCad 10.0.6 and 10.0.8 export J2.1 on `A` and J3.1
on `B`. After `move_region` moves only `A` down by 0.02 mm onto the
wire at Y=40, both versions export J2.1 and J3.1 on `A`.
At Y=40.0001 both pins are on `A`; at Y=40.0002 they are separate again.
