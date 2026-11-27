# `carried-no-connect.kicad_sch`

Built in a new `crowded` project using Copperpour's schematic tools and KiCad 10's
stock libraries. The sheet keeps the `crowded` fixture's six-wire input jog and
only its C1, U1, and +5V driver. Place a no-connect on C1 pin 2 at `(63.5, 113.03)`.
Cleanup moves C1 and carries the same marker to `(114.3, 83.82)`.

Saved with KiCad 10.0.8: `kicad-cli sch upgrade --force crowded.kicad_sch`.
A second resave produced identical bytes. Tests copy the sheet as `crowded.kicad_sch`
beside the existing `crowded.kicad_pro` to keep the saved project name.
