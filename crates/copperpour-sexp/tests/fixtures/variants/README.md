# KiCad 10 native variant grammar — findings of record

Established empirically on KiCad 10.0.5 (2026-08-26), with `kicad-cli`
acceptance as the ground truth and the emitter format strings in
`_eeschema.dll` as the map. This fixture is the KiCad ecc83 demo with two
minimal, verified edits; the golden CSVs are real `kicad-cli sch export bom`
output over it.

## Where variant data lives

1. **`.kicad_pro`, top-level `"variants"` key** — a JSON array of variant
   *names* (`["Lite"]` here). This is the GUI's registry of known variants
   (`kicommon.dll` serializes it; `GetDefaultVariantName()` backs the
   implicit default). **`kicad-cli` does not consult it**: exports honor a
   schematic's variant clauses even when this key is absent (verified).
   Copperpour must still maintain it, for GUI interoperability.

2. **`.kicad_sch`, per symbol placement** — inside each symbol's
   `(instances (project "…" (path "/uuid" (reference …) (unit …) …)))`
   block, after `(unit N)`:

   ```
   (variant (name "Lite")
       (dnp yes)
       (field (name "Value") (value "22k"))
   )
   ```

   Emitter sequence in `_eeschema.dll` confirms the shape:
   `(path %s (reference %s) (unit %d)` → `(variant (name %s)` →
   `(field (name %s) (value %s))`.

## What a variant clause can express (verified per token)

| Clause | Result |
|---|---|
| `(dnp yes)` | HONORED — `${DNP}` reports DNP under `--variant`, empty under default |
| `(field (name "Value") (value "22k"))` | HONORED — **arbitrary field overrides are native**; the exported Value changes per variant. MPN/LCSC-style overrides need no sidecar. |
| `(in_bom no)` | Loads without error, but `${EXCLUDE_FROM_BOM}` did not change in the variant export — effect unverified, do not rely on it |
| `(exclude_from_bom yes)` | **Parse error** — "Failed to load schematic". The clause grammar is a strict token set, not a bag of symbol attributes |

## Behavioral notes for implementers

- **`kicad-cli --variant` never validates the name.** An undefined variant
  name silently exports the default (verified on a stock demo). Copperpour's
  variant tools must do their own name validation against the project
  registry — the exporter will not catch a typo.
- The default variant is implicit and protected: eeschema carries distinct
  "Cannot copy/delete/rename the default variant" errors and a reserved-name
  check.
- A parse error from an unknown clause token means writes must be
  fixture-tested against `kicad-cli` acceptance before shipping — an invalid
  token bricks the schematic for every KiCad tool, not just variants.

## Fixture provenance

Base files are KiCad's own ecc83 demo (KiCad-authored serialization,
CRLF + tabs preserved). The two edits (the `"variants"` array; R1's variant
clause) were hand-authored and then adjudicated by `kicad-cli sch export bom`
— see the goldens:

- `bom_default.golden.csv` — no `--variant`: `R1, 1.5K, fitted`
- `bom_lite.golden.csv` — `--variant Lite`: `R1, 22k, DNP`

A GUI-authored re-save pass (open in eeschema, save) is still wanted to
confirm KiCad's own serialization of these clauses byte-for-byte; until
then, CLI acceptance is the standing evidence.
