### `route_specctra_dsn` survives unanswered job polls (minor release)

A JVM short of CPU, or in a long garbage collection, can stop answering for
more than 30 seconds in the middle of a long route. The first job poll left
unanswered used to fail the call with `Freerouting MCP request 'tools/call'
timed out`, and the routing done so far was lost. Up to 10 polls in a row may
now go unanswered: each one is reported as progress, `Freerouting not
answering, 3/10 polls missed in a row (12m05s)`, and the job is polled again.
The next answer resets the count; the 11th unanswered poll in a row fails the
call as before. The overall timeout still bounds the whole run.

The late answer to a poll that was given up is skipped instead of failing the
call as an unexpected response id. Callers need no change.
