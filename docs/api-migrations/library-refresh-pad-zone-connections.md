### `update_footprints_from_library` refreshes footprints with pad zone connections or thermal relief settings (minor release)

Typed library refresh refused a library footprint whose pads set a zone
connection, `(zone_connect N)`, or thermal relief settings,
`(thermal_bridge_angle …)`, `(thermal_bridge_width …)` or `(thermal_gap …)`,
as `unsupported_library_footprint`
([#300](https://github.com/Copperpour/Copperpour/issues/300)). 1,141 of KiCad
10.0.8's stock footprints set a pad zone connection and 137 a pad spoke
angle, among them the exposed-pad packages and the plated mounting holes.

Refresh now reads them on every KiCad 10 version:

- Each pad is sent with its zone connection and thermal settings, as KiCad
  loads them from the file. A pad without a connection inherits one, and a
  pad without a spoke angle gets KiCad's default for its shape: 45° for a
  round pad, 90° for the others.
- A pad whose connection or thermal settings differ from its library pad
  plans `pads`. That includes a placed pad that inherits its connection but
  whose spoke angle, spoke width or gap was changed on the board. Refresh
  used to leave such a pad's settings out of the comparison; it now restores
  the library's.

A footprint that also has a pad fabrication property or a reference point
still needs KiCad 10.0.7 for those, as before.

A footprint-level `(zone_connect …)` is still refused, and so is a footprint
with any other clause refresh doesn't read, with the clause named.
