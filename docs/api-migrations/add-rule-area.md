### `add_rule_area` adds live KiCad rule areas in the `pcb_rule_areas` toolset (minor release)

The new `pcb_rule_areas` toolset provides `add_rule_area` for creating netless KiCad rule areas (keep-out zones) on one or more enabled copper layers of a live board open in KiCad (`ZT_RULE_AREA`, `RuleAreaSettings`).

- Accepts `layers` (specific enabled copper layers or `"*.Cu"`), either `points` or `rect` geometry in millimetres, optional `name`, and KiCad's five keepout flags (`keepout_tracks`, `keepout_vias`, `keepout_pads` defaulting to `true`; `keepout_copper_pours`, `keepout_footprints` defaulting to `false`).
- Requires a live KiCad PCB Editor holding the exact requested board (no closed-board file fallback in this increment); validates enabled layers before opening a commit, verifies post-commit readback by KIID, refills copper zones afterward (reporting an `applied_but_not_refilled` partial outcome if refill fails), and uses the shared `outcome` envelope.
- Creating a rule area does not run autorouting. The [Specctra rule-area profile](specctra-rule-areas.md), added by [#81](https://github.com/Copperpour/Copperpour/issues/81), supports straight routing keep-outs with whole-micrometre vertices and lists the forms still refused by export.
