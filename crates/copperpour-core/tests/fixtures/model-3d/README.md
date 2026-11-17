# STEP generator fixtures

These are original fixtures authored for Copperpour's #543 tests, generated
by `generate.py` through KiCad's pcbnew API (KiCad 10.0.6). No third-party
component/model assets are included; these fixtures have the repository's
AGPL-3.0 license. Regenerate with the system Python containing pcbnew:

```
/usr/bin/python3 crates/copperpour-core/tests/fixtures/model-3d/generate.py
```

UUIDs change when regenerated; no test depends on them. Files are kept as
native KiCad output, without line-ending or S-expression normalization.

- `outlines.pretty/rectangle-fab.kicad_mod`: F.Fab is 2 × 1.25 mm centred at
  zero; the different F.CrtYd rectangle proves F.Fab takes precedence.
- `rectangle-courtyard.kicad_mod`: only F.CrtYd, from (1,2) to (5,8) in
  footprint coordinates; the model must span X [1,5], Y [-8,-2].
- `unsupported-fab.kicad_mod`: F.Fab circle plus rectangular F.CrtYd. The
  unsupported Fab must refuse, rather than fall back to the courtyard.
- `assembly.kicad_pcb`: 20 × 20 mm board, nominal stack-up thickness 1.6 mm,
  one top-side footprint U1 at (10,10) with a STEP reference to
  `${KIPRJMOD}/generated.step`. The integration test copies it unchanged into
  a temporary directory and generates that STEP alongside it.

The kernel oracle fixes its expectations independently: body 4 × 6 × 2 mm
at Z=0.3; two rows of three pins, centred at Y=±3.4 with X=-1.5,0,1.5;
each pin 0.4 × 0.8 × 0.3 mm at Z=0. Total raw solid volume is 48.576 mm³.
Colours: body RGB (0.2,0.3,0.4), pins RGB (0.75,0.75,0.75).
KiCad's stack-up policy controls the absolute component Z in the assembly;
this isn't treated as a model-generator dimension. No generated STEP or
exported assembly is committed.
