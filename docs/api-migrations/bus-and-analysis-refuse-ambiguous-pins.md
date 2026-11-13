### Bus connections and pin analysis require a unique placed pin (minor release)

`get_pin_connections` and its alias `get_pin_net_name` now return
`ambiguous_target` with candidates (unit, UUID and position) when a reference and
pin number name more than one placed pin. Missing components, pins or embedded
symbol definitions return `stale_target`. Previously these tools reported the
first matching pin, or a free-text missing-target error.

`connect_pins_to_bus` uses the same strict resolver and honors sheet-local
`lib_name` definitions. Ambiguous or stale items are skipped and described in
`errors`; valid items still connect. A call with no valid items leaves the file
unchanged. Its arguments and response shape are unchanged.

Give placed symbols unique references before retrying ambiguous items. For a
pin shared by multiple placed units, use explicit-coordinate wiring and bus
entry tools for the intended candidate instead. Retry only skipped bus items;
successful items have already been applied.
