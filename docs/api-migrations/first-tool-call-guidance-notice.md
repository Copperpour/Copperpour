### Tool calls surface installed-guidance drift notice (`guidance_notice`) and defer recording until a tool runs (patch release)

When installed Claude or Codex guidance is `out_of_sync` for a new
`(install marker, embedded guidance bundle)` pair, the MCP server now attaches
the one-line drift notice as a top-level `guidance_notice` string on
non-`get_guidance` tool call responses in that drifted session (in addition to
including it in MCP `initialize` `instructions`), and writes
`~/.copperpour/.guidance-checked-<client>` for that `out_of_sync` client only
when the session's first non-`get_guidance` tool call delivers the notice
([#722](https://github.com/Copperpour/Copperpour/issues/722)).

Previously, `.guidance-checked-<client>` was written during `initialize` and
the notice appeared only in `initialize` `instructions`, where clients with
large multi-server system prompts could overlook it and where a non-KiCad
session that initialized the server without calling any Copperpour tool consumed
the one-time notice before any KiCad work ran.

`get_guidance` responses are unchanged (preserving byte-for-byte parity with
`resources/read`), and subsequent sessions whose installed guidance is
`current`, `not_installed`, or already recorded for the current
`(marker, bundle_digest)` pair see no `guidance_notice` field.
