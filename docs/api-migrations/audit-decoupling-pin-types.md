### `audit_decoupling` picks supply pins by electrical type (patch release)

`audit_decoupling`, and with it `run_design_review`, chose the pins to audit
by name alone. It now audits every `power_in` pin, whatever its name
(`IOVDD`, `VREG_IN`), and applies the name list only to pins typed
`unspecified` or `passive`. A pin of any other type is not audited even when
its name matches, so a bidirectional `3V3_EN` and a `power_out` `3V3` are no
longer reported. Nor is a pin under a no-connect flag, a ground pin (its name
or a name of its net is a ground name as KiCad's ERC reads one: containing
`GND`, or `EARTH`, `EARTH_…`, `VSS` or `VSSA`), or a power symbol's pin. A
hidden `power_in` pin is on the net its name names, as KiCad nets it, so a
capacitor on that rail decouples it. `total_power_pins` and `pass_count` count
the pins audited. Issue: [#120](https://github.com/Copperpour/Copperpour/issues/120).
