### Browser viewer startup and listener recovery (minor release)

The browser viewer retries transient listener accept errors instead of terminating
its HTTP service. Connection errors retry immediately; other errors are logged
and retry after one second. Shutdown interrupts the backoff. Authentication,
loopback binding and bounded ownership of accepted connections are unchanged.

An unchanged hierarchy renders once at startup. Source timestamps and lengths
are observed before discovery reads and compared after watcher installation;
changed sources receive a catch-up refresh. Changed project settings invalidate
all sheets, and unavailable revision metadata conservatively triggers catch-up.
Treat `revision` as an opaque change marker: it remains 1 at unchanged startup
until an actual update or explicit refresh, rather than advancing to 2 for a
redundant second full render. SSE still sends the current snapshot on connection.

Existing refusal of child paths containing `..` remains, including paths such as
`sub/../child.kicad_sch` that resolve inside the root. Tests now pin that refusal
and its diagnostic independently of the later outside-root check.
