### Live net readers require a `GetNets` response payload (patch release)

`get_nets_list` now returns a structured `editor_unavailable` error naming the
requested board when KiCad supplies no net response payload, instead of reporting
`count: 0` and `nets: []`. Malformed responses also return this query failure.
Retry the live query after checking KiCad; no saved board is substituted.
An explicitly present, empty `NetsResponse` still returns a successful empty list.
Successful response fields and tool arguments are unchanged.

The IPC `get_nets` and `get_nets_in` methods now require a response, just like
`get_nets_required_in` (retained as an alias). Their consumers propagate a missing
payload through their existing query/planning failure paths rather than deriving
empty net tables, unknown-net decisions, or empty routing rules. In particular,
`get_netclasses` refuses the failed live read without saved-file fallback.
`get_board_info` retains its existing fallback policy for failed live queries;
this change does not migrate that tool's source selection.
