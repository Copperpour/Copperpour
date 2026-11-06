### `update_footprints_from_library` no longer replans `graphics` after an apply for a polygon that repeats its first point (patch release)

892 of KiCad 10.0.8's stock footprints have a polygon whose last point
repeats its first. KiCad keeps that point when it loads a board from its
file, but drops it when refresh sends the footprint over the API. So after
an apply, every later dry run planned `graphics` for that footprint again
([#320](https://github.com/Copperpour/Copperpour/issues/320)). Refresh now
ignores a closed outline's repeated last point when it compares, and the dry
run after an apply plans no change. What refresh sends KiCad is unchanged.
