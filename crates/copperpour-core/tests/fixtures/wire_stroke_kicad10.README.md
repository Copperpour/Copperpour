# New-wire stroke fixture

`wire_stroke_kicad10.kicad_sch` is KiCad 10.0.8's resave of
`power_orientation_kicad10/power_orientation.kicad_sch` after one `add_wire` call, so it
shows how KiCad writes the wire Copperpour adds (#899).

## Provenance

Copperpour's stdio server, built from `fix/new-wire-stroke` (on `main` at `70ffae55`), ran
this call on a copy of the sheet named `power_orientation.kicad_sch`, with a minimal
`power_orientation.kicad_pro` beside it so that the resave keeps the instance records'
project name:

```json
{"meta":{"filename":"power_orientation.kicad_pro","version":3}}
```

The call:

```text
add_wire  x1 152.4  y1 101.6  x2 165.1  y2 101.6
```

KiCad 10.0.8 (Windows) then parsed and force-resaved the result in place, and the line
breaks were stored as LF:

```text
kicad-cli sch upgrade --force power_orientation.kicad_sch
```

```text
sha256 a317101c9b0d109254478f5cb62d23f5cb954fe0a9d7015778d3bbb9dbb27e43
```

## What it shows

KiCad writes the new wire as Copperpour wrote it, stroke included:

```text
	(wire
		(pts
			(xy 152.4 101.6) (xy 165.1 101.6)
		)
		(stroke
			(width 0)
			(type default)
		)
		(uuid "7fe9d4d4-e5ee-4d88-b6c5-8a9769eed22b")
	)
```

This follows KiCad's source: a new `SCH_LINE` gets width 0, `LINE_STYLE::DEFAULT` and no
colour (`SCH_LINE::SCH_LINE`), which `STROKE_PARAMS::Format` writes as
`(stroke (width 0) (type default))`. The only other difference from Copperpour's output is
the order of the sheet's wires, which KiCad sorts on save. Otherwise the fixture is the
source sheet line for line.
