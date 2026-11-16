### New tool `get_placement_overview` in the `placement` toolset (minor release)

Describes a footprint placement as numbers: airwire (ratsnest) length in total,
per net and per footprint, the longest airwires with their pads, crossings
between nets, parts outside the outline, and per net group (the project's
netclasses, or `net_groups`) its nets, parts, region, the other parts inside
that region and the closest pads of other nets. `image: true` adds
a picture as an image block. It reads the board live from KiCad when open
there, else the saved file, as `score_placement` does. No caller needs to
change anything.


How nets are matched:

- **Netclass groups** (the default) use the patterns in the project file's
  `net_settings.netclass_patterns`, matched as KiCad matches them: against
  the whole net name, as a wildcard pattern or as a regular expression,
  whichever names the net. `VCC` does not name `/power/VCC`, and `A.B` names
  `AxB`. A net in several netclasses is in each of those groups.
- **Where it can name fewer nets than KiCad** (measured against pcbnew
  10.0.7): a pattern with a brace, and an expression the Rust `regex` crate
  does not accept (look-around, `\Q...\E`), are matched as wildcards only.
  Netclass directives placed in the schematic are not read, so a net assigned
  only that way is in no group.
- **`net_groups` and `skip_nets`** patterns also match a sheet-local net by
  the name after its last `/`: `G_A` names `/Output stage/G_A`.

`call_tool` can run it: it only reads.
