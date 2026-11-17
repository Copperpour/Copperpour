### `import_jlcpcb_part` attaches 3D models and reports partial failures (minor release)

`include_3d` still defaults to `false`. Non-boolean values are refused with
`invalid_argument` before downloading or writing anything.

With `include_3d: true`, the importer resolves the model UUID and transform from
EasyEDA's `outline3D` SVGNODE. It prefers STEP payloads with complete Part-21 envelopes, otherwise
converts OBJ to KiCad-scaled VRML (`.wrl`). Both paths require finite OBJ bounds
to establish the model's placement; a STEP-only download is refused rather than
attached at a guessed origin. STEP bytes are preserved, including non-UTF-8 strings. Envelope checks detect incomplete responses;
they are not a full CAD geometry validator. WRL fallback provides mesh
visualization, not a STEP solid for mechanical exports.

Models are saved under `${KIPRJMOD}/3dmodels/<footprint>-<content-hash>.<ext>`
with verified, create-only atomic publication. Identical regular files can be
reused. Conflicting files, directories and symlinks are never overwritten.
The generated footprint receives its converted offset/rotation through the
existing revision-checked `set_footprint_models` writer.

Successful responses include `model_3d.status: "attached"`, `uuid`, `format`,
`kicad_path`, `path`, `offset_mm`, and `rotation_degrees`. An absent model UUID
reports `status: "not_available"` without downloading or publishing a model.

A failed requested 3D stage now returns `isError: true` and `success: false`,
rather than an overall success with a discarded download/write error. Existing
structured `conflict` and `mutation_outcome_uncertain` errors are retained.
`model_3d` reports `status: "error"` or `"uncertain"`, the failing `stage`
(`download`, `prepare`, `publish`, or `attach`), model identity/path when known,
`retry_safe: false`, and recovery guidance. Unclassified writer errors are
conservatively uncertain; they do not prove that nothing changed.

The import is a multi-file operation, not one atomic transaction. Its symbol
and footprint may already have been created when the model stage fails.
`imported` identifies those paths, states that rollback was not attempted, and
reports schematic placement as `"not_attempted"`. A successfully published model
is retained if footprint attachment fails. Inspect the reported library files,
model and footprint before retrying; repair only the incomplete stage rather
than blindly repeating the import or assuming rollback.
