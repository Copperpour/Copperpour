### Messages and tool descriptions say Copperpour (patch release)

Error messages, warnings, `source_evidence.detail` text, update instructions
from `get_installation_info`, and tool and argument descriptions now name the
server Copperpour instead of Konnect. A Specctra DSN from
`export_specctra_dsn` names its `host_cad` as `Copperpour`. Kinds, reasons,
JSON keys and tool names are unchanged; match on those, not on message text.
An SES that Freerouting returns with `host_cad Konnect` still imports.
