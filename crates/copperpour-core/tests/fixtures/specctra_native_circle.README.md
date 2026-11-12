# Native export with a circular Edge.Cuts cutout (#72)

`specctra_native_circle.kicad_pcb` is the existing KiCad-authored two-resistor
fixture with a circular Edge.Cuts cutout added at (120, 60) mm, radius 2 mm.
`specctra_native_circle.generate.py` creates the shape and saves/exports the board
using KiCad 10.0.6’s native `pcbnew` API. No serialized design text was patched.
The source `.kicad_pro` was produced by KiCad and records its default routing
rules: 0.2 mm track/clearance, 0.6/0.3 mm via. Both nets use Default.

The native DSN retains KiCad’s actual outer boundary and polygonized circular
cutout as a keepout. Only the environment-dependent root `(pcb ...)` identifier
was normalized to `board.dsn`; native geometry, pin names, classes and wiring
are unchanged. The Rust writer deliberately refuses the board’s `gr_circle`.

`specctra_native_circle.native-kicad-10.freerouting-2.5.0.ses` was produced with:

```bash
java -Djava.awt.headless=true -jar /path/to/freerouting-2.5.0.jar \
  -de specctra_native_circle.native-kicad-10.dsn \
  -do specctra_native_circle.native-kicad-10.freerouting-2.5.0.ses -mp 2
```

Freerouting reported zero unrouted connections and zero violations. Its session
has five straight tracks and no vias. The session/design names are left as
Freerouting wrote them; the importer accepts that self-consistent transport alias
while binding the board separately by canonical path and exact snapshot hash.

`specctra_native_circle.native-applied.kicad_pcb` is KiCad’s own import of that
session, created by `pcbnew.ImportSpecctraSES` followed by `pcbnew.SaveBoard`.
It is an independent geometry/layer/net oracle for the served Copperpour apply
test. Native track UUIDs and ordering can differ from IPC-created items.
Reproduce it after routing with the native API on a disposable board copy.

The served bridge test uses authenticated loopback HTTP and an isolated child
process registration directory; IPC is a bound double. The apply test checks a
stale manifest refuses before mutation, then compares five created tracks with
this native import. Its default DRC response is a stub for control-flow coverage.
The ignored `native_profile_circle_manifest_apply_with_native_drc` test runs real
`kicad-cli` DRC on the candidate. Neither mock test claims a real GUI transaction
or cross-platform native-plugin observation.

Real KiCad 10.0.6 DRC on disposable copies found two `lib_footprint_issues`
warnings both before and after routing; unconnected items fell from two to zero.
The fixture is not claimed DRC-clean, and no warning was suppressed to make the
test pass. The warnings are unrelated to the new circular cutout or route geometry.

Freerouting’s original trailing spaces in the SES are preserved with its output
bytes; source/document whitespace checks exclude this one native SES artifact.
