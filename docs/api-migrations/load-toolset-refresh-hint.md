### `load_toolset` returns a `hint` (patch release)

A `load_toolset` reply that adds tools now carries a `hint` string: if the client
still rejects the new tools as unknown, it caches its first `tools/list`, and
`"eager_toolsets": true` plus a client restart makes them callable. A reply that
loads nothing new has no `hint`. Callers that ignore unknown keys need no change.
