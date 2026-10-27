# Bus connectivity fixture

`bus_connectivity_kicad10.kicad_sch` is a KiCad 10 schematic with one bus,
one bus entry, one wire-side signal, a bus label, and a signal label at the far
end. It was
saved through `kicad-cli sch upgrade --force` before being committed.

The fixture captures the mixelpixx/Konnect#328 positive case: the entry's `(at)` corner
is on the bus and its `(at + size)` corner terminates the wire. The bus label
is attached to bus geometry without making the bus an ordinary electrical
net.

KiCad comparison command:

```powershell
kicad-cli sch erc --format json --output bus_connectivity_kicad10.erc.json bus_connectivity_kicad10.kicad_sch
```

KiCad 10.0.6 reports one `label_dangling` violation for the intentionally
pin-less `D0` global label. It reports no `wire_dangling` violation for the
wire/entry pair and no violation for the `D[0..3]` label on the bus. That is
the comparison relevant to mixelpixx/Konnect#328: KiCad recognizes the wire-side entry
termination and bus-label attachment, while its separate rule still expects a
component pin on the scalar signal.
