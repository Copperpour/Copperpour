### `export_specctra_dsn` refuses KiCad's native DSN when it merges pads of different drills (patch release)

KiCad's native Specctra exporter names a pad's padstack after its copper
alone, so two pads with the same shape and size but different drills share one
padstack name. The native bridge adopted that DSN, and its reverse manifest
then held the same padstack name twice: `plan_specctra_ses_import` and
`apply_specctra_ses` refused it with "manifest contains an empty or duplicate
padstack name", only once the board had been routed. The export now refuses
the native DSN itself, with "KiCad native DSN gives one padstack '…' to pads
that the Rust export keeps apart: …", naming each pad with its drill. With
`native_bridge_mode` `prefer` the export uses the Rust DSN, which names one
padstack per shape and drill, and reports the refusal in
`native_bridge_diagnostics`; with `require` it fails before any routing. Boards
whose pads do not share a shape across drills are unaffected.
