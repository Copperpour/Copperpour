### Verify live PCB readback before reporting success (minor release)

An empty, correctly typed `GetNets` reply still means there are no nets. A reply with a different protobuf type does not.

Deleting a trace, via, or footprint requires a readable inventory of the bound board showing that the original item is gone. An unreadable inventory does not prove absence. A footprint delete sends no write if the requested reference is empty, missing, or ambiguous, or if the inventory has an unreadable footprint reference. A KiCad status that prevents the initial read is retained as evidence in the no-write refusal.

After KiCad acknowledges a write, Copperpour checks the assigned KIID and pose for initial footprint creation, and the original KIID and pose for a single-footprint move. Via placement checks the assigned KIID, position, and net before reporting success. On a net mismatch it requires a readable position and keeps the existing cleanup behavior. Via moves check the original KIID, destination, and observed net. If required readback cannot verify the write, Copperpour returns `ipc_outcome_unknown` with the board and any known item identity. Inspect the live board before retrying.

These checks do not verify via dimensions, footprint children or groups, the subsequent flip or model lookup during placement, batch alignment, or straight-trace creation. Write acknowledgement handling and rollback policy are unchanged.
