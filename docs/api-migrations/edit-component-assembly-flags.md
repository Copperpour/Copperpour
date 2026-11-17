### `edit_component` sets DNP and the BOM and position-file exclusions (minor release)

`edit_component` changed only a placed footprint's value. It now also takes
`dnp`, `exclude_from_bom` and `exclude_from_pos_files`, each optional; every
property not named is kept. Before, these flags could be set on a board only
through the schematic and `update_pcb_from_schematic`.

An edit is one KiCad undo step, and the answer is a fresh read of the
footprint, as `edit_footprint_field` answers. This applies to a value edit as
well, which before was neither a single undo step nor read back:

- The answer keeps `reference`, `value` and `footprint`, and adds `dnp`,
  `exclude_from_bom`, `exclude_from_pos_files`, `linked_to_schematic`,
  `status` (`applied` or `unchanged`), `applied`, `source`, `changed`,
  `before`, `undo` and `warning`.
- A request the footprint already holds sends nothing and answers `unchanged`.
- A fresh read that does not hold the request is a `mutation_outcome_uncertain`
  error instead of success.
- `warning` says that KiCad 10 replaced the footprint whole, and, for a
  footprint linked to a schematic symbol, that `update_pcb_from_schematic` sets
  `dnp` and `exclude_from_bom` from the symbol again.

A call naming no property reports the footprint, as before, with the new keys.

`reference`, `value` and `footprint` keep their names and formats (`footprint`
is `Library:Entry`, as `list_footprints` writes it), and a missing reference
or a footprint without a value field is refused with the same text as before.
One refusal is new: a reference that more than one footprint carries is
refused before anything is sent ("Footprint reference 'R1' is ambiguous:
KiCad returned 2 matches"); before, the first of them was reported or edited.
