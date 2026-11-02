### `set_board_stackup` (minor release)

A new `pcb_board` tool replaces the physical stackup of a board KiCad does not
hold open ([#78](https://github.com/Copperpour/Copperpour/issues/78)):
`set_board_stackup(board, copper_layers, dielectrics, solder_mask?, silkscreen?,
finish?, impedance_controlled?, board_thickness_mm?)`. `copper_layers` runs top
to bottom, one per copper layer of the board, each `{thickness_mm}` or
`{weight_oz}`, an ounce read as KiCad reads `oz` typed into a field, 1.37 mil.
`dielectrics` runs top to bottom between them, each `{type: "core" | "prepreg",
thickness_mm, material?, epsilon_r?, loss_tangent?}`. `solder_mask` (`color`,
`thickness_mm`) and `silkscreen` (`color`) apply to both sides; a color is one
of KiCad's names (Green, Red, Blue, Purple, Black, White, Yellow) or
`#RRGGBB`/`#RRGGBBAA`, written as KiCad writes one, `#RRGGBBAA`. `finish` is
one of Board Setup's finish names.

What a call leaves out is kept from the board's stackup: a dielectric's
material, εr, loss tangent, color and thickness lock, taken from the dielectric
in the same place, as KiCad numbers them; the mask's and silkscreen's colors
and the mask's thickness, material, εr and loss tangent; the finish, impedance
control and the edge settings. Where the board has no stackup, KiCad's
defaults: FR4, εr 4.5 and loss tangent 0.02 for a dielectric, a 0.01 mm mask
with no color, finish "None".

The stack is written as KiCad writes one, in its order and layout, and the
board thickness under `(general …)` is always the stack's sum, which KiCad
reports and exports. `board_thickness_mm` only checks it: a sum more than
0.001 mm away is refused with `invalid_argument`, naming the sum. Refused the
same way: a count of copper layers or dielectrics that does not fit the board,
a copper layer with both or neither of `thickness_mm` and `weight_oz`, a
thickness that rounds to under 1 nm or that KiCad's reader would clamp, a
`solder_mask` or `silkscreen` on a board without those layers, and a value of
the wrong type or outside the schema. The copper-layer count is never changed.
Refused as `plan_blocked`: a board file older than format 20200922, whose
layer table may name copper otherwise, until KiCad 10 has saved it; copper
that is not F.Cu, In1.Cu … B.Cu; a dielectric that holds sub-layers, left to
Board Setup.

Before writing, kicad-cli loads and saves a copy with the new stack (`pcb
upgrade --force`); unless KiCad's save gives back every layer, value and the
thickness, or when there is no kicad-cli, the tool returns `plan_blocked`
naming each difference, and writes nothing. A board KiCad holds open is
refused, before the copy and again before the write; a file saved after the
read returns `conflict`; the saved stack is read back. The response gives
`changed`, `copper_layer_count`, `board_thickness_mm`, `finish`,
`impedance_controlled`, `edge_connector`, `has_edge_plating`, `count` and
`layers`, each layer as `get_board_stackup` gives one, less `enabled`,
`user_name` and a copper layer's `material`, and with the `color` the file
names; `source: "file"`, and `warning` when the file changed.

`get_board_stackup`'s `hint` and description now name `set_board_stackup` for
a closed board. No existing tool's arguments or responses change otherwise.
