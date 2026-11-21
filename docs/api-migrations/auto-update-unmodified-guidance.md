### Unmodified Claude/Codex guidance auto-updates on upgrade and drift notices use a runnable `init` path (patch release)

`copperpour init` now writes a sibling manifest
(`~/.copperpour/.installed-manifest-claude.json` or
`~/.copperpour/.installed-manifest-codex.json`) recording the SHA-256 digests of
installed skills, references, and agents plus managed Claude hook entries
([#731](https://github.com/Copperpour/Copperpour/issues/731)).

When an upgraded MCP server scans an `out_of_sync` Claude or Codex install whose
manifest version matches `.installed-<client>`:

- Unmodified files and managed sole-handler hook entries matching the recorded
  manifest (and newly bundled files) are updated atomically in place. If all
  items become `current`, `.installed-<client>`, `.installed-manifest-<client>.json`,
  and `.guidance-checked-<client>` advance without emitting `guidance_notice`.
- User-modified files, user-deleted files, user-edited or shared hook entries,
  symlinks, and legacy installs without a matching manifest are left untouched.
  Remaining drift emits `guidance_notice` naming any locally modified files
  (`(locally modified: ...)`).
- Drift notices now include the serving executable's runnable path (quoted when
  needed) instead of bare `copperpour init` whenever bare `copperpour` on `PATH`
  does not resolve to the serving binary (e.g. KiCad PCM plugin installs).
- `copperpour init` skips rewriting unchanged files and reports updated vs.
  unchanged skills, agents, and hooks in its progress output and final summary.
