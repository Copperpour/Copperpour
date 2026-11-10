### The wiring tools refuse a pin that two placed symbols share (minor release)

`connect_to_net`, `connect_pins`, `batch_connect_to_net` and
`batch_connect_pins` resolved `reference` + `pin_number` to the first placed
symbol carrying the pin, in file order, and reported success
([#88](https://github.com/Copperpour/Copperpour/issues/88)). A duplicated
reference, or a pin drawn on more than one placed unit of one symbol, now names
more than one pin and is refused, as `add_power_symbol` already refused it.

`connect_to_net` and `connect_pins` return `ambiguous_target`, with `target`
(`R4 pin 1`) and `candidates` naming each pin by unit, UUID and position, and
write nothing. Give `connect_to_net` the chosen pin's `pin_x` + `pin_y`; wire
`connect_pins`' ends with `add_schematic_connection`. A missing component or pin
is now `stale_target` with a `reason` (`component R99 is not present`,
`U1 has no pin 9 on placed units 1, 3`) instead of a plain-text error. So is a
reference with a placed unit whose library definition is not embedded: that
unit may be the one carrying the pin, so it is no longer skipped.

The batch tools skip such a pin and list it in `errors`
(`R4 pin 1 names more than one placed pin: …`), and the rest of the batch still
lands, as for a pin that does not resolve. Their other `errors` lines now read
`R99 pin 1: component R99 is not present` instead of
`Component 'R99' not found`.

`add_power_symbol`'s `reason` for a missing pin now names the units searched:
`R1 has no pin 9 on placed unit 1`.
