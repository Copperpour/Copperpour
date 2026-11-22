### The KiCad plugin drops manual MCP server controls (minor release)

The Copperpour action opens the settings dialog. Existing settings and the
optional native Specctra bridge continue to work. Claude Code, Codex, and
other clients configured with a stdio launch command start Copperpour
automatically.

If you previously used the plugin to launch an HTTP server, pass the existing
plugin `settings.json` to the standalone binary with `--config`. Set
`"transport": "http"` in that file or a copy so the server stays up with stdin
redirected from the null device. For example, on macOS or Linux run
`copperpour --config /path/to/settings.json </dev/null`. Point your client
at the configured endpoint, which defaults to `http://127.0.0.1:3000/mcp`.

The plugin no longer reads or removes the legacy `server.pid` file or stages
binary and config copies on Windows. Stop any legacy plugin-launched process
still running before starting HTTP on the same address.
