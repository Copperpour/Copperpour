# ERC severity fixture

`erc_severity_kicad10.kicad_sch` is one sheet with three global labels and
nothing else: `LONE` once, at (101.6, 50.8), and `PAIR` twice, at
(101.6, 63.5) and (101.6, 76.2). KiCad counts global labels by name over the
whole schematic and reports a name it finds once as `single_global_label`
(`CONNECTION_GRAPH::ercCheckSingleGlobalLabel`,
`eeschema/connection_graph.cpp`), a check it leaves at `ignore` by default
(`eeschema/erc/erc_settings.cpp:104`). So the sheet raises it on `LONE` alone,
and only when the project turns it on.

## Provenance

The three labels were typed in a text editor into a sheet holding nothing
else, and the sheet force-resaved by KiCad 10.0.6:

```text
kicad-cli sch upgrade --force erc_severity_kicad10.kicad_sch
```

What is committed is KiCad's serialization: `(generator "eeschema")`,
`(version 20260306)`, the labels in KiCad's order, each with the
`Intersheetrefs` field KiCad adds to a global label.

The tests put it beside a project file of one of two kinds:
`annotate_nested_hierarchy/nested_pages.kicad_pro`, Eeschema 10.0.6's own save,
whose `erc.rule_severities` holds every check, `single_global_label` at
`ignore`; or the project file `create_project` writes, which has no `erc`
section. The first is borrowed because, without Eeschema's window, KiCad 10.0.6
writes no `erc` section: kicad-cli's schematic commands load the project and do
not save it, and Eeschema's API has no save. KiCad sets `meta.filename` when it
saves a project (`common/project/project_file.cpp:759`) and does not read it on
load, so the file serves under another name as it is.

## KiCad's own answer

Beside the Eeschema project file, unchanged:

```text
kicad-cli sch erc --format report --severity-all erc_severity_kicad10.kicad_sch
```

```text
[label_dangling]: Label not connected
    ; error
    @(101.60 mm, 50.80 mm): Global Label 'LONE'
[label_dangling]: Label not connected
    ; error
    @(101.60 mm, 76.20 mm): Global Label 'PAIR'

 ** ERC messages: 2  Errors 2  Warnings 0

 ** Ignored checks:
    - Global label only appears once in the schematic
    - Four connection points are joined together
    - SPICE model issue
    - Assigned footprint doesn't match footprint filters
```

With `single_global_label` set to `error` in that file, the report adds one
message and no longer lists the check as ignored:

```text
[single_global_label]: Global label only appears once in the schematic
    ; error
    @(101.60 mm, 50.80 mm): Global Label 'LONE'
```
