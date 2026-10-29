# A board whose outline, footprint and texts KiCad made (issue #47)

Every byte of `board_outline_kicad10.kicad_pcb` is **KiCad's own save**. KiCad
made the outline and the texts on requests over its IPC API, and placed the
footprint from a library file made for it; no item was written into the board
by hand or by a file writer.

## Provenance

1. Konnect 0.14.0's `create_project` laid out the project, and its schematic
   was deleted. pcbnew **10.0.6**
   opened the board with the IPC API enabled, and Konnect's `save_project` saved
   it through the API's `SaveDocument`.
2. With pcbnew holding the board, Konnect 0.14.0 made these calls; each was
   answered from KiCad (`"source": "ipc"`):

   ```text
   add_board_outline  x1=0 y1=0 x2=100 y2=50 corner_radius=3
   place_component    footprint=Test:Slot reference=U1 x=50 y=20 layer=F.Cu
   add_board_text     text="REV A" x=50 y=45 layer=F.SilkS
   add_board_text     text="EDGE" x=10 y=45 layer=Edge.Cuts
   ```

   `Test:Slot` is a footprint made for this fixture in a project library, with
   `create_footprint` (one 0.5 mm pad, the least that writes its reference and
   value) and `set_footprint_graphics` (a 10 mm × 2 mm rectangle on Edge.Cuts,
   centred on its origin). KiCad loaded it from that library and wrote it into
   the board. Nothing from KiCad's own libraries is in the board.
3. `save_project` saved the board through `SaveDocument` again, and pcbnew was
   closed. A second open and save left the file byte-identical.

Measured against **KiCad 10.0.6** on Ubuntu 24.04 (Linux).

## What the tests rely on

| Item | Layer | UUID |
|---|---|---|
| 4 `gr_line`, 4 `gr_arc`: a 100 mm × 50 mm outline with 3 mm corners | Edge.Cuts | `80edbc7e-…`, `d65cb623-…`, `5c5a0563-…`, `cb0492d0-…`, `9381393c-…`, `fcda717d-…`, `844e3a41-…`, `20fade57-…` |
| `fp_rect` inside footprint `U1` | Edge.Cuts | `43d37a15-8750-41b9-9cc8-721c18b000e4` |
| `gr_text "REV A"` | F.SilkS | `323f9e8d-3648-434d-8fec-16fa74d87abb` |
| `gr_text "EDGE"` | Edge.Cuts | `a29de0ab-5535-43c9-a5ee-8839b9ac95ff` |

The slot spans x 45 to 55 mm and y 19 to 21 mm, inside every outline the tests
write over it.

## sha256

`78a6f7c0b9eb46a52ae4ad67fc6f8ae2b14247c55c89f6ad1d06181374e30a6b`

## Regenerating

1. Create the project with `create_project` (name `board_outline_kicad10`), and
   delete the schematic it lays out.
2. Open the `.kicad_pcb` in pcbnew 10.0.6 with the API server enabled, and save
   it through `SaveDocument`.
3. Make `Test:Slot` as above, register `Test.pretty` for the project, and make
   the four calls above with pcbnew holding the board.
4. Save through `SaveDocument`, close pcbnew, and remove any `~*.lck` it leaves.

New UUIDs come out of each run, so the tests' UUIDs change with the file.

## Tests using this fixture

`crates/copperpour-core/src/tools/pcb_board_outline.rs`: the saved-board tests,
the board KiCad holds in the live tests, and the `kicad-cli` test.
