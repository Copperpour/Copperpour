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
| Placed Reference change with and without a saved instance | [Paulius's second review] verified that KiCad 10.0.8 exports the saved instance reference and restores it on resave. The no-instance and repeated-reference refusals are separate served controls. |
| `changing_a_variant_with_repeated_dnp_is_not_layout` | Served-only ambiguity control. No equality claim for competing flags. |
| `formatting_and_reordering_variant_records_is_allowed` | Served-only normalization control with the same literal key and value. |
| `parent_variant_dnp_affects_its_children` | Prior KiCad 10.0.8 [review evidence] verified the fitted `Lite` BOM drops D1 for this saved placement override. |
| Placement and Component Class comparisons | See the rule-area cases below for the current guard. |
| `changed_saved_input_to_exported_expression_blocks` | Served-only refusal naming the field that reads the changed page. |
| `field_move_with_a_substituted_variant_override_is_layout_only` | Served-only acceptance when the move leaves the saved `Lite` page dependency unchanged. |
| `competing_instance_records_keep_their_order_and_multiplicity` and `repeated_child_instances_do_not_hide_an_order_change` | Served-only instance ambiguity cases from the split review. The guard does not guess which record KiCad uses. |
| `legacy_and_default_instance_export_inputs_are_guarded` and `duplicate_field_suffixes_cannot_hide_reordered_exports` | Served-only cases from the split review. No native result is claimed for these exact edits. |
| Root UUID, legacy simulation order, legacy defaults with variant overrides, and repeated instance text | Served-only parser-compatibility regressions. They exercise saved inputs that the normalized view does not resolve. |

XML comparison ignores the source path and export timestamp. The standard and
fitted BOMs match byte-for-byte. KiCad wrote a project-local settings sidecar in
each disposable directory but did not change the saved inputs. The exact
literal-variant pair uses these SHA-256 hashes:

```text
5787cf396a4ae81d8942c07ad0b65e7a014ce09394065de9a2c692ba0954249c  before/sparse.kicad_sch
3ba595b421f0f29e5c33f7a892ca56f968b575d1ddcb99cf836063219f6a8da6  after/sparse.kicad_sch
46823f1c871cce5ce82611a4397bb29d9f3e4da22d4e804934aaa600c15504a6  both/sparse.kicad_pro
```

## Rule-area and Component Class cases

The served tests can copy their exact before and after schematics to an empty
audit directory for KiCad XML and BOM exports. The comparator reads only the
test's two input files, not the audit copies or project used for native exports.
The copies below start from the KiCad-saved inputs listed in `SHA256SUMS`.
Deliberately ambiguous records are parser controls,
not examples of normal KiCad output.

| String-built case | Native evidence |
|---|---|
| R1 Reference moves into the DNP area | [Paulius's review] confirmed that the native export can gain DNP when the saved area reaches R1. The served edit changes the placement rather than the area outline. |
| R1 Reference moves within the DNP area | `inside-dnp-area`: compare the exact served pair's XML, standard BOM and fitted BOM. Verify R1 stays DNP. |
| R1 Reference moves with the same literal Component Class | `literal-component-class`: compare the exact served pair's XML and both BOM modes. Verify `AssemblyA` stays assigned to R1. |
| The placed spelling grows toward a DNP area, but saved R1 is unchanged | `saved-reference-near-dnp-area`: compare XML and both BOM modes and check that R1 is still fitted. |
| An active area has a second point list | KiCad 10.0.8 [appends the points]. The served test checks that the first list alone cannot prove separation. |
| An attached class directive gains a second position | KiCad 10.0.8 [applies each directive position]. The served test checks the conservative refusal. |
| Attached directive class `~` crosses the legacy empty-field cutoff | KiCad 10.0.8 [loads legacy `~` as empty]. The served test compares the empty and literal class. |
| A long default Value enters a DNP area while the short `Lite` Value stays clear | `load-time-geometry-with-lite-value`: base XML stays DNP, the `Lite` DNP override disappears, and R1 drops from the fitted `Lite` BOM. |
| Unchanged repeated default or `Lite` DNP records and an area entry | `repeated-default-dnp-area-entry` and `repeated-lite-dnp-area-entry`: R1 drops from the fitted `Lite` BOM. The XML shows the base flag and variant delta separately. |
| R1's second Reference position or second library definition reaches a DNP area | `repeated-reference-position-area-entry` and `duplicate-library-geometry-area-entry`: base XML gains DNP and R1 drops from the fitted `Lite` BOM. |
| R1's active Reference field enters the DNP area while an earlier copy is already inside | `superseded-reference-area-entry` checks the effects of the last field record. |
| An extended `Device:R` definition appears in a later library block | `second-library-block-area-entry` checks the XML and fitted BOM with that definition in a separate block. |
| A later visibility setter shows R1's Reference, or a later rectangle coordinate extends its body | `repeated-visibility-area-entry` and `repeated-rectangle-coordinate-area-entry` check the actual native setters and DNP output. |
| Legacy `Sim.Enable` can override the local exclusion while an area still applies | `legacy-sim-enable-area-entry` records XML variant and BOM effects separately. |
| An earlier stroke width or font size understates the actual bounds | `repeated-shape-stroke-width` and `repeated-field-font-size` record the resulting native DNP effects. |
| A visible legacy simulation field is discarded during load | `discarded-sim-field` records the native simulation effect when the actual Reference enters the area. |
| A visible lowercase `sim.enable` field enters a DNP area | `case-distinct-sim-field` checks the area effect of a custom field whose spelling KiCad preserves. |
| A later pin name or number record changes decoration size | `repeated-pin-name-style` and `repeated-pin-number-style` check whether the fitted BOM changes without changing the pin name or number. |
| `body_style` and its legacy spelling `convert` select different shapes | `competing-body-style-spellings` checks a style change with the same pins. |
| A later `(hide no)` reveals a decorated pin after bare `hide` | `mixed-pin-visibility` checks the affected DNP flag without moving the pin. |
| A later `lib_name` selects a different embedded body | `repeated-library-selection` checks the body change with unchanged pin positions and instance records. |
| Repeated library selectors hide a changed body or select a power alias first | `repeated-library-ids` and `ambiguous-power-exemption` check the area lookup and power exemption separately. |
| Two embedded keys differ only by KiCad's `{slash}` escape | `escaped-library-alias` checks the later body selected by KiCad. |
| A variant record sets two names and conflicting DNP values | `competing-variant-names` checks the fitted `Lite` BOM across an area entry. |
| A style declaration disappears while its drawing remains saved | `pruned-body-style` checks the DNP result of the body KiCad actually loads. |
| An alternate drawing is moved directly under the embedded library symbol | `root-library-drawing` checks its default unit and style in the native DNP output. |
| The final mirror follows a field or is reset by the position record | `field-before-mirror` and `mirror-before-position` check which saved geometry reaches the DNP area. |
| The attached Netclass directive assigns a different literal to `SIG_C` | `literal-rule-area-netclass-change`: `/SIG_C`'s XML class changes from `ClassA` to `ClassB` with the same project class definitions. Both sets of BOM outputs are unchanged. |
| A global power Value's position changes, then its text changes | Served-only check against the existing net-name gate. [#937] documents using the placed Value for a custom global rail. |
| Uncertain directive anchor, encoded class substitutions, competing private classes, and legacy or named units | Served-only checks of cases where the comparator cannot prove unchanged effects. No native export value is inferred. |

[#822]: https://github.com/Copperpour/Copperpour/issues/822
[Paulius's review]: https://github.com/Copperpour/Copperpour/pull/850#pullrequestreview-6078422391
[Paulius's second review]: https://github.com/Copperpour/Copperpour/pull/850#pullrequestreview-6080100865
[review evidence]: https://github.com/Copperpour/Copperpour/pull/850#pullrequestreview-6078422391
[#937]: https://github.com/Copperpour/Copperpour/pull/937
[appends the points]: https://github.com/KiCad/kicad-source-mirror/blob/10.0.8/eeschema/sch_io/kicad_sexpr/sch_io_kicad_sexpr_parser.cpp#L4190-L4207
[applies each directive position]: https://github.com/KiCad/kicad-source-mirror/blob/10.0.8/eeschema/sch_io/kicad_sexpr/sch_io_kicad_sexpr_parser.cpp#L4672-L4696
[loads legacy `~` as empty]: https://github.com/KiCad/kicad-source-mirror/blob/10.0.8/eeschema/sch_io/kicad_sexpr/sch_io_kicad_sexpr_parser.cpp#L2327-L2333

## Saved text expressions

[`net-name`](net-name/sparse.kicad_sch.README.md) contains the KiCad-saved
directive move pair. Both tested KiCad versions export R2's `${NET_NAME(1)}`
as `/SIG_C` on both sides. The remaining expression probes check the served
comparator only; they make no claims about exact native output.
