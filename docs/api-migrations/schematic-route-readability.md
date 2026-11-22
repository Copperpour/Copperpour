### Automatic schematic routing prefers the clearer elbow (minor release)

`connect_pins`, `add_schematic_connection`, and `batch_connect_pins` now prefer the
elbow that crosses fewer symbol bodies, then fewer label and field text runs. A tie
still favors horizontal-first. An elbow that would join another net is never used,
and straight routes are unchanged.

The same endpoints can now produce a vertical-first elbow even when horizontal-first
is electrically safe. Read `path` in `connect_pins` and `add_schematic_connection`
results instead of assuming the corner. `batch_connect_pins` still reports counts
and errors; inspect the sheet if you need the drawn paths.
