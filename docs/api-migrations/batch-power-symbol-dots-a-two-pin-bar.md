### `batch_add_power_symbol` dots the corner a two-pin group's symbol sits on (patch release)

A group whose pins end on two points puts its power symbol on the first stub's
end, a corner of the bar, where the stub, the bar and the power pin meet. That
corner now gets a junction dot, as eeschema draws one at three or more exits,
and it is listed in `junctions_added`. KiCad joined the pins there without the
dot, so no net changes. Groups of one point, or of three or more, are
unchanged. Issue: [#376](https://github.com/Copperpour/Copperpour/issues/376).
