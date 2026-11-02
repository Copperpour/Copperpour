### `evidence_source` values, the viewer's app id and the bridge's server name use `copperpour` names (minor release)

`get_editor_state` capabilities now report `evidence_source` as
`copperpour_bundled_kicad_protocol` instead of `konnect_bundled_kicad_protocol`,
and `copperpour_saved_structure_and_typed_live_context` instead of
`konnect_saved_structure_and_typed_live_context`. The old values are no longer
returned, so a caller that matches them should accept both while it talks to
older servers.

The schematic viewer's Tauri identifier is
`com.github.copperpour.copperpour.viewer` instead of
`com.github.mixelpixx.konnect.viewer`. On Linux and Windows it names the
viewer's WebView data directory under the local data directory (`~/.local/share`,
`%LOCALAPPDATA%`). The viewer stores no settings, so nothing is lost: it starts
with a new directory, and the old one can be deleted.

The plugin's native bridge sends `Server: CopperpourNativeBridge/1` instead of
`KonnectNativeBridge/1`
([#250](https://github.com/Copperpour/Copperpour/issues/250)).
