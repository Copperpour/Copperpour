### `modify_trace` changes the track in place (patch release)

`modify_trace` now updates the track it names, so the track keeps its UUID and
`modified_uuid` names a track that is still on the board, and the change is one
KiCad undo step ([#366](https://github.com/Copperpour/Copperpour/issues/366)).
It used to delete the track and create a new one with a new UUID; when KiCad
refused the new one, the old track was already gone.

It now refuses, with the board unchanged, when the UUID is not a trace segment
on the board (`stale_target`; it used to delete whatever item had that UUID),
when the board has no net named `net_name` (`invalid_argument` on `net_name`),
when the board has no such copper layer (`invalid_argument` on `layer`), and
when `width` is not greater than zero (`invalid_argument` on `width`; it used to
be sent to KiCad). If KiCad accepts the update but the track does not read back
as sent, the answer is `ipc_outcome_unknown`: inspect the live board before
retrying.
