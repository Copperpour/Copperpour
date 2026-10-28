### The native bridge registers only where the server looks (patch release)

The KiCad plugin now picks its native-bridge directory by the server's rule.
It ignores an empty or relative `XDG_DATA_HOME` (and, on Windows,
`LOCALAPPDATA`) instead of writing below KiCad's working directory. If no
absolute directory remains, the settings dialog shows the bridge as
unavailable and it stays off. On
Windows the server reads the LocalAppData known folder, so a `LOCALAPPDATA`
changed to name another folder still leaves the two apart.

`COPPERPOUR_BRIDGE_DIR` must be an absolute path on both sides. A relative
value, or one starting with `~`, now leaves the bridge off, and
`export_specctra_dsn` reports it in `native_bridge_diagnostics`. Before this,
the plugin and the server each resolved it against their own working
directory ([#181](https://github.com/Copperpour/Copperpour/issues/181)).
