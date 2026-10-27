### New KiCad files name Copperpour as their generator (patch release)

Schematics, boards, footprints and symbol libraries that Copperpour creates
(`create_project`, `create_schematic`, `create_footprint`, `create_symbol` and
the other tools that start a new file) now carry `(generator "copperpour")`
instead of `(generator "konnect")`. KiCad replaces the value with its own the
next time it saves the file, and nothing reads it back. The schematic viewer's
window is titled "Schematic Viewer — Copperpour" and uses the Copperpour icon.
