### `connect_to_net` tries to keep its stub and label clear (minor release)

The tool tries 2.54, 5.08 and 7.62 mm when `stub_length` is omitted. With
`direction: "auto"` (or no `direction`), it tries the allowed lengths outward before
turning across. An explicit direction or length stays fixed. A clear outward 2.54 mm
stub stays where it was. With `stub_length: 0`, the label stays on the pin and no wire
is drawn. If the permitted directions give different label orientations, `auto` can
turn the label to clear nearby items.

Use `label` and `wire` in the reply instead of assuming a 2.54 mm stub. The new
`stub_length` gives the length used in mm, and `text_overlaps` lists clearance findings
introduced by this call. It can include `collinear_overlap` beyond the kinds
`check_schematic_overlaps` reports. The shared clearance rule also checks `pin_contact`
when symbol pins change; `connect_to_net` does not move or add symbol pins.
An empty list means no measured findings. Like `check_schematic_overlaps`,
`bounds_unresolved` lists references whose symbol bounds could not be checked.
Missing bounds do not by themselves prevent a write. As before, `wire.x1` and
`wire.y1` retain the input precision for coordinate calls. `stub_length` measures
the saved wire at KiCad's precision.

If every candidate has a finding, the tool writes the first electrically safe
candidate and reports its findings. If none is electrically safe, it still returns
`plan_blocked` without writing.

`batch_connect_to_net` and `connect_passthrough` are unchanged.
