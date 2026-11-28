### `check_freerouting` runs the JAR with `-help` and reports `freerouting_version` (patch release)

`check_freerouting` proved that the JAR runs with `java -jar <jar> --version`.
Freerouting has no `--version` option: every release rejects it and exits 1,
so `available` was always `false` and `note` said the JAR did not run, even
when routing worked. The probe now runs `-help`, which every release since 1.9
accepts and exits 0 on. `available`, `version_checked` and `version_output`
keep their names and meaning; `version_output` now holds the `-help` output.

The response adds `freerouting_version`, the version Freerouting logs when it
starts (`2.6.0`), or `null` when the output does not show one. Callers that
treated `available: false` as a broken install can trust it again.
