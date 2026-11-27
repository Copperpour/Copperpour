# `NET_NAME` directive move

The seed comes from the `net-name` probe at #924's earlier `dd00c471`
head, based on the Eeschema 10.0.8 [`base` fixture](../base/sparse.kicad_sch).
It has R2's hidden `BOM Comments` field `${NET_NAME(1)}` and a Netclass
directive. After removing the six power placements through Copperpour,
Eeschema 10.0.8's **Save Selection as Design Block** produced a new
native sheet with only the selected circuit and its used library symbol.
`Device:R` is the only embedded definition. Its two placements give
the `SIG_C` net two pins.

Copperpour's `batch_delete` removed the unused `Other` label and stub.
The `after` directive is detached at `(45.72, 40.64)`; `move_region`
moved it onto the `SIG_C` stub at `(45.72, 43.18)` in the `before` copy.
Only that directive and its field position differ. `kicad-cli` 10.0.8
saved both committed inputs with `sch upgrade --force`.

The files have no project or child sheet. On both versions, export each
with `sch export netlist --format kicadxml` and `sch export bom`, with
and without `--exclude-dnp`. R2's `BOM Comments` field is `/SIG_C` in
XML and both CSVs before and after. The XML net `/SIG_C` changes from
`class="AssemblyA,Default"` to `class="Default"`. Both BOMs are byte
identical within each version. XML matches after removing the document
source and date and the net class attributes, then canonicalizing.

Use `--group-by ""` and pass this list to both `--fields` and `--labels`:

```text
Reference,Value,Footprint,Datasheet,Description,BOM Comments,DNP,EXCLUDE_FROM_BOM,EXCLUDE_FROM_BOARD,EXCLUDE_FROM_SIM
```

| KiCad | R2 field, before → after (XML and both BOMs) | Matched XML SHA-256 | Matched BOM SHA-256 (both modes) |
|---|---|---|---|
| 10.0.6 | `/SIG_C` → `/SIG_C` | `57ba65856481e7451a23cb56d6ef4737b6573e3f7f3490b10711d0b9fabbb103` | `98528ada63b411ed1ce27034956017a6a77512a6e018c54e0bd55a73d5fa23de` |
| 10.0.8 | `/SIG_C` → `/SIG_C` | `73f276d9891830ffe2a7e2a6ba41bc50c827599064858cad3872a9e64110b79e` | `98528ada63b411ed1ce27034956017a6a77512a6e018c54e0bd55a73d5fa23de` |

The byte hashes of the inputs are recorded in [SHA256SUMS](../SHA256SUMS).
