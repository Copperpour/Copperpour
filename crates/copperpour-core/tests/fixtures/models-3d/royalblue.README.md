# Real KiCad 3D-model fixtures

`royalblue.kicad_pcb` is an unmodified copy of KiCad 10.0.6's installed
`demos/royalblue54L_feather/RoyalBlue54L-Feather.kicad_pcb`, designed by
Lords Boards, with KiCad Project modifications dated February 17, 2025.
Its CERN-OHL-P v2 licence is reproduced in `LICENSE`. Keeping the complete
KiCad-written board preserves the embedded payloads and their footprint references.
It has six `kicad-embed://` models, footprint-level embedded-file metadata,
board-level embedded payloads, DNP footprints, and external KICAD8 model paths.

Copied without reserialization; no hand-written board fixture is used. The
reviewer confirmed that KiCad 10.0.8 ships the same board bytes.

Observed on KiCad 10.0.6:

```sh
kicad-cli pcb export step --subst-models -o /tmp/royalblue.step royalblue.kicad_pcb
```

The export succeeds. The recorded KiCad 10.0.6 run reported 48 external-model
errors; the KiCad 10.0.8 review environment without stock 3D libraries reported
49. Neither run reported an error for an embedded model. These counts depend on
installed libraries and path configuration and are not test expectations.

The audit test asserts only library-independent facts: J4, U1, Y2, J6 and J5
have only embedded models and are never missing or broken; no `kicad-embed://`
path is reported broken. D3 has both an embedded model and the external
`LED_Wurth_150044M155260.wrl`, so D3 may be broken through that WRL while its
embedded model is present. The test does not require six fully OK footprints.
A separate test removes each embedded owner in memory to test both scopes and
absence independently.
The CLI's STEP substitution and DNP export policy differ from the path-existence
audit; a READY verdict is not a guarantee of successful model decoding/export.

`resistor.step` is KiCad's stock `Resistor_SMD.3dshapes/R_0805_2012Metric.step`
(KiCad library, CC-BY-SA-4.0 with the KiCad libraries exception).
`adjustable_rx2v4.wrl` is copied from KiCad's
`demos/pic_programmer/libs/3d_shapes/adjustable_rx2v4.wrl` (Wings 3D export).
They supply actual serialized STEP and VRML data for the search tests, which
otherwise only inspect filenames. Format acceptance expectations are written
independently from KiCad's OCE, VRML and IDF plugin filter lists.
