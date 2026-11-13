### `update_pcb_from_schematic` gives footprints their sheet name, sheet file and footprint filters (minor release)

A footprint the sync placed had no sheet name, sheet file or footprint
filters until KiCad's own Update PCB ran, and KiCad's rule areas generated
from sheets, and Repeat Layout with them, find a sheet's footprints by their
sheet name ([#237](https://github.com/Copperpour/Copperpour/issues/237)). The
sync now sets all three as Update PCB does: the symbol's sheet path, its
sheet's file and its library symbol's footprint filters.

Each planned `add` and `update` in `changes` carries them as `sheet_name`,
`sheet_file` and `footprint_filters`. A footprint whose three differ from the
schematic's is an `update`, so the first sync of a board an earlier sync built
plans an update of every footprint that sync placed. A footprint that owns
groups, points or variants and differs only in these three leaves the plan
instead, with a `footprint_sheet_not_updated` diagnostic that refuses nothing:
KiCad's Update PCB sets its sheet in place. One that differs in anything else
as well is a `footprint_children_not_carried` conflict, as before. The
post-apply readback reports a sheet the board did not keep as
`board_readback_differs`.
