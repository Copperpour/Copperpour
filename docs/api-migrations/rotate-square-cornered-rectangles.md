### Footprints with square-cornered rectangles rotate off-axis over IPC (patch release)

`rotate_component` and `set_component_placements` refused any non-90° rotation
of a footprint holding a rectangle graphic, because KiCad sends every
rectangle with a `corner_radius`, 0 for square corners
([#224](https://github.com/Copperpour/Copperpour/issues/224)). Such a
rectangle now rotates into a polygon, as KiCad's own rotation does, so stock
footprints such as `C_0805_2012Metric` rotate to 45°. Only a rectangle with a
positive corner radius is still refused, and the refusal now names the
footprint and the graphic's layer, for example
`cannot rotate footprint 'Y1' by a non-90° angle: its rounded-rectangle graphic
on F.Fab has no off-axis form`. The textbox and unsupported-item refusals of
the same tools, and of `move_component` and `align_components`, name the
footprint too. A caller that matched the old message text should match the
new one, or rotate that footprint by a multiple of 90°.
