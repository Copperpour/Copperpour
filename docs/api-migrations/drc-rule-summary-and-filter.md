### `run_drc` reports per-rule counts and accepts a rule include list (minor release)

`run_drc` adds `by_rule`, keyed by exact reported rule names:

```json
{"by_rule": {"clearance": {"errors": 7, "warnings": 2}}}
```

Counts cover the complete result after severity and rule filtering, before the
display `limit`. `limit: 0` still returns the breakdown. Each bucket counts errors
and warnings, as the existing global counters do; informational rows remain in
`filtered_count`/`violations` when requested with `severity: "info"`. A rule with
only informational rows has a bucket with zero errors and zero warnings.

Pass an optional `rules` array to inspect a subset without repeated findings
from other rules consuming the returned sample:

```json
{"board": "/absolute/project/board.kicad_pcb", "rules": ["clearance"], "limit": 10}
```

Names match case-sensitively against the response's `rule` values. Omitted
`rules` includes all names; `rules: []` includes none. Duplicate names are
coalesced and unknown names yield no matching rows. A malformed present list,
including `null` or any non-string item, returns `invalid_argument` before DRC
or report work. The new `rules_filter` field is the sorted unique effective
selection, or `null` when the argument was omitted.

`filtered_count`, global `errors`/`warnings`, `by_rule`, the displayed list,
`shown`, and `truncated` all use both filters. Existing calls without `rules`
keep their previous counts/order and gain the new metadata. Full-report
`total_violations` and category counts retain their existing unfiltered meanings;
missing categories remain `null`/`categories_not_reported`, not checked zeroes.

Rule selection does not reduce KiCad's checks, change synchronization/refill
behavior, or filter the complete optional `output` report. It reduces response
content; it does not make the DRC engine run faster.
