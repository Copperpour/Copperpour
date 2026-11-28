### Freerouting discovery takes the highest JAR version in a directory (patch release)

When `route_specctra_dsn` or `check_freerouting` is called without `jar_path`
and one searched directory holds several Freerouting JARs, Copperpour now uses
the one with the highest version in its file name (`freerouting-2.6.0.jar`
over `freerouting-2.5.0.jar`, `2.10.0` over `2.9.0`, a release over a
`-SNAPSHOT` of the same version). It used to take the first in name order,
which kept an old JAR in use after a newer one was installed next to it. The
order in which locations are searched is unchanged. Pass `jar_path` to pin a
specific JAR.
