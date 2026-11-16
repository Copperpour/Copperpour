### Power symbols are numbered across the whole hierarchy (minor release)

`add_power_symbol` and `batch_add_power_symbol` gave a new `#PWR` the lowest
number free on the one sheet file, so a number already held on another sheet of
the project was issued again, and a sheet placed more than once got the same
designator on every instance. eeschema checks annotation over the whole
hierarchy, power symbols included: the netlist export warns that the schematic
has annotation errors.

The number is now the lowest free among every `#PWR` this sheet holds and the
project's instance records on every other sheet file of the hierarchy, read as
`annotate_schematic` reads them. Each instance of a sheet placed more than once
gets its own number, in hierarchy order as eeschema numbers its pages, and the
`Reference` property carries the one eeschema saves for that file, the record
of its first instance in page order, so the lowest. A compact reply gives that
property; the per-instance numbers are in the file and in the `verbose` reply's
`instance_references`. A batch numbers each symbol after the previous one.

A sheet no project owns is numbered against itself alone, as before. The reply
now has `other_sheets_consulted` and `other_sheets_unreadable`, the keys
`annotate_schematic` uses. A sheet file that could not be read is named in
`other_sheets_unreadable`, and its numbers were not seen. When the hierarchy's
page order could not be loaded, the file it stopped at is named there too, the
other sheets' numbers are still taken, and the instance paths are numbered in
sorted path order instead of page order. Issue:
[#505](https://github.com/Copperpour/Copperpour/issues/505).
