### Portable skills installation target (patch release)

`copperpour init`, `status`, and `uninstall` now accept `--skills-dir <absolute path>`
and the `COPPERPOUR_SKILLS_DIR` environment variable. The flag takes precedence.
With either override, these commands operate on portable skills only, without
writing Claude agents, hooks, or client-profile installation markers. Without an
override their client-specific behavior is unchanged. The option is not a server
startup or double-click installer setting.

Keep the target's `.copperpour-skills.json` ownership marker. Custom uninstall
preserves modified/unowned files and the target root. Custom init refuses to
replace differing unowned or locally edited files. See README's Managing skills
section for other clients and update instructions.
