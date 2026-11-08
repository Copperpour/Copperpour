# Instance repair fixture

`leaf.kicad_sch` is an eeschema 10.0.6 save (`version 20260306`), byte for byte
`crates/copperpour-core/tests/fixtures/instance_repair/shared/leaf.kicad_sch`;
that directory's README says how it was built. It backs
`SchematicCommand::repair_symbol_instances`.

Its two symbols, R1 and R2, each record three projects:

| Project | Path | Role |
|---|---|---|
| `owner` | `/0283ae14…/b0fe0e7a…` and `/0283ae14…/df588be5…` | the owning project, at both placements of the sheet |
| `""` | `/28a9c51f…`, the leaf's own uuid | what `kicad-cli sch upgrade` writes for a sheet resaved with no project file beside it |
| `other` | `/e7c1f0eb…/514ce7ab…` | a project in a sibling directory that places the sheet |

`renamed_leaf.kicad_sch` is, byte for byte,
`crates/copperpour-core/tests/fixtures/instance_repair/renamed/leaf.kicad_sch`: a leaf placed twice in a project `old`, annotated R1
and R2 at `/4e248827…/24f983cc…` and R3 and R4 at `/4e248827…/9b6ba800…`, all
under `old` after the project was renamed `new`.
