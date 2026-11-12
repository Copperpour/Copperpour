//! Skill and agent manifests embedded at compile time.
//!
//! The client-aware `init` subcommand installs shared skills for Claude or Codex.
//! Claude-specific agents and hooks remain scoped to Claude's directories.
//! Hook skills are also patched into `~/.claude/settings.json`.

// The installer and MCP endpoints use one embedded skill/reference catalog.
pub use copperpour_core::guidance::{LEGACY_OPERATING_RULES_SKILL, OPERATING_RULES_SKILL, SKILLS};

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
