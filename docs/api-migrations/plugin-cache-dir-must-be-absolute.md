### The plugin's cache directory is never relative (patch release)

The KiCad plugin keeps `server.pid`, and on Windows the staged server binary,
in `%LOCALAPPDATA%\copperpour\cache` on Windows and `~/.cache/copperpour/cache`
elsewhere. It no longer reads `LOCALAPPDATA` outside Windows, or falls back to
`%USERPROFILE%\.cache` on Windows. A server recorded under the old directory
is not reaped on the next start; stop it once by hand.

When that directory would be missing or relative to KiCad's working directory
(an unset, empty or relative `LOCALAPPDATA` on Windows, a relative `HOME`
elsewhere), the plugin writes no PID file and runs the server from the plugin
directory unstaged. Stop Server then reaches only the server started by the
same plugin instance: not one orphaned by a crashed session, and not one
started before a plugin reload
([#190](https://github.com/Copperpour/Copperpour/issues/190)).
