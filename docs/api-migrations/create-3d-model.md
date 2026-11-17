### Generate approximate package STEP models with `create_3d_model` (minor release)

The new `model_3d` toolset contains `create_3d_model`. It creates a new AP214
STEP file from an explicit rectangular body and optional pin-row boxes, or
from an unambiguous saved footprint F.Fab rectangle (F.CrtYd fallback only
when F.Fab has no outline) and a supplied height. No CAD runtime dependency,
network access or existing-tool rename is introduced.

Output uses millimetres, Y-up coordinates and independent, unfused solids.
The tool reports the source used and solid bounds/colours; it never attaches
the model or modifies the source footprint. Existing output paths are
refused with `conflict`; a failed post-write readback reports
`mutation_outcome_uncertain`. `call_tool` requires write permission.

Read [the usage and validation guide](../3d-model-generation.md) for exact
limits, approximation semantics, defaults, recovery and the independent
KiCad/OCCT test. Runtime success establishes exact file readback, not CAD
validation or physical fit. Existing callers need no changes.
