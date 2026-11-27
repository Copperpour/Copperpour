Copied `exact`, moved only `TOLERANT_OUT` to `(134.6198, 88.9)` through
Copperpour, and resaved with `kicad-cli sch upgrade --force` in KiCad
10.0.8. The label is 0.0002 mm past the left end of the trunk at
X=134.62, with no perpendicular offset. Both KiCad 10.0.6 and 10.0.8
export J2.1 on `unconnected-(J2-Pin_1-Pad1)` and report `label_dangling`.
