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

A complete project file is in
[`examples/jlcpcb_cpl_corrections.example.json`](../examples/jlcpcb_cpl_corrections.example.json):

```json
{
  "schema_version": 1,
  "policy_id": "my-board-jlcpcb-v1",
  "provenance": "Checked in JLCPCB Component Placements (2D, top), rev B preview",
  "rotation_insensitive_prefixes": ["Crystal_SMD_3215-"],
  "footprint_rules": [
    {
      "id": "tssop-8-lying-model",
      "footprint_prefix": "TSSOP-",
      "rotation_degrees": 270,
      "evidence": [
        {
          "board": "my-board rev B",
          "designator": "U402",
          "footprint": "TSSOP-8_4.4x3mm_P0.65mm",
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
      "id": "j201-usb-c-kicad-rotation",
      "designator": "J201",
      "rotation_degrees": 0,
      "note": "LCSC C165948: SMD pins on the inner pads, opening at the board edge."
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
| `footprint_rules` | no | First match wins inside the file |
| `component_overrides` | no | One entry per designator |

Footprint rule (`footprint_rules[]`):

| Key | Meaning |
|---|---|
| `id` | Unique in the file |
| `footprint_prefix` **or** `footprint_exact` | Exactly one. Use `footprint_exact` when a longer name shares the prefix: `SOT-23` must not match `SOT-23-5` |
| `rotation_degrees`, `offset_x_mm`, `offset_y_mm` | Correction, default 0 |
| `status` | `verified` (default) or `unverified`. An unverified rule carries no correction: matching parts keep KiCad's rotation and are listed in `warnings` |
| `evidence` | Preview observations, see below |
| `note` | Free text, repeated in the warning of an unverified rule |

Designator override (`component_overrides[]`): `id`, `designator`,
`rotation_degrees`, `offset_x_mm`, `offset_y_mm`, `evidence`, `note`. This is
how a project freezes the rotations it checked part by part: one override per
designator, kept in a versioned file next to the board. An override with
`rotation_degrees: 0` is meaningful: it states that KiCad's rotation was seen to
be right and silences the review warning for that part.

### Evidence

Each `evidence[]` entry records one look at the preview:

| Key | Meaning |
|---|---|
| `board`, `designator`, `footprint` | What was looked at (`footprint` optional) |
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

`evidence` is optional in a project file. Every built-in verified rule has
some, and a test checks them all.

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

`konnect-jlcpcb-cpl-v1`, observed on KiCad 10 library footprints:

| Footprints | Correction | Seen on |
|---|---|---|
| `SOIC-*` | +270 | #518 U1 (SOIC-8), grumpf-bord U202 (SOIC-16) |
| `SSOP-*`, `MSOP-*`, `TI_SO-PowerPAD-*`, `TQFN-*`, `VQFN-*`, `SOT-23-6`, `SW_DIP_SPSTx*`, `ESP32-WROOM-32E`, `IDC-Header_2x05_P2.54mm_Vertical` | +270 | grumpf-bord |
| `SOT-23`, `SOT-23-5` (exact names) | +180 | grumpf-bord |
| `D_SOD-123*`, `D_SMA*`, `D_SMB*`, `D_SMC*`, `LED_0603_*`, `LED_0805_*`, `CP_Elec_*`, `SW_Push_1P1T_XKB_TS-1187A`, `Relay_SPDT_Omron-G5Q-1`, `PhoenixContact_MC_*`, `PhoenixContact_MSTBA_*`, `RJ45_Hanrun_HR911105A*` | 0 | grumpf-bord |

Unverified, so warned about and left at KiCad's rotation: `TSSOP-*`, `QFN-*`,
`DFN-*`, other `IDC-Header_*`, and
`USB_C_Receptacle_HRO_TYPE-C-31-M-12`. The HRO rule (+180) from #518 was
withdrawn: on grumpf-bord, with the library footprint (pad A1 at the top at
0 degrees), KiCad's rotation was already right and the rule turned the
connector upside down. The #518 preview shows the pads at the bottom for the
same name and rotation, so the two boards did not carry the same geometry.
Pin it per project with a designator override.
