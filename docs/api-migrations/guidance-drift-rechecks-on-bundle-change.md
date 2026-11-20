### Installed-guidance drift probe re-checks once when the embedded bundle changes (patch release)

`~/.copperpour/.guidance-checked-<client>` now keys its cached drift state on
both the install marker and a SHA-256 digest of that client's embedded guidance
bundle (skills, references, and for Claude, agents and hook matchers).

Previously, the cache key included only the install marker, so upgrading the
`copperpour` binary (either to a newer release or a newer dev build) never
re-scanned installed guidance or emitted the MCP `initialize` drift notice.

Callers of `initialize` and `get_installation_info` see no schema changes. On
the first start of an upgraded binary whose embedded guidance changed (or when
upgrading over a pre-change cache file without `bundle_digest`), `checked`
reports `"now"` with per-file detail and `initialize` includes the one-line
drift notice if `out_of_sync`. Subsequent starts with the same marker and
embedded bundle stay quiet (`checked: "earlier"`).
