### Inline renders arrive as image content, not a base64 string (minor release)

`render_schematic_png` with `inline: true` no longer adds `png_base64` to its
JSON report, and `compare_visual_baseline` with `inline_diff: true` no longer
adds `current_png_base64`. The report stays the first content block, and the
PNG follows as a second block of type `image` (`mimeType` `image/png`), the
form `get_board_2d_view` already used. Read the image from that block; the PNG
file named in `rendered` is unchanged.
