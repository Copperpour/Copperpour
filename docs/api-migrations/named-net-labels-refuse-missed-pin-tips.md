### Named net labels refuse anchors that miss pin tips (minor release)

`connect_to_net` and `batch_connect_to_net` no longer place a label or stub
at a named pin if KiCad would read its anchor away from the pin tip. This
can happen with a legacy saved symbol angle such as `90.5`. The single tool
returns `plan_blocked` without writing. The batch lists each skipped pin in
`errors` and saves any successful siblings. If it applies nothing and rejects
an anchor, it also returns `plan_blocked`. Correct or resave the symbol's
orientation in KiCad and retry only the skipped pins. Explicit coordinate
placement in `connect_to_net` is unchanged.

Nearby labels and wires no longer count as duplicates in the batch. If an
older call left an item off the tip, retrying after correcting the angle
puts the new connection on the pin and leaves the old item for you to remove.
