# Real KiCad 3D-model fixtures

`royalblue.kicad_pcb` is an unmodified copy of KiCad 10.0.6's installed
`demos/royalblue54L_feather/RoyalBlue54L-Feather.kicad_pcb`, designed by
Lords Boards, with KiCad Project modifications dated February 17, 2025.
Its CERN-OHL-P v2 licence is reproduced in `LICENSE`. Keeping the complete
KiCad-written board preserves the embedded payloads and their footprint references.
It has six `kicad-embed://` models, footprint-level embedded-file metadata,
board-level embedded payloads, DNP footprints, and external KICAD8 model paths.

Copied without reserialization; no hand-written board fixture is used.

Observed on KiCad 10.0.6:

```sh
kicad-cli pcb export step --subst-models -o /tmp/royalblue.step royalblue.kicad_pcb
```

The export succeeds. With KICAD8_3DMODEL_DIR unset, it reports 48
`Could not add 3D model` lines for external models and none for the six
embedded models. The tests assert those same six are present and are never
reported broken, and remove each embedded owner in memory to test both scopes
and absence independently. The external finding count depends on installed libraries.
The CLI's STEP substitution and DNP export policy differ from the path-existence
audit; a READY verdict is not a guarantee of successful model decoding/export.

`resistor.step` is KiCad's stock `Resistor_SMD.3dshapes/R_0805_2012Metric.step`
(KiCad library, CC-BY-SA-4.0 with the KiCad libraries exception).
`adjustable_rx2v4.wrl` is copied from KiCad's
`demos/pic_programmer/libs/3d_shapes/adjustable_rx2v4.wrl` (Wings 3D export).
They supply actual serialized STEP and VRML data for the search tests, which
otherwise only inspect filenames. Format acceptance expectations are written
independently from KiCad's OCE, VRML and IDF plugin filter lists.
