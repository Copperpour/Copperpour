### `route_differential_pair` creates the pair in one commit (patch release)

`route_differential_pair` now sends both traces in one `CreateItems` inside one
KiCad commit, so a routed pair is one undo step, and when KiCad refuses either
trace the commit is dropped and neither is left on the board
([#367](https://github.com/Copperpour/Copperpour/issues/367)). It used to create
them one after the other, each its own undo step, and a failure creating the
negative trace left the positive one behind.

`net_neg` equal to `net_pos` is now refused with `invalid_argument` on
`net_neg` before KiCad is asked anything; it used to route two traces of one
net. A failure is now reported in its own words; only an unreachable KiCad, or
one holding no board, still reads "KiCAD must be running". KiCad refusing to
open the commit, while busy or with one of this client's already open, fails
the call before anything is created. A create, commit or drop KiCad does not
confirm is `ipc_outcome_unknown`: inspect the live board before retrying. The
arguments and the successful response are unchanged.
