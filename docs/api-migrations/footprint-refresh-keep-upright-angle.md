### Library refresh compares keep-upright text angles modulo 180° (patch release)

`update_footprints_from_library` now compares angles of `keep_upright` footprint user texts, custom properties, and reset `Reference`/`Value` fields modulo 180° instead of 360°. Back-side footprints flipped left-to-right (`FLIP_DIRECTION::LEFT_RIGHT`, KiCad 10's default) no longer plan spurious `graphics`, `metadata`, or `text_effects` updates when KiCad keeps a keep-upright text 180° from the angle refresh builds. Text with `keep_upright` disabled (`(unlocked yes)`) continues to compare modulo 360°.
