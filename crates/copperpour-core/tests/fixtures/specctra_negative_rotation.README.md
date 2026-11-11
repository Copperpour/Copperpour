# Negative-angle Specctra fixture

`specctra_negative_rotation.kicad_pcb` is KiCad 10.0.8's serialization of
`specctra_two_resistors.kicad_pcb` with four footprints at negative angles,
built by `specctra_negative_rotation.generate.py` with KiCad's `pcbnew`
Python (run it with `/usr/bin/python3` on a machine with KiCad's bindings;
UUIDs may differ on regeneration):

| Ref | Set with `SetOrientationDegrees` | Written by KiCad | Rust DSN `place` | Freerouting 2.3.0 SES `place` |
|-----|----------------------------------|------------------|------------------|-------------------------------|
| R1 | -90 | `(at 100 50 -90)` | `front -90` | `front 270` |
| R2 | 0 (unchanged) | `(at 110 50)` | `front 0` | `front 0` |
| R3 | -45 | `(at 100 40 -45)` | `front -45` | `front 315` |
| R4 | -180 | `(at 110 40 180)` | `front 180` | `front 180` |

KiCad keeps a footprint's angle in (-180, 180], so -180 is saved as 180.

`specctra_negative_rotation.freerouting-2.3.0.ses` is Freerouting 2.3.0's
session for the Rust exporter's DSN of that board (`-mp 2`; 0 unrouted, 0
violations). Only the session and base design names were set to `board`.
Freerouting writes every placement angle in [0, 360): the same directions as
the DSN, written differently.

KiCad's native DSN of this board is not kept: R3's pads at -45 degrees fall on
half micrometres, which `adopt_native_dsn` refuses
([#98](https://github.com/Copperpour/Copperpour/issues/98)).
