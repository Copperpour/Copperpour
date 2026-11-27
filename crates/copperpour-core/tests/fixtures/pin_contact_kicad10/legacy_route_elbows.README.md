# Pin routing around legacy mirror angles

Create a blank `legacy_route_elbows.kicad_sch` with `create_schematic`. Place four
`Device:R` symbols with `add_schematic_component`:

| Reference | X (mm) | Y (mm) | Rotation | Mirror |
| --- | ---: | ---: | ---: | --- |
| R30 | 25.4 | 21.59 | 180° | None |
| R31 | 38.1 | 41.91 | 0° | None |
| R32 | 41.91 | 25.4 | 90° | x |
| R33 | 21.59 | 38.1 | 90° | y |

Save it with `kicad-cli sch upgrade --force` using KiCad 10.0.8. KiCad writes R33
as `270°`/`x` and leaves R32 at `90°`/`x`. The test changes only the instance
orientations to `90.5°`/`x` and `90.5°`/`y`, respectively, on a disposable copy.
Resaving that copy would erase the fractions the test needs.

KiCad 10.0.6 and 10.0.8 put R32.1 at the horizontal-first elbow and R33.1 at the
vertical-first elbow of a route from R30.1 to R31.1. Both routes must be refused.
