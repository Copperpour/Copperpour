# Selected alternate on an unlabeled net

Start with `wire_pin_text_kicad10.kicad_sch`. Its connector `U2` already selects
the bidirectional `TMS` function on pin 2. With Copperpour's served tools,
remove the other symbols and wires. In a disposable copy of KiCad 10.0.8's
`Connector` library, keep only pins 2 and 6 of `Conn_ARM_SWD_TagConnect_TC2030`.
Make pin 2 a hidden `power_in` named `LEGACY_RAIL` and pin 6 a passive pin named
`AUX`. Refresh the embedded symbol without replacing the placed instance, then
join its two pins with a wire. Transfer the symbol and wire to a blank root
sheet so only their library definition is carried over. Save the standalone
sheet with `kicad-cli sch upgrade --force pin_fallback_alternate.kicad_sch`
using KiCad 10.0.8.

`kicad-cli sch export netlist --format kicadxml -o witness.xml
pin_fallback_alternate.kicad_sch` gives one net, `Net-(U2-AUX)`, on both KiCad
10.0.6 and 10.0.8. Its only nodes are `U2.2` (bidirectional `TMS`) and `U2.6`
(passive `AUX`). There are no labels or power symbols. The hidden pin names
the net `LEGACY_RAIL` only if naming incorrectly uses its base electrical type
instead of the selected alternate.
