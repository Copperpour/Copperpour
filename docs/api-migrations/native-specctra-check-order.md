### Native Specctra export no longer runs Rust-only geometry checks first (minor release)

`export_specctra_dsn` keeps `native_bridge_mode: "disable"` as the Rust-only
default. All modes first validate the stable live IPC snapshot and shared
reverse-manifest/import requirements. On KiCad 10, `require` then attempts the
authenticated native bridge and refuses with a native-path reason if unavailable
or semantically invalid, without running or falling back to the Rust writer.
`prefer` also attempts native first; it uses Rust only if native is unavailable
or invalid, and only if Rust-specific checks pass. A refused fallback reports
the native diagnostic as well as the Rust reason.

The native correlator now uses identities built directly from the validated
snapshot model. The Rust writer alone requires a single closed outline of
Edge.Cuts lines and circular arcs (flattened within `max_error`). KiCad-native
circular Edge.Cuts cutouts can therefore export through `prefer`/`require`.
Native output is still checked against components, pads, nets, layers/types,
routing classes/vias and planes, and the live source revision is rechecked after
receiving a native DSN. Source/hash binding, non-replacing paired publication
and SES validation/apply are preserved.

The native manifest’s `supported_profile.outline` now truthfully labels a
KiCad-native boundary rather than Rust flattening. Manifest schema 4 and older
supported schemas remain readable; this is a label change, not a schema change.
Existing method strings, counts, paths and response keys remain unchanged.

Shared restrictions still apply: representable SMD and all-copper through-hole
pads (including the current per-layer geometry limitation), supported copper stack and zones, locked-routing
inventory, complete effective rules, no rule areas and no custom rules. The
`.kicad_dru` presence refusal now names its SES-import reason explicitly; #75 owns
future custom-rule support. This does not add KiCad 11 native export, repeated-pad
native adoption (#386), or relax existing native parser limitations (#98).
Native export remains an explicit opt-in through the KiCad 10 legacy plugin.
