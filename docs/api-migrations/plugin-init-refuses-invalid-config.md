### `kicad_plugin_init` and `--config` refuse invalid startup configuration (patch release)

`kicad_plugin_init` in the embedded `copperpour_plugin` library previously
returned `1` and silently substituted built-in defaults when an explicit
configuration path was empty or invalid UTF-8, when an explicit configuration
file was missing, unreadable, or malformed, or when a selected search-path file
was malformed. It now loads configuration synchronously before creating the
Tokio runtime or starting any transport:

- On failure, `kicad_plugin_init` returns `-1`, writes a structured JSON
  refusal line (`kicad_plugin_init: {...}` with `outcome: "failed"`,
  `started: false`, `defaults_substituted: false`, `origin`, and `error`) to
  `stderr`, records it in `copperpour_plugin::last_startup_refusal()`, leaves
  the plugin uninitialized so a later valid call can succeed, and starts no
  server.
- Passing `NULL` continues to use the default search path and falls back to
  built-in defaults only when no configuration file exists.
- The standalone `copperpour` binary now refuses a trailing `--config` with no
  value or a repeated `--config` flag instead of silently falling back to
  search-path discovery or ignoring the second path.

Callers that previously ignored the `kicad_plugin_init` return code or relied
on silent fallback from a broken configuration path must pass a valid file path
or `NULL` (Part of #741).
