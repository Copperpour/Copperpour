# `redundant-power.kicad_sch`

Built in a new `crowded` project using Copperpour's schematic tools and KiCad 10's
stock libraries. Keep U1, C2 and the +3V3 driver on U1 pin 2. Place a second +3V3
symbol (`#PWR99`) at `(139.7, 68.58)` and wire it to C2 pin 1 at `(139.7, 76.2)`.
The junction at C2 pin 1 connects the stub to the surviving supply wire.

Saved with KiCad 10.0.8: `kicad-cli sch upgrade --force crowded.kicad_sch`.
A second resave produced identical bytes. Tests copy the sheet as `crowded.kicad_sch`
beside the existing `crowded.kicad_pro` to keep the saved project name.
