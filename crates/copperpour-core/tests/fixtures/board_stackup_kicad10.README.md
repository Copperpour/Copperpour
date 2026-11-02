# A two-layer board with a stackup KiCad saved (issue #78)

`board_stackup_kicad10.kicad_pcb` is `board_outline_kicad10.kicad_pcb` (see its
README) with a `(stackup …)` block and the board thickness it sums, as **KiCad's
own save**: the block was added to a copy as text, and kicad-cli loaded and
saved the copy. The save is byte-identical to the text it was given, so the
block is KiCad 10's layout as well as its reading.

## Provenance

1. `board_outline_kicad10.kicad_pcb` was copied, `(thickness 1.6)` under
   `(general …)` became `(thickness 1.582)`, and this block became the first
   item of `(setup …)`, a tab more indented than it, one item a line:

   ```text
   F.SilkS       Top Silk Screen      color White, material "Liquid Ink"
   F.Paste       Top Solder Paste
   F.Mask        Top Solder Mask      color Green, 0.012 mm, material Epoxy, εr 3.5, loss tangent 0.025
   F.Cu          copper               0.035 mm
   dielectric 1  core                 color "FR4 natural", 1.488 mm locked, FR4, εr 4.6, loss tangent 0.02
   B.Cu          copper               0.035 mm
   B.Mask        Bottom Solder Mask   as F.Mask
   B.Paste       Bottom Solder Paste
   B.SilkS       Bottom Silk Screen   color White
   (copper_finish "ENIG") (dielectric_constraints yes) (edge_connector bevelled) (edge_plating yes)
   ```

   The materials are only names to keep; KiCad stores any text there.
2. `kicad-cli pcb upgrade --force` saved the copy in place.

Measured against **KiCad 10.0.6** on Ubuntu 24.04 (Linux).

## What the tests rely on

Every value above that `set_board_stackup` does not set: the mask's material,
εr and loss tangent, the silkscreen's material, the dielectric's color and
thickness lock, the edge settings. They stand where a call leaves them; the
thickness, 0.012 + 0.035 + 1.488 + 0.035 + 0.012 mm, is the stack's sum.

## sha256

`d53a117f67a06bf19328fdf282812ec58f853141b279bd842d5ccd21ff0e7a96`

## Regenerating

Repeat the two steps on a fresh `board_outline_kicad10.kicad_pcb`. The UUIDs
are that board's, unchanged.
