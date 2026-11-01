//! Skill and agent manifests embedded at compile time.
//!
//! The client-aware `init` subcommand installs shared skills for Claude or Codex.
//! Claude-specific agents and hooks remain scoped to Claude's directories.
//! Hook skills are also patched into `~/.claude/settings.json`.

/// A skill to install to `~/.claude/skills/<name>/SKILL.md`.
/// Optional reference files go into `~/.claude/skills/<name>/references/`.
pub struct SkillManifest {
    pub name: &'static str,
    pub content: &'static str,
    pub references: &'static [(&'static str, &'static str)],
}

/// An agent to install to `~/.claude/agents/<filename>`.
pub struct AgentManifest {
    pub filename: &'static str,
    pub content: &'static str,
}

/// A hook-bound skill: triggers before/after specific MCP tool calls.
/// Installed as a hook entry in `~/.claude/settings.json` that runs
/// `copperpour hook <name>` to emit Claude's structured hook JSON.
pub struct HookSkillManifest {
    pub name: &'static str,
    pub content: &'static str,
    pub board_access: copperpour_core::tools::BoardAccess,
    pub event: &'static str, // "PreToolUse" or "PostToolUse"
}

// ─── Skills ──────────────────────────────────────────────────────────────────

/// The skill with Copperpour's operating rules.
pub const OPERATING_RULES_SKILL: &str = "copperpour";

/// [`OPERATING_RULES_SKILL`]'s name before #239. `copperpour skill` still
/// accepts it; `init` and `uninstall` remove what was installed under it.
pub const LEGACY_OPERATING_RULES_SKILL: &str = "konnect";

pub const SKILLS: &[SkillManifest] = &[
    SkillManifest {
        name: OPERATING_RULES_SKILL,
        content: include_str!("../assets/skills/copperpour/SKILL.md"),
        references: &[(
            "reliability-contract.md",
            include_str!("../../../docs/RELIABILITY_CONTRACT.md"),
        )],
    },
    SkillManifest {
        name: "kicad-schematic",
        content: include_str!("../assets/skills/kicad-schematic/SKILL.md"),
        references: &[
            (
                "common-lib-ids.md",
                include_str!("../assets/skills/kicad-schematic/references/common-lib-ids.md"),
            ),
            (
                "wiring-patterns.md",
                include_str!("../assets/skills/kicad-schematic/references/wiring-patterns.md"),
            ),
        ],
    },
    SkillManifest {
        name: "kicad-pcb",
        content: include_str!("../assets/skills/kicad-pcb/SKILL.md"),
        references: &[
            (
                "layer-reference.md",
                include_str!("../assets/skills/kicad-pcb/references/layer-reference.md"),
            ),
            (
                "trace-width-table.md",
                include_str!("../assets/skills/kicad-pcb/references/trace-width-table.md"),
            ),
            (
                "design-rules.md",
                include_str!("../assets/skills/kicad-pcb/references/design-rules.md"),
            ),
        ],
    },
    SkillManifest {
        name: "kicad-manufacture",
        content: include_str!("../assets/skills/kicad-manufacture/SKILL.md"),
        references: &[
            (
                "jlcpcb-rules.md",
                include_str!("../assets/skills/kicad-manufacture/references/jlcpcb-rules.md"),
            ),
            (
                "gerber-layers.md",
                include_str!("../assets/skills/kicad-manufacture/references/gerber-layers.md"),
            ),
        ],
    },
    SkillManifest {
        name: "kicad-review",
        content: include_str!("../assets/skills/kicad-review/SKILL.md"),
        references: &[
            (
                "error-taxonomy.md",
                include_str!("../assets/skills/kicad-review/references/error-taxonomy.md"),
            ),
            (
                "design-checklist.md",
                include_str!("../assets/skills/kicad-review/references/design-checklist.md"),
            ),
        ],
    },
    SkillManifest {
        name: "kicad-library",
        content: include_str!("../assets/skills/kicad-library/SKILL.md"),
        references: &[],
    },
];

// ─── Agents ──────────────────────────────────────────────────────────────────

pub const AGENTS: &[AgentManifest] = &[
    AgentManifest {
        filename: "kicad-design-review-agent.md",
        content: include_str!("../assets/agents/kicad-design-review-agent.md"),
    },
    AgentManifest {
        filename: "kicad-schematic-build-agent.md",
        content: include_str!("../assets/agents/kicad-schematic-build-agent.md"),
    },
];

// ─── Hook Skills ─────────────────────────────────────────────────────────────

pub const HOOK_SKILLS: &[HookSkillManifest] = &[
    HookSkillManifest {
        name: "pre-pcb-ipc",
        content: "This operation is live-IPC-only. KiCad must be running with the exact requested .kicad_pcb board open. If IPC or board identity fails, ask the user to open that board and retry once; do not bypass the server-side guard or edit the board file behind KiCad.",
        board_access: copperpour_core::tools::BoardAccess::LiveOnly,
        event: "PreToolUse",
    },
    HookSkillManifest {
        name: "pre-pcb-fallback",
        content: "This operation prefers live KiCad IPC but has a guarded closed-board file fallback. Let the tool select the safe path. Never force a file edit while KiCad holds this board open, and treat the server-side board identity and revision checks as authoritative.",
        board_access: copperpour_core::tools::BoardAccess::LivePreferredWithFallback,
        event: "PreToolUse",
    },
    HookSkillManifest {
        name: "pre-pcb-closed",
        content: "This operation requires the requested board to be closed in KiCad because it edits the saved file directly. Ask the user to close that board before applying the change; do not bypass the server-side open-board refusal.",
        board_access: copperpour_core::tools::BoardAccess::ClosedBoardOnly,
        event: "PreToolUse",
    },
    HookSkillManifest {
        name: "pre-pcb-conditional",
        content: "This tool has different board-state requirements for planning and applying. Dry-run or planning is non-mutating; before apply, follow the tool description and returned plan exactly. Do not reuse a stale plan revision or bypass the server-side board-state guard.",
        board_access: copperpour_core::tools::BoardAccess::ApplyModeDependent,
        event: "PreToolUse",
    },
];
