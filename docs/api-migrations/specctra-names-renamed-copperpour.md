### Specctra jobs use `copperpour` names (minor release)

`export_specctra_dsn` writes its reverse manifest to `<output>.copperpour.json`
by default instead of `<output>.konnect.json`; `manifest_output_path` still
chooses another path. The Rust exporter names the DSN's padstacks
`copperpour_pad_NNNN` and `copperpour_via_NNNN`, its images
`copperpour_image_<ref>` and its net classes `copperpour_class_NNNN`, so
Freerouting's SES carries the same names. A KiCad-native DSN keeps KiCad's
names, as before. `plan_specctra_ses_import` and `apply_specctra_ses` check
the SES against the names in the manifest you pass, so a `.konnect.json`
manifest and its SES from an earlier release still validate. Pass the old
manifest path explicitly; nothing looks for `<output>.konnect.json`
([#250](https://github.com/Copperpour/Copperpour/issues/250)).
