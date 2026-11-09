### `export_specctra_dsn` supports rounded board outlines (minor release)

The Rust exporter used to refuse `Edge.Cuts` arcs. It now flattens circular
arcs into line segments within the board project's `max_error` tolerance,
keeping one closed `Edge.Cuts` loop. Boards without a `.kicad_pro` file use
KiCad's 5 µm default. Unsupported outline geometry and multiple or open loops
remain refused.

The reverse manifest's `supported_profile.outline` now describes a closed
loop of lines and circular arcs flattened within `max_error`; callers should
not rely on the former straight-lines-only description.
