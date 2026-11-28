### Wire deletion reads the wire's own UUID (patch release)

`delete_schematic_wire` and `batch_delete_schematic_wire` now find a wire by its
own UUID even if the saved UUID is unquoted or an earlier non-wire item shares
it. A UUID shared by multiple wires is not deleted. When nothing can be deleted
because of a duplicate wire UUID, these tools return `plan_blocked`. A batch
still applies its unambiguous requests and lists the blocked UUIDs in `errors`.
Repair duplicate UUIDs in the sheet before retrying those requests.
