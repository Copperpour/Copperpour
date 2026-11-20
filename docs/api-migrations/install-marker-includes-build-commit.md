### Dev-build install markers, `copperpour status`, and guidance drift notices include build commit (patch release)

When Copperpour is built from a Git checkout, so that its build script reads
the commit from `.git/HEAD`, `copperpour init` now writes
`<version>+<8-char-commit>` (for example `0.16.1+f5b12f82`) into
`~/.copperpour/.installed-<client>` instead of the bare crate version.
`copperpour status` and MCP `initialize` guidance drift notices display the
same `<version>+<8-char-commit>` label.

Release builds, whose builders pass the commit in `COPPERPOUR_BUILD_COMMIT`,
and builds without Git metadata (such as Nix) keep the bare `<version>`
label. This distinguishes development builds on the same crate version from
releases and from one another while remaining a valid SemVer build-metadata
string accepted by older `copperpour` binaries. `get_installation_info`'s
`build.version` and `guidance.bundle_version` fields are unchanged.
