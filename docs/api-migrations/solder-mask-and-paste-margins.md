### `edit_footprint_pad` and `set_footprint_metadata` set solder mask and paste margins (minor release)

Both tools accept `solder_mask_margin` and `solder_paste_margin` (mm) and
`solder_paste_margin_ratio` (a fraction, as KiCad stores it: `-0.1` for
-10 %), as agreed on
[#637](https://github.com/Copperpour/Copperpour/issues/637).
`edit_footprint_pad` sets a pad's own margins, on every matching pad with
`match_all`; `set_footprint_metadata` sets the footprint's own.

- **Values:** a number writes the override where KiCad writes it. `0` is an
  override like any other value, since KiCad's format 20240202.
  `"inherited"` removes the override, so the footprint's or the board's
  value applies. Omitting an argument leaves the margin as it is.
- **Refused** as `invalid_argument` on the argument, with nothing written:
  anything else, such as `null` or a numeric string; a length beyond the
  largest KiCad keeps; and `0` in a file of format 20240201 or older, where
  KiCad reads a margin of 0 as inherited.
- **Answer:** `edit_footprint_pad` reports each margin set as
  `{"previous": …, "current": …}`, read as KiCad reads the pad, with
  `"inherited"` for no override; pads that disagree give a list of the
  distinct pairs. `set_footprint_metadata` reports a changed margin in
  `changed_fields` and `changes`, as it does its other fields.
- `set_footprint_metadata` now also accepts a call with only margins.
