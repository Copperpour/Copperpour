### `set_footprint_metadata` accepts `allow_soldermask_bridges` (patch release)

`attributes` now accepts `allow_soldermask_bridges`, which KiCad writes in a
footprint's `(attr …)` block, as on its stock solder jumpers. It was refused, so
an attributes edit on such a footprint removed the flag
([#119](https://github.com/Copperpour/Copperpour/issues/119)). `attributes` still
replaces the whole block: list the flag to keep it. Leaving it out drops it
without the warning given for an attribute the argument does not accept;
`changes` still shows it under `previous`.
