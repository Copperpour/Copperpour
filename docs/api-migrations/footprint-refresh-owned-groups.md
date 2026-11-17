### Library refresh refuses footprints that own groups (patch release)

`update_footprints_from_library` now reads the live board before it reports or applies a replacement, and returns `status: "conflict"` with `footprint_children_not_carried` if a footprint it would replace owns a group, which KiCad 10.0.8's API drops. Nothing in the plan is applied. Refresh such footprints with KiCad's Update Footprints from Library. A footprint that is only a member of a board-level group is unaffected.
