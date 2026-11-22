### The KiCad plugin drops manual MCP server controls (minor release)

The Copperpour action opens the settings dialog. Existing settings and the
optional native Specctra bridge continue to work. Claude Code, Codex, and
other clients configured with a stdio launch command start Copperpour
automatically.

If you previously used the plugin to launch an HTTP server, copy the plugin's
`settings.json`, set `"transport": "http"` in the copy so the server stays up with
stdin redirected from the null device, and pass the copy with `--config`. For
example, on macOS or Linux run
`copperpour --config /path/to/http-settings.json </dev/null`. Point your client
at the configured endpoint, which defaults to `http://127.0.0.1:3000/mcp`. Don't
set `"http"` in the plugin's own file if a client launches the installed binary
over stdio: that launch reads the same file and would no longer answer on stdio.

The plugin no longer reads or removes the legacy `server.pid` file or stages
binary and config copies on Windows. Stop any legacy plugin-launched process
still running before starting HTTP on the same address.
