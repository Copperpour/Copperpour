### Transaction journals and scratch files are now `.copperpour-*` (minor release)

Multi-file schematic changes write their journal as
`.copperpour-transaction-<id>.json` instead of `.konnect-transaction-<id>.json`,
and `copperpour transaction abandon` renames it to
`.copperpour-transaction-<id>.abandoned.json`. A `.konnect-transaction-*`
journal left by an earlier release is still listed by `transaction status`,
completed by `transaction recover` and by the check every multi-file change
runs first, and abandoned under its old prefix. An older, Konnect-named build
does not see the new journals, so recover any interrupted change before going
back to one. Atomic-write scratch files are `.copperpour-*`, export staging
directories `.copperpour-export-*` (holding `.copperpour-previous-*`), the
Freerouting SES scratch file `.<name>.copperpour-<uuid>.tmp.ses`, the ERC report
directory `copperpour-erc-*` and the JLCPCB catalog work directory, beside the
database, `copperpour-jlcpcb-*`. Add `.copperpour-transaction-*.json` to any
ignore rule that names the old pattern
([#250](https://github.com/Copperpour/Copperpour/issues/250)).
