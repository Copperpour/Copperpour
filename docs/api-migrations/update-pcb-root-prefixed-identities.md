### `update_pcb_from_schematic` matches equivalent root-prefixed schematic identities (patch release)

`update_pcb_from_schematic` now normalizes only the requested root schematic's
UUID prefix (`/<root_uuid>/...`) when comparing exported root-relative netlist
identities against stored PCB footprint `symbol_path` values.

- Existing footprints carrying either `/<root_uuid>/<sheet...>/<symbol>` or
  `/<sheet...>/<symbol>` match the exported component (including reference
  renames, unassigned components, and `on_board=no` exclusions) and retain
  their stored PCB `symbol_path` and pose on updates.
- Newly added footprints continue to receive native root-relative paths from
  the netlist export.
- Foreign root UUIDs, other sheet instances, other symbol UUIDs, and boards
  carrying simultaneous rooted and root-relative aliases for the same identity
  remain refused (`reference_identity_conflict` / `duplicate_board_identity`)
  before any PCB mutation.

No caller changes are required.
