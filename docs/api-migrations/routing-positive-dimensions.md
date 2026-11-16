### Routing widths and differential-pair gaps must be positive (patch release)

`route_trace`, `route_pad_to_pad`, `modify_trace`, and
`route_differential_pair` refuse non-positive widths as `invalid_argument` before
IPC. `route_differential_pair` applies the same rule to `gap`. Values that round
to zero nanometres and malformed supplied values are also refused. The schemas
advertise `exclusiveMinimum: 0`; handlers additionally check nanometre precision.

Supply positive numeric dimensions that round to at least one nanometre, or omit
them to retain the existing defaults (0.25 mm width, or 0.1 mm width and gap for
pairs). Existing valid calls and success responses are unchanged.
