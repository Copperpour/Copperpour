### New tool `get_placement_overview` in the `placement` toolset (minor release)

Describes a footprint placement as numbers: airwire (ratsnest) length in total,
per net and per footprint, the longest airwires with their pads, crossings
between nets, parts outside the outline, and per net group (the project's
netclasses, or `net_groups`) its nets, parts, region, the other parts inside
that region and the closest pads of other nets. A group or skip pattern also
matches a sheet-local net by the name after its last `/`. `image: true` adds
a picture as an image block. It reads the board live from KiCad when open
there, else the saved file, as `score_placement` does. No caller needs to
change anything.
