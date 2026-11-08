### `route_specctra_dsn` keeps Freerouting's JVM crash reports out of the working directory (patch release)

The Freerouting JVM inherits Copperpour's working directory, usually the KiCad
project, so a HotSpot crash wrote `hs_err_pid<pid>.log` there, and could dump
core there. The JVM now starts with `-XX:ErrorFile` pointing at the
directory that already holds Freerouting's logs (`$XDG_STATE_HOME/freerouting`,
else `~/.local/state/freerouting`, created if missing), or at the temporary
directory when that one is relative or cannot be created, and with
`-XX:-CreateCoredumpOnCrash`. Look for crash reports there; nothing changes in
the tool's arguments or responses.
