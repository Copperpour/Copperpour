### `get_teardrop_settings` and `set_teardrop_settings` read and set native pad and via teardrops (minor release)

Two new tools in the `pcb_copper` toolset work on KiCad's own pad and via teardrop settings on the board open in KiCad ([#555](https://github.com/Copperpour/Copperpour/issues/555)). Both need KiCad 10.0.7 or newer, read from the bound editor; older or unknown versions get `unsupported_capability` before anything is read or written.

- `get_teardrop_settings(board, references?, nets?, kinds?, limit?)` lists each pad and via with its UUID, reference, pad number, net, position and settings, plus the board's teardrop zones by net and `legacy_teardrops`.
- `set_teardrop_settings(board, targets, settings, refill_zones?, dry_run?)` changes the settings of the selected pads and vias in one KiCad undo step, without saving. `targets` selects by `pads` (`{reference, pad_number}` or `{uuid}`), `vias`, `references`, `nets`, `all_pads` and `all_vias`. `settings` takes `enabled`, `curved_edges`, `allow_multiple_track_segments`, `prefer_zone_connection`, `max_length_mm` and `max_width_mm` (`null` for no limit), `best_length_ratio` (0.2 to 2), `best_width_ratio` (0.6 to 1) and `max_track_width_ratio` (0 to 1); omitted fields keep their value.

KiCad rebuilds the edited items' teardrops in the same undo step. `refill_zones` defaults to `false`; set it to refill every zone on the board and rebuild every teardrop afterwards. The response reports `targets` (settings before and after, read back from KiCad), `copper.commit` and `copper.refill` separately.

Boards in legacy teardrop mode, and pads of a footprint that owns groups, are refused with `plan_blocked` and nothing written: pads are written inside their whole footprint, which would drop such a group. No existing call changes.
