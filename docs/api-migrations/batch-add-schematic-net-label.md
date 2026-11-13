### `batch_add_schematic_net_label` adds several labels in one write (minor release)

The new `sch_batch` tool takes `labels`, each entry with the arguments
`add_schematic_net_label` takes: `net`, `x`, `y`, `rotation`, `label_type`
and `shape`. A sheet's interface of hierarchical labels is one call instead of
one per signal:

```json
{
  "schematic": "interface.kicad_sch",
  "labels": [
    {"net": "SDA", "x": 96.52, "y": 99.06, "rotation": 180,
     "label_type": "hierarchical_label", "shape": "bidirectional"},
    {"net": "SCL", "x": 96.52, "y": 101.6, "rotation": 180,
     "label_type": "hierarchical_label", "shape": "input"}
  ]
}
```

`shape` is one of `input`, `output`, `bidirectional`, `tri_state` and
`passive`, the shapes `add_sheet_pin` takes, and defaults to `input` on a
global or hierarchical label; a `net_label` given one is refused. Every entry
is checked before the file is read, so a malformed one refuses the whole call
with `invalid_argument` naming it (`labels[1].x`) and nothing is written.

The labels are written in one revision-checked write. A save made after the
read, or KiCad's lock on the sheet or on its project's root, refuses the call as
`conflict` with nothing written. `added` is read back from the written file: per
label its `net`, `label_type`, `shape` (`null` on a `net_label`), `x`, `y`,
`rotation` and `uuid`, in request order, with `added_count`. A write that fails
after it may have replaced the file, a file that cannot be read back, or one that
does not hold exactly the requested labels beside the ones it had returns
`mutation_outcome_uncertain`; reload and inspect it before retrying.
