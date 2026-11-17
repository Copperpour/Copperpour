# 3D import regression fixtures

## Redistributed KiCad assets

- `resistor.step`: copied unchanged from KiCad's stock
  `Resistor_SMD.3dshapes/R_0805_2012Metric.step`.
  Upstream: https://gitlab.com/kicad/libraries/kicad-packages3D/.
- `SOT-23.kicad_mod`: copied unchanged from the installed KiCad footprint library,
  `Package_TO_SOT_SMD.pretty/SOT-23.kicad_mod` (generator version 20260206).
  Upstream: https://gitlab.com/kicad/libraries/kicad-footprints/.

Copyright: KiCad library contributors. These assets remain under CC-BY-SA-4.0
with the KiCad libraries exception; see LICENSE and
https://www.kicad.org/libraries/license/. They are not relicensed as Copperpour
code. They pin real serialized STEP bytes and KiCad-authored footprint syntax.

## Purpose-written test data

`placement.obj` is a tetrahedron authored for these regression tests, not an
EasyEDA model or a derivative of one. Its raw extrema are X [10,12], Y [20,24],
Z [-5,-1] mm. A 180-degree Z rotation gives X [-12,-10], Y [-24,-20], Z [-5,-1].
Centred on XY=0 with its bottom at Z=0, its expected offset is (11,22,5) mm.
The EasyEDA-format SVGNODE in the Rust tests is also purpose-written, with a
stale c_origin and a simple rectangular child outline. These test data have
Copperpour's repository license.

The stock STEP and purpose-written OBJ are deliberately different geometry:
the STEP tests establish format selection, exact-byte transport/publication and
footprint attachment. Hand-calculated OBJ cases establish transforms and mesh
conversion, including nonzero X/Y/composed rotations. Neither is claimed as a
cross-format mechanical alignment oracle or a live-board rendering check.

## Local observation, not redistributed assets

During development on 2026-11-17, C20526 metadata and model assets were obtained
from the JLCEDA/EasyEDA Official Library (https://lceda.cn/ and https://easyeda.com/).
The OBJ/STEP attachment tests passed with those real assets. The STEP contained
non-UTF-8 bytes, motivating byte-preserving publication. Their redistribution
license was not established, so no EasyEDA metadata/model payload is committed.
This observation supplements the reproducible committed tests; CI does not
claim to repeat it or to verify mechanical fit.
