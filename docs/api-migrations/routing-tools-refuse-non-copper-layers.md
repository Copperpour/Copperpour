### Routing tools refuse a `layer` that is not copper (patch release)

`route_trace`, `route_pad_to_pad`, `modify_trace` and `route_differential_pair`
now refuse a `layer` other than `F.Cu`, `B.Cu` or `In1.Cu`..`In30.Cu`, spelled
as KiCad writes it, as `invalid_argument` on `layer`, before KiCad is asked
([#205](https://github.com/Copperpour/Copperpour/issues/205)). They used to
send an unknown name such as `F.Silk` or `In0.Cu` to KiCad as `BL_UNDEFINED`,
and put a track on a non-copper layer such as `F.SilkS` and report success.
`modify_trace` now refuses before it deletes the old track. Pass a copper layer
name.
