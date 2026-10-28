### A relative `HOME` no longer reaches the working directory (patch release)

The remaining per-user lookups now treat a relative `HOME` (and, on Windows, a
relative `USERPROFILE`, `LOCALAPPDATA` or `ProgramFiles`) as no directory at
all, instead of resolving it against the server's working directory:

- Schematic and board edits refuse, as they already did with no local-data
  directory: the cooperative document lock lives below the home directory.
  Set `COPPERPOUR_STATE_DIR` to an absolute path to edit anyway.
- `check_freerouting` and `route_specctra_dsn` skip the KiCad plugin folders
  below a relative `HOME` or `USERPROFILE`, so `searched_locations` no longer
  lists them. This supersedes the earlier note that a relative value is
  searched below the working directory; it still holds for
  `KICAD*_3RD_PARTY` and `PATH`.
- KiCad install discovery (`kicad-cli`, the bundled libraries) and the native
  Specctra bridge skip the per-user locations.

On Unix, an unset `HOME` falls back to the account's home directory. Set
`HOME` to an absolute path to restore the old locations
([#179](https://github.com/Copperpour/Copperpour/issues/179)).
