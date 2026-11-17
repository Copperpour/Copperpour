# Approximate package STEP models

Load `model_3d`, then call `create_3d_model` to generate an offline AP214 STEP
file. This first increment uses Copperpour-owned planar B-rep code, with no
new crate or external CAD runtime dependency. It approximates packages with
independent box solids; it does not model manufacturing detail or fuse solids.
Cylinders, chamfers, arbitrary polygons and Boolean operations are outside
this increment. Revisit a geometry-kernel dependency before expanding there.

```json
{
  "output": "/project/3dmodels/package.step",
  "name": "package",
  "body": {
    "width_mm": 4,
    "length_mm": 6,
    "height_mm": 2,
    "standoff_mm": 0.3,
    "color": [0.2, 0.3, 0.4]
  },
  "pin_rows": [
    {
      "axis": "x", "count": 3, "pitch_mm": 1.5,
      "width_mm": 0.4, "length_mm": 0.8, "height_mm": 0.3,
      "center_x_mm": 0, "center_y_mm": -3.4
    }
  ]
}
```

The output parent directory must already exist. `.step` and `.stp` extensions
are accepted, case-insensitively. Existing destinations, including directories
and symlinks, are refused with `conflict`; no overwrite option is provided.
A verified create-only atomic write protects publication. A failed readback
returns `mutation_outcome_uncertain`: inspect the destination before retrying.

Coordinates are millimetres, with X right, Y up and Z above the PCB surface.
An explicit body is centred on XY=0; its bottom is `standoff_mm` (default 0).
Pin rows are centred on their supplied XY coordinates and distributed along
`axis` using `pitch_mm`; pin width/length remain aligned with X/Y regardless
of the row axis. `bottom_z_mm` defaults to 0 and may be negative. Body colour
defaults to `[0.15,0.15,0.15]`, pins to `[0.75,0.75,0.75]`; colours are RGB
components in [0,1]. Name defaults to `package` and accepts 1–128 printable
ASCII characters except backslash; apostrophes are escaped in STEP strings.
Malformed present values are refused, not replaced by defaults.

Dimensions and pitch must be at least 0.001 mm. All box endpoints must lie
within ±10000 mm. At most 16 rows, 128 pins per row and 256 pins in total are
accepted. Model bounds are validated before writing. Boxes may touch or
overlap; they remain separate solids, so summed solid volumes are not a
Boolean-union volume and overlap should not be used to estimate material.

## Body from a saved footprint

Replace explicit `width_mm`/`length_mm` with `footprint_path`:

```json
{
  "output": "/project/3dmodels/from-footprint.step",
  "body": {
    "footprint_path": "/project/custom.pretty/package.kicad_mod",
    "height_mm": 2
  }
}
```

The source is a saved library footprint, not live editor state. The tool reads
one `fp_rect` or four exactly closed axis-aligned `fp_line` edges on F.Fab.
Only when F.Fab has no outline does it try F.CrtYd. It refuses multiple/open/
rotated outlines, circles, arcs, polygons and malformed geometry; it does not
silently bound unsupported geometry or skip a bad F.Fab to use the courtyard.
Many stock KiCad IC footprints have a chamfered F.Fab corner and are deliberately
refused; pass explicit body dimensions for those footprints. An additional
F.Fab pin-1 mark also makes the outline ambiguous and is refused.
Text and graphics on other layers are ignored. Footprint Y-down coordinates
are converted to model Y-up coordinates, preserving off-centre placement.
Courtyards include clearance and are only an approximation of the body.

The response identifies `body_source`, the chosen layer and saved path when
applicable, plus the generated solids' bounds/colours. `attached: false` is
explicit: the source footprint is never changed. Attach the result separately
with `set_footprint_models`, or `set_placed_footprint_models` for a live board.
Keep generation and attachment as separate ordered operations. Generation is
write-gated through `call_tool` just like other artifact-producing tools.

## Validation and independent oracle

Runtime success proves exact file readback, not CAD-kernel acceptance or
mechanical accuracy. Default tests cover topology references/shared edges,
outward face planes and contiguous loops, geometry planning, served schemas, call_tool policy, source precedence,
unchanged sources, conflict preservation and uncertain write outcomes.

The optional integration test generates a seven-solid body/pin model through
served MCP dispatch, copies an unchanged KiCad-authored board fixture, runs
`kicad-cli pcb export step`, and checks both model and exported assembly using
an independent OpenCASCADE reader:

```sh
python3 -m venv /tmp/step-oracle
/tmp/step-oracle/bin/pip install -r scripts/requirements-step-oracle.txt
PATH="/tmp/step-oracle/bin:$PATH" cargo test -p copperpour-core --locked \
  --test model_3d_contract step_model_kicad_assembly_and_independent_occt_oracle \
  -- --ignored --nocapture
```

These are development-only dependencies. Missing KiCad/OCCT causes that
explicitly requested test to fail, not report a pass; the default suite marks
it ignored. CAD readers can repair incorrectly oriented input on import, so the topology
unit tests separately enforce the original file's orientation and loop invariants.
The oracle checks the imported B-rep validity, per-solid bounding boxes and
volumes, raw STEP colours, and preservation of all body/pin solids in the
export. It does not reuse the writer's calculations or response fields.
KiCad owns the component's absolute Z placement relative to its stack-up;
the assembly check allows one common Z translation while retaining exact
model dimensions and relative placement. The raw-model check independently
pins the requested standoff. The substrate itself is not generated here.

Fixtures and native regeneration instructions are in
`crates/copperpour-core/tests/fixtures/model-3d/README.md`.

## Implementation references

The writer is original Copperpour code, not a copied model or third-party
exporter. It follows the AP214 representation chain and the shared
vertex/edge, closed-shell and outward planar face conventions:

- [STEP Tools: manifold_solid_brep](https://steptools.com/stds/stp_aim/html/t_manifold_solid_brep.html)
- [OpenCASCADE STEP exchange documentation](https://github.com/Open-Cascade-SAS/OCCT/wiki/step)
- [KiCad package generator](https://gitlab.com/kicad/libraries/kicad-packages3D-generator)
  as a reference for the independent kernel-validation approach, not a runtime dependency.
