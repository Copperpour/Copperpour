### `set_custom_rule` (minor release)

A new `verification` tool adds or replaces one named rule in the board's
sibling `.kicad_dru` custom rules file:
`set_custom_rule(board, name, constraints, condition?, layer?, severity?)`
([#108](https://github.com/Copperpour/Copperpour/issues/108)). Each constraint
is `{type, min?, opt?, max?}`, for the types whose values are min, opt and max;
values are written in mm, except `track_angle` and `via_count`, which KiCad
reads without units. `layer` is a layer name as the board names it, `outer` or
`inner`. A rule of the same name is replaced where it stands; a new one goes at
the end, where KiCad gives it precedence over every earlier rule that matches
the same items. The rest of the file is left as written. A missing file is
created with `(version 1)`.

`kicad-cli` drops a rules file it cannot parse and checks against KiCad's
implicit rules alone, without a word. Before writing, the tool checks the whole
resulting file as KiCad 10's rule parser and expression compiler read it, and
when any rule, old or new, has a fault it returns `plan_blocked` with each fault
in `reasons`, naming its rule and line, and writes nothing. That check does not
know property names in conditions (`A.Width`), which KiCad looks up at run
time, so the tool then runs `kicad-cli pcb drc` on a copy of the board with the
file and a probe rule that fails on every item. When kicad-cli does not report
the probe, or cannot run, the tool returns `plan_blocked` and writes nothing. A
board with no item yet gives the probe nothing to fail on, and no rule anything
to act on, so there the first check alone decides. The response gives
`rules_file`, `name`, whether the file was `created` and `changed`, `previous`
and `current`, the rule's text before and as read back, `kicad_checked`, false
when the probe did not run, and `unchecked_properties`, the property names the
file uses that no check verified. Two rules of the name return
`ambiguous_target`; a file saved after the read returns `conflict`.

No existing tool's arguments or responses change. `verification` rises from 10
to 11 tools.
