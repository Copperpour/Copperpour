### Hierarchical sheet pins use KiCad's UUID field order (patch release)

When Copperpour writes a hierarchical sheet pin, its `(uuid …)` follows `(at …)` and precedes `(effects …)`, matching KiCad. UUID values and tool arguments and responses are unchanged. Callers do not need to change their requests.
