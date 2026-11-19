### Power symbols take the library's reference prefix, so `PWR_FLAG` is `#FLG` (patch release)

`add_power_symbol` and `batch_add_power_symbol` gave every symbol they placed a
`#PWR` designator. KiCad's `power:PWR_FLAG` has the `Reference` `#FLG`, and
eeschema numbers `#FLG01`, `#FLG02`, … apart from `#PWR`, so a flag disagreed
with the library definition the tool had just embedded and used up a `#PWR`
number.

The prefix is now the embedded library symbol's `Reference`, as eeschema takes
it when it places a symbol, and each prefix is numbered on its own: the lowest
number free among that prefix's designators across the hierarchy. A
`PWR_FLAG` gets `#FLG01` and a `GND` placed after it still gets `#PWR01`. A
library symbol with no `Reference` prefix still gets `#PWR`. Flags already
placed as `#PWR` are left as they are, and their numbers stay held in the
`#PWR` series. Issue:
[#110](https://github.com/Copperpour/Copperpour/issues/110).
