### `connect_passthrough` dots its stub (patch release)

A stub drawn from a point where wires already met, or from mid-span on a wire,
used to get no junction dot
([#137](https://github.com/Copperpour/Copperpour/issues/137)). It now gets the
dots a new wire calls for, as a wire drawn with `add_wire` does. The response
is unchanged, and a stub that meets no other wire, and no pin past its root, is
written as before.
