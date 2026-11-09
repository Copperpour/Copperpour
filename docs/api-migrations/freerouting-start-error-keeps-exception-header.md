### `check_freerouting` and `route_specctra_dsn` start errors keep the exception header (patch release)

When Freerouting's MCP server fails to start, no `.freerouting.log` keeps its
stderr, and the error ended with the last 12 lines of it. A Java stack trace
names the failure in its first lines, so a long trace lost them. The error now
puts the header of the last stack trace in front of those 12 lines when they
leave it out: the last line that names an error (`ERROR`, `Exception` or
`Error`), with the line just before it when that one names an error too, each
cut to 500 characters, then a `...` line. Errors of a run that wrote a
`.freerouting.log` are unchanged.
