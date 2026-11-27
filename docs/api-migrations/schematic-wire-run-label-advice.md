### Opt-in labels-versus-wires advice and exchange (minor release)

`check_schematic_wire_runs` accepts an optional `label_advice` object. Existing
calls still return the same fields. Pass a positive `label_advice.long_run_mm`;
there is no default for advice. Supply either:

- `region: {"min": [x1, y1], "max": [x2, y2]}` to select runs whose drawing
  touches the rectangle, including runs that pass through it; or
- `wire_uuids: [...]` with the complete set of saved wire UUIDs for one run.
  UUIDs shared by more than one measured run do not select either one.

Coordinates are in millimetres. Optional `blocks` entries pair a `name` with
a `bounds` rectangle in the same format. The advisor doesn't infer blocks from
drawn outlines.

The opt-in `label_advice` response includes the selected runs' geometry, pin and
label endpoints, and crossings of foreign nets and caller-supplied blocks. The
label choice distinguishes reusing an ordinary local label from asking the
caller for a name. Choosing a name for an unnamed net changes its netlist name.
`consider_labels` requires a qualifying length and either a foreign crossing
or a passage through a supplied block. Detours at or above the check's
`detour_mm` threshold (10 mm by default) recommend rerouting first.
The advisor reports run endpoint distance separately. When it suggests labels,
it estimates the spacing using reusable anchors and default stubs without
checking placement clearance. The tool reads the saved schematic; it never
adds a label, renames a net, or writes a file.
Both tools report unreadable schematic paths as `file_not_found`, with the
path. Invalid schematic contents still report `malformed_file`.

Load `sch_readability` to use `exchange_schematic_wires_and_labels`. Omit
`revision` to preview one edit on a standalone sheet. For `direction: "wire_to_labels"`,
pass the exact `wire_uuids` of the run and use `local_name` only if the net is
unnamed. Optional `anchors` supplies the two final label positions in measured
start/end order. Explicit anchors must be exactly representable in KiCad's 0.0001 mm
schematic units. If a chosen stub cannot clear, the edit refuses rather than
moving that anchor. For `direction: "labels_to_wire"`, pass two `label_uuids`;
the first is kept if removing both would split the net or change its name.
To apply, send the same edit with the preview's `revision`. If the preview
reports a `rename`, pass its exact `from` and `to` as `acknowledge_rename`.
Symbolic links and other non-regular schematic targets report
`invalid_write_target` before the tool opens them. After `conflict`, preview again. After `mutation_outcome_uncertain`, re-read
the saved sheet before retrying: the edit may already be on disk.
Unknown selectors and invalid arguments report `invalid_argument`. If the
requested edit can be built but cannot be verified safe, the tool reports
`plan_blocked` with the reasons.
The exchange refuses when variable-expanded labels prevent it from proving
net names. A reverse bridge joins labels on wire ends or bare pin tips. It
cannot add a leg to a junction, a pin with a wire already attached, or an
existing through-run. New stubs and bridges stay outside the bounding
rectangles of buses and bus entries. The exchange refuses when KiCad
rounding leaves pin connectivity unproven.
New stubs and bridges use KiCad's default wire stroke. Retained wires are unchanged.
Electrical label taps use KiCad's saved-coordinate attachment rules; anchor reuse
still requires exact contact.
The forward edit refuses a cut that would strand a junction away from a pin,
or leave a label or wire end dangling. It removes a pin junction only when the
retained wiring no longer needs that dot.

The preview includes before and after measurements from
`compare_schematic_layout`. A better score does not authorize an edit.

The preview revision binds the saved schematic and its project-file input.
Apply rereads both, then replaces only the schematic with the same
revision-checked writer as `apply_schematic_cleanup`. The writer's final recheck
covers only the schematic. A concurrent project edit after the reread can
affect generated names, not the bytes written.
