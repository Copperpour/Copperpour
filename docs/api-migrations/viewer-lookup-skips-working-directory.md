### `open_schematic_viewer` looks for the viewer next to the server, then on `PATH` (patch release)

The viewer was looked for next to the server binary, then as
`schematic-viewer` or `schematic-viewer.exe` in the server's working
directory. A file found there was returned as a bare name, which the spawn
looked up on `PATH`, so `viewer` named something other than the binary that
ran, or the launch failed. The second step is now a `PATH` lookup over
absolute entries, for the platform's file name only, and `viewer` is the full
path launched. Keep the viewer next to the server binary, as the release
packages install it, or put its directory on `PATH`. The not-found error now
names both places
([#184](https://github.com/Copperpour/Copperpour/issues/184)).
