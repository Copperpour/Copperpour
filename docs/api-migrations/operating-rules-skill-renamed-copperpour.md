### The operating-rules skill is now `copperpour` (minor release)

The bundled skill that holds Copperpour's operating rules installs as
`copperpour` instead of `konnect`: `~/.claude/skills/copperpour/` (Codex:
`~/.agents/skills/copperpour/`), frontmatter `name: copperpour`, and
`copperpour/SKILL.md` in `status` and `get_installation_info`. The bundled
agents preload it under the new name. `init` removes the `SKILL.md` and
`references/reliability-contract.md` it wrote under `konnect/`, and the
directory once it is empty; files you added there are kept, and a `konnect`
or `konnect/references` that is a link is not followed. `uninstall` does the
same. `copperpour skill konnect` still prints the rules. Update your own
skills or agents that preload `konnect`
([#239](https://github.com/Copperpour/Copperpour/issues/239)).
