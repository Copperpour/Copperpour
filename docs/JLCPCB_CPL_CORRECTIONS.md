# JLCPCB CPL corrections

KiCad and JLCPCB do not always use the same zero-degree orientation for a
package. A CPL can therefore pass JLCPCB's structural checks while an IC or
connector is physically rotated in Component Placements.

`export_manufacturing_package(fab_house="jlcpcb")` applies a small, versioned
built-in correction policy after KiCad exports the native component geometry.
Pass `jlcpcb_cpl_corrections_path` to add project rules without editing Copperpour.
Keep that JSON file with the KiCad project so the manufacturing decision is
reviewable and repeatable.

## Policy format

This example is illustrative: the board, designators and LCSC numbers are made up, and the rotations it shows prove nothing about a real part. The part decides how JLCPCB's model faces, so check the preview for your own part. A complete project file is in
[`examples/jlcpcb_cpl_corrections.example.json`](../examples/jlcpcb_cpl_corrections.example.json):

```json
{
  "schema_version": 1,
  "policy_id": "my-board-jlcpcb-v1",
  "provenance": "Checked by the project owner in JLCPCB Component Placements (2D, top), my-board rev B order preview",
  "rotation_insensitive_prefixes": ["Crystal_SMD_3215-"],
  "footprint_rules": [
    {
      "id": "tssop-8-lying-model",
      "footprint_exact": "TSSOP-8_4.4x3mm_P0.65mm",
      "rotation_degrees": 270,
      "evidence": [
        {
          "board": "my-board rev B",
          "designator": "U3",
          "footprint": "TSSOP-8_4.4x3mm_P0.65mm",
          "lcsc": "C0000001",
          "side": "top",
          "kicad_rotation_degrees": 0,
          "cpl_rotation_degrees": 0,
          "footprint_pin1": "top-left",
          "jlc_model_pin1": "bottom-left"
        }
      ]
    }
  ],
  "component_overrides": [
    {
      "id": "j1-usb-c-kicad-rotation",
      "designator": "J1",
      "rotation_degrees": 0,
      "note": "rotation_degrees is the correction added to KiCad's rotation, not the final CPL rotation: 0 keeps KiCad's 180. The SMD pins must land on the inner pads, opening at the board edge.",
      "evidence": [
        {
          "board": "my-board rev B",
          "designator": "J1",
          "footprint": "USB_C_Receptacle_HRO_TYPE-C-31-M-12",
          "lcsc": "C0000002",
          "side": "top",
          "kicad_rotation_degrees": 180,
          "cpl_rotation_degrees": 0,
          "footprint_pin1": "bottom-right",
          "jlc_model_pin1": "top-left"
        }
      ]
    }
  ]
}
```

Policy level:

| Key | Required | Meaning |
|---|---|---|
| `schema_version` | yes | `1` |
| `policy_id`, `provenance` | yes | Reported verbatim in every response |
| `rotation_insensitive_prefixes` | no | Footprint-name prefixes whose rotation does not matter; added to the built-in list (`R_`, `C_`, `L_`, `Fuse_`, `Fiducial_`, `TestPoint_`, `MountingHole_`) |
| `rotation_sensitive_prefixes` | no | Exceptions to the list above, whose rotation always matters; added to the built-in `R_Array_` (pin 1 of a resistor array matters, though it starts with `R_`) |
| `footprint_rules` | no | First match wins inside the file |
| `component_overrides` | no | One entry per designator |

Footprint rule (`footprint_rules[]`):

| Key | Meaning |
|---|---|
| `id` | Unique in the file |
| `footprint_prefix` **or** `footprint_exact` | Exactly one. Use `footprint_exact` when a longer name shares the prefix: `SOT-23` must not match `SOT-23-5` |
| `rotation_degrees`, `offset_x_mm`, `offset_y_mm` | Correction, default 0. `rotation_degrees` is **added** to KiCad's rotation; it is not the rotation written to the CPL |
| `status` | `verified` (default) or `unverified`. An unverified rule carries no correction: matching parts keep KiCad's rotation and are listed in `warnings` |
| `evidence` | Preview observations, see below |
| `note` | Free text, repeated in the warning of an unverified rule |

Designator override (`component_overrides[]`): `id`, `designator`,
`rotation_degrees`, `offset_x_mm`, `offset_y_mm`, `evidence`, `note`. As in a
footprint rule, `rotation_degrees` is a correction added to KiCad's rotation,
not the final CPL rotation: to keep a part KiCad rotated by 180 degrees as it
is, write `0`, not `180`. This is
how a project freezes the rotations it checked part by part: one override per
designator, kept in a versioned file next to the board. An override with
`rotation_degrees: 0` is meaningful: it states that KiCad's rotation was seen to
be right and silences the review warning for that part.

### Evidence

Each `evidence[]` entry records one look at the preview:

| Key | Meaning |
|---|---|
| `board`, `designator`, `footprint` | What was looked at (`footprint` optional) |
| `lcsc` | The LCSC part placed (optional, recommended): the part, not the footprint, decides which way JLCPCB's model faces |
| `side` | `top` (default). `bottom` is refused: the pin-1 arithmetic below assumes the top view, which mirrors a bottom-side part |
| `kicad_rotation_degrees` | The footprint's rotation in KiCad |
| `cpl_rotation_degrees` | The rotation in the CPL that was actually uploaded |
| `footprint_pin1` | Where pin 1 of the KiCad footprint lies from the part centre |
| `jlc_model_pin1` | Where pin 1 of the JLCPCB model lies (the magenta dot) |

Directions are `right`, `top-right`, `top`, `top-left`, `left`, `bottom-left`,
`bottom`, `bottom-right`, as seen in the 2D top view. On a long part whose pin 1
sits near the middle of an end (a 2x05 header), take the dominant side, `left`
rather than `bottom-left`. Record what you saw, even
when the part was wrong: Copperpour derives the correction the observation
proves, `footprint_pin1 − jlc_model_pin1 + cpl − kicad`, and refuses the policy
if it differs from the rule's `rotation_degrees`. A rule cannot then say one
thing while its own evidence says another.

`evidence` is optional in a project file. A test checks every observation of
the built-in rules.

## Matching and coordinates

JLCPCB assembly export requires the exact board open in KiCad with IPC enabled.
Copperpour asks KiCad for each pad's native axis-aligned bounding box in board
coordinates and uses the centre of their union, not the footprint anchor or a
rotated local bounding box. A footprint with no pads uses its observed anchor
and reports `no_pads_observed_anchor`. Missing geometry or a board change during
the reads refuses export before output creation; no anchor fallback is guessed.

Footprints excluded from position files (`exclude_from_pos_files`) and DNP
footprints are skipped during midpoint collection, matching the CPL export's
selection. Excluded artwork may share placeholder references such as `REF**`
without blocking assembly export, whether or not it has pads. Duplicate
references on position-included, populated footprints still refuse export;
excluding a footprint from the BOM alone does not exclude it from the CPL.

Gerbers, drills, enabled-layer selection, and CPL positions use the same captured
live board snapshot, including unsaved board edits. The original board is not
saved or modified. Project settings and the schematic BOM still come from their
saved files. Other manufacturers and exports without assembly retain saved-file
behavior. Direct `export_position_file` remains KiCad's anchor-based export.

Midpoint conversion precedes correction-policy offsets. Review old offsets that
compensated for anchor-based positions: retaining them can double-correct the
new midpoint. Copperpour preserves explicit user policies rather than guessing
which offsets are obsolete. Model-specific offsets can still be necessary.

Copperpour strips a KiCad library prefix such as `Package_SO:` before matching
`footprint_prefix`. Matching is case-sensitive. Precedence is deterministic:

1. exact `designator` in the project policy;
2. first matching project `footprint_rules` entry;
3. first matching built-in footprint rule;
4. no correction.

A matching rule with `status: "unverified"` stops the search: a project rule
can still override a built-in unverified family, but nothing falls through to
a broader built-in rule.

`rotation_degrees` is added for top-side parts and applied with the documented
bottom-side view transform. The result is normalized into `[0, 360)`.
`offset_x_mm` and `offset_y_mm` are additions in the exported board coordinate
axes; they are not local component-axis offsets.

Use a designator override when a specific LCSC/JLCPCB placement model differs
from other parts sharing the same KiCad footprint. Use a footprint rule only
after verifying that the correction is valid for every matched package in the
project.

## Required evidence

The tool response contains:

- `geometry`: observed anchor, native pad-box midpoint, and algorithm for every
  exported designator; these are separate from the policy correction evidence;
- `applied_corrections`: designator, footprint, side, policy/rule IDs, and the
  position and rotation before and after the correction;
- `unmatched_footprints`: every exported component for which no rule matched,
  with `review_required: false` only for `rotation_insensitive_prefixes`;
- `unverified_footprints`: components matched by an unverified rule;
- `policies`: the policy ID, schema version, provenance, and precedence; and
- `status: "PREVIEW_REQUIRED"` with `physical_validation: false`.

Both kinds also reach `warnings`: one entry per unverified rule, and one
listing every polarized or multi-pin footprint no rule covers. They do not make
the package incomplete, but the summary no longer says "No warnings" while a
part sits at KiCad's raw rotation unchecked.

An unmatched footprint is not automatically wrong, and a matched footprint is
not automatically right. Upload the Gerbers, BOM, and CPL together, open
JLCPCB's Component Placements view, and inspect every component—especially ICs,
connectors, diodes, LEDs, and other polarized parts. Only that preview validates
the selected JLCPCB component models for the order.

Copperpour does not bundle the GPL-3.0 JLCKicadTools correction database. The
built-in policy contains only narrowly scoped rules independently verified by
the Copperpour project and identifies its provenance in every response.

## Built-in rules

`copperpour-jlcpcb-cpl-v2`:

| Footprints | Correction | Seen on |
|---|---|---|
| `SOIC-8_3.9x4.9mm_P1.27mm*` | +270 | Konnect issue #518, U1 |

The SOIC-8 rule predates the bar below: one board, no LCSC number. It stays
`verified` for now and is re-evaluated in the rule-set change, since turning it
`unverified` would change the CPL of every SOIC-8 board.

Unverified, so warned about and left at KiCad's rotation:
`USB_C_Receptacle_HRO_TYPE-C-31-M-12`. Its +180 rule from #518 was withdrawn in
v2: on another board, with the KiCad 10 library footprint, KiCad's rotation was
already right and the rule turned the connector upside down. Pin it per project
with a designator override, as in the example above.

The built-in list grows only by rules anyone can check again: observations
from more than one board, naming the LCSC part, and a pattern no wider than the
parts observed. Until then a project keeps its own observations in its policy
file.
