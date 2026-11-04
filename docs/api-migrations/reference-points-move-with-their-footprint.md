### Footprints with a reference point can be moved and rotated on KiCad 10.0.7+ (patch release)

KiCad 10.0.7 started sending a footprint's reference point, its `(point …)`,
as a child of the footprint (`kiapi.board.types.ReferencePoint`). 617 of
KiCad 10.0.8's stock footprints have one, among them the DIP packages. On
KiCad 10.0.7 and later, `move_component`, `rotate_component`,
`set_component_placements` and `align_components` refused every move of such
a footprint, as an item Copperpour could not carry along
(`unsupported_capability`, [#270](https://github.com/Copperpour/Copperpour/issues/270)).

They now move and turn the point with the footprint, about its anchor, as
KiCad does. Nothing else changes. Footprints holding other items Copperpour
cannot carry, such as a barcode or a table, are still refused the same way.
