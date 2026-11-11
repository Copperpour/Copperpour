### Schematic writes respect the root editor lock (patch release)

Every tool that writes a schematic sheet now also checks the KiCad editor lock of
the project that owns it, `~<project>.kicad_sch.lck` beside the root schematic,
before it writes. Eeschema locks only that root and saves every sheet from memory,
so a child sheet was written while the project was open, and the change was lost
on Eeschema's next save. A present or unreadable root lock now refuses the write
with `conflict` naming the root, and leaves every file unchanged.
`delete_schematic_component` refuses earlier, in its document binding, with
`stale_target`. A sheet that more than one project could own is refused while any
of those projects is open. If the owning project cannot be determined, for
example because its directory is unreadable, the write is refused as well.

Save and close Eeschema before retrying a refused write. Remove a stale lock only
after confirming no editor owns it. Annotation dry runs and calls that change
nothing remain read-only and can run while the project is open.
