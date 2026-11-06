### `check_freerouting` and routing pick the Java they run, and report it (patch release)

Freerouting used to start with a bare `java`, the first one on the `PATH`. It
now runs `$JAVA_HOME/bin/java` when `JAVA_HOME` is set, then the `java` on the
`PATH`, and on macOS Homebrew's `openjdk` before the `/usr/bin/java` stub. On
Unix a candidate without an execute bit is skipped for the next one.

`check_freerouting` adds `java_path`, `java_release` and
`required_java_release` (the release the JAR's main class was built for). When
the Java found is older than the JAR needs, it answers `available: false` with
a note, instead of `available: true` and an `UnsupportedClassVersionError` at
routing time. `available` is now `true` only when the JAR actually ran
(`version_checked`); before, a JAR and any `java` were enough. When the JAR
does not run, `note` says so and points at `version_output`. Callers that
read `available` alone get the stricter answer and need no change.
