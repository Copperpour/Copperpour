### Custom pad shapes and the corner ratio in the library footprint tools (minor release)

As agreed on [#675](https://github.com/Copperpour/Copperpour/issues/675):

- **`edit_footprint_pad`:**
  - `roundrect_rratio` (0..0.5) sets a roundrect pad's corner ratio and is reported as `roundrect_rratio` with `previous` and `current`. It is refused on a pad of another shape unless the same call sets `shape: "roundrect"`.
  - `shape` also takes `"custom"`, which needs `custom_shape`: `anchor` (`circle` or `rect`), `clearance` (`outline`, the default, or `convexhull`) and `primitives` in `set_footprint_graphics`' items, in the pad's own coordinates.
  - `custom_shape` alone redraws a pad that is already custom. It always replaces the whole primitive list, written as KiCad writes it, with coordinates, radii and widths rounded to KiCad's 1 nm grid. The answer reports `custom_shape` as read back from the pad as written.
  - `custom_shape` on a pad that is not custom, without `shape: "custom"`, is refused on `custom_shape`. So is `shape: "custom"` without it.
- **`create_footprint`:** a pad with `shape: "custom"` needs `custom_shape`, and `custom_shape` is refused on any other shape. Before, `shape: "custom"` wrote a custom pad without primitives.
- **`get_footprint_info` with `include_pads`:** a custom pad also has `custom_shape`, in the form the two tools take. A pad the model cannot read has `custom_shape_unreadable` with the reason instead, for example a polygon with arc segments in its outline.

KiCad's single-polygon rule for custom pads is left to its DRC (`DRCE_PADSTACK_INVALID`). Callers that never use the new arguments need not change their calls.
