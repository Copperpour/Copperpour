### Portable skills installation target (patch release)

`copperpour init`, `status`, and `uninstall` now accept `--skills-dir <absolute path>`
and the `COPPERPOUR_SKILLS_DIR` environment variable. The flag takes precedence,
and the variable is ignored when `--client` is given.
With either override, these commands operate on portable skills only, without
writing Claude agents, hooks, or client-profile installation markers. Without an
override their client-specific behavior is unchanged. The option is not a server
startup or double-click installer setting.

Keep the target's `.copperpour-skills.json` ownership marker. Custom uninstall
preserves modified/unowned files and the target root. Custom init refuses to
replace locally edited files and any file the marker does not list. Files that a
later release no longer bundles are removed by init or uninstall unless edited. See README's Managing skills
section for other clients and update instructions.

Installer-only `--client gemini`, `--client cursor`, and `--client antigravity`
(alias `agy`) select ownership-checked, skills-only roots at `~/.gemini/skills`,
`~/.cursor/skills`, and `~/.gemini/config/skills` respectively. Precedence is
explicit `--skills-dir`, then the preset directory; `COPPERPOUR_SKILLS_DIR` applies
only without `--client`.
Use the same target when checking or removing an installation. These presets do
not register an MCP server or change server-startup client selection. Existing
`claude` means Claude Code; existing `codex` also covers the desktop app. Claude
Desktop skill uploads remain a separate workflow.
