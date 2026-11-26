# Layout comparison property fixtures

The single-sheet fixtures were saved in Eeschema 10.0.8 from
[`region_cleanup_kicad10/sparse`](../region_cleanup_kicad10/sparse). The
project basename remains `sparse`, and the project defines the `Lite` variant
without replacing the saved instance data. The hierarchy fixtures came from
[`annotate_nested_hierarchy`](../annotate_nested_hierarchy). They keep the project
basename `nested_pages`. The comparator tests leave `bank` and `cell` out of
the input directories; the native checks include them.

[SHA256SUMS](SHA256SUMS) records the committed sheet and project bytes for
each case. [Paulius's review] independently checked the fixture resaves,
ERC, and the XML and BOM comparisons below with KiCad 10.0.8.

| Saved pair | Native output delta | Served verdict or reason |
|---|---|---|
| `base` → `pos` | XML gains `exclude_from_pos_files`; BOM unchanged. | `attribute in_pos_files` |
| `base` → `variant-dnp` | Fitted `Lite` BOM loses R1; default BOM unchanged. | `variant "Lite" changed attribute dnp` |
| `base` → `variant-value` | `Lite` XML sets R1 Value to `1`; default BOM unchanged. | `variant "Lite" changed field "Value"` |
| `before-sim` → `base-sim` | BOM `EXCLUDE_FROM_SIM` column gains `Excluded from simulation`. | `attribute exclude_from_sim` |
| `area-flagless` → `area-dnp` with R1 outside | XML and BOM unchanged. | Area inputs changed; current guard cannot prove separation. |
| `nested_pages` → `parent-dnp` | XML gains `dnp` on 16 children; fitted BOM drops those rows. | `south ... changed attribute dnp` |
| `nested_pages` → `parent-field` | XML gains `Field2` and `AssemblyNote` on six children. | Names both changed fields on `south`. |

## Runtime edits and native evidence

The tests repeat the comparison and check the file bytes and directory
entries in both input directories. A served-only case does not claim an
independent KiCad export result.

| Test or input edit | Native evidence |
|---|---|
| Served edits to Footprint, Value, DNP, BOM inclusion, Datasheet, Description, MPN, Manufacturer, BOM Comments, and hiding or moving ordinary field text | [#822] reports the original KiCad 10.0.8 reproductions and layout control. Test values differ in some cases. |
| Custom field update or removal; a custom `EXCLUDE_FROM_SIM` field; cleanup isolation | Served checks; [#822] documents custom field forwarding and the distinction from the native flag. |
| `field_move_on_a_literal_variant_override_is_not_blocked` | Exact `variant-value` pair on KiCad 10.0.8. The edit changes only R1's saved Reference x coordinate from `40.132` to `40.64`. XML and both BOM modes were unchanged for default and `Lite`. |
| `changing_only_the_saved_instance_reference_keeps_identity_refusal` | Served-only edit to the instance record; [#822] supplies a separate native Reference rename control. |
| `changing_a_variant_with_repeated_dnp_is_not_layout` | Served-only ambiguity control. No equality claim for competing flags. |
| `formatting_and_reordering_variant_records_is_allowed` | Served-only normalization control with the same literal key and value. |
| `parent_variant_dnp_affects_its_children` | Prior KiCad 10.0.8 [review evidence] verified the fitted `Lite` BOM drops D1 for this saved placement override. |
| `placement_move_with_possible_rule_area_effects_blocks` | Prior KiCad 10.0.8 exports show that moving R1 text to the active area adds DNP. The current test checks the coarse refusal, not the exact intersection. |
| `edit_with_a_possible_component_class_dependency_blocks` | Served-only coarse refusal. No claim that moving Reference text changes a literal class. |
| `changed_saved_input_to_exported_expression_blocks` | Served-only coarse refusal for changed saved page input. |
| `field_move_with_a_substituted_variant_override_is_not_proven_layout` | Served-only coarse refusal. No claim that the move changes the expansion. |

XML comparison ignores the source path and export timestamp. The standard and
fitted BOMs match byte-for-byte. KiCad wrote a project-local settings sidecar in
each disposable directory but did not change the saved inputs. The exact
literal-variant pair uses these SHA-256 hashes:

```text
5787cf396a4ae81d8942c07ad0b65e7a014ce09394065de9a2c692ba0954249c  before/sparse.kicad_sch
3ba595b421f0f29e5c33f7a892ca56f968b575d1ddcb99cf836063219f6a8da6  after/sparse.kicad_sch
46823f1c871cce5ce82611a4397bb29d9f3e4da22d4e804934aaa600c15504a6  both/sparse.kicad_pro
```

[#822]: https://github.com/Copperpour/Copperpour/issues/822
[Paulius's review]: https://github.com/Copperpour/Copperpour/pull/850#pullrequestreview-6078422391
[review evidence]: https://github.com/Copperpour/Copperpour/pull/850#pullrequestreview-6078422391
