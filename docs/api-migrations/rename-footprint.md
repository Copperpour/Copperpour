### New tool `rename_footprint` in `library_maintenance` (minor release)

`rename_footprint(library_path, footprint_name, new_name)` renames a footprint
in its `.pretty` library, as KiCad's footprint editor does, and as agreed on
[#633](https://github.com/Copperpour/Copperpour/issues/633).

- **Write:** `new_name.kicad_mod` holds the footprint with the new name in its
  header, and the Value changed when it equals the old name; then
  `footprint_name.kicad_mod` is removed. Nothing else in the file changes. A
  rename that changes only the case rewrites and renames the one file.
- **Refused** as `invalid_argument` on `new_name`, with nothing written: a name
  already in the library, or one that differs from a footprint in the library
  only in case; an empty name, one with spaces at either end, or one with a
  character KiCad does not accept in a footprint name (`% $ < > " \ / :`, tabs
  or line breaks); on Windows also `* ? |` and device names such as `CON` or
  `NUL`. `footprint_name` not in the library is refused on `footprint_name`. A
  path that is not a `.pretty` directory is a handler error; a missing one is
  `file_not_found`.
- **Concurrent change:** if the old file changes before it is removed, the new
  file is removed again and the call answers `conflict`. A rename interrupted
  after writing the new file is finished by the next call
  (`finished_earlier_rename`).
- **Answer:** `previous_name`, `name`, `footprint_path`, `value` (`previous`
  and `current`) when the Value changed, and `kept_references`: symbols'
  Footprint fields and footprints placed on boards keep the old library id.
