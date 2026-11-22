### Automatic schematic routing allows local label baselines (patch release)

`connect_pins`, `add_schematic_connection`, and `batch_connect_pins` no longer
count a wire along a local label's baseline as a text crossing. These tools may
choose a different elbow for the same endpoints. Read the returned `path` on
single connections, or inspect the saved schematic after a batch.
