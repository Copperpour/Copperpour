### The IPC client, temp directories and bridge thread use `copperpour` names (minor release)

Copperpour names itself `copperpour-<pid>` instead of `konnect-<pid>` in every
KiCad IPC request header, and the plugin's native bridge thread is
`copperpour-native-bridge`. The schematic viewer renders into
`<temp>/copperpour-viewer-<pid>` instead of `<temp>/konnect-viewer-<pid>`.
`get_schematic_view` writes its SVG under `<temp>/copperpour-schematic-views/`
instead of `<temp>/konnect-schematic-views/`, so the path it returns changes
too; nothing reads the old directory any more, and it can be deleted. Update
any sandbox rule, log filter or temp-dir cleanup that matches the old names
([#250](https://github.com/Copperpour/Copperpour/issues/250)).
