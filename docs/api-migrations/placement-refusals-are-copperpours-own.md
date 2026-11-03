### Placement refusals Copperpour makes itself say so, with an error kind (patch release)

On a live board, `set_component_placements`, `move_component`,
`rotate_component` and `align_components` refuse some requests themselves,
on what they read from KiCad, before KiCad keeps any change
([#240](https://github.com/Copperpour/Copperpour/issues/240)):

- a rotation that is not a multiple of 90° of a footprint holding a
  rounded rectangle or a textbox;
- a footprint holding an item Copperpour cannot carry along, such as a
  KiCad 10 reference point (`kiapi.board.types.ReferencePoint`, in the
  stock DIP packages among others);
- a reference no footprint on the board carries, or, in a
  `set_component_placements` batch, one two footprints carry;
- a copper check that could not complete.

These refusals were plain text that blamed KiCad: "KiCAD rejected the …
over IPC: … KiCAD is reachable and may hold this board open …", and for
`align_components` "IPC align error: KiCad batch failed; changes dropped: …".
They now read `The {what} was not written: {reason}. The board file was not
modified.`, where `{what}` is `move`, `rotation`, `component placement
batch` or `alignment`, and carry an `error`:

| Refusal | `error` |
|---|---|
| A rotation that is not a multiple of 90° | `invalid_argument`, `field` `rotation` or `placements[i].rotation` |
| A reference missing, or held twice | `invalid_argument`, `field` `reference`, `placements[i].reference` or `references[i]` |
| An item Copperpour cannot carry | `unsupported_capability`, `capability` the tool's name, `kicad_version` null |
| A copper check that could not complete | `plan_blocked`, `operation` the tool's name, `reasons` holding the refusal |

An `invalid_argument`'s `reason` is the refusal's text. Nothing was changed
on the board: the refusal comes before any change is sent, or inside a
commit that is then dropped. A refusal KiCad itself returns keeps the
"KiCAD rejected …" wording and has no kind. A caller that matched the old
sentences should match `error.kind` instead.
