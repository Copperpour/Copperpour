//! Canonical embedded skill catalog shared by MCP and the native installer.

/// A skill to install to `~/.claude/skills/<name>/SKILL.md`.
/// Optional reference files go into `~/.claude/skills/<name>/references/`.
pub struct SkillManifest {
    pub name: &'static str,
    pub content: &'static str,
    pub references: &'static [(&'static str, &'static str)],
}

/// The skill with Copperpour's operating rules.
pub const OPERATING_RULES_SKILL: &str = "copperpour";

/// [`OPERATING_RULES_SKILL`]'s name before #239. `copperpour skill` still
/// accepts it; `init` and `uninstall` remove what was installed under it.
pub const LEGACY_OPERATING_RULES_SKILL: &str = "konnect";

pub const SKILLS: &[SkillManifest] = &[
    SkillManifest {
        name: OPERATING_RULES_SKILL,
        content: include_str!("../../../copperpour/assets/skills/copperpour/SKILL.md"),
        references: &[(
            "reliability-contract.md",
            include_str!("../../../../docs/RELIABILITY_CONTRACT.md"),
        )],
    },
    SkillManifest {
        name: "kicad-schematic",
        content: include_str!("../../../copperpour/assets/skills/kicad-schematic/SKILL.md"),
        references: &[
            (
                "common-lib-ids.md",
                include_str!("../../../copperpour/assets/skills/kicad-schematic/references/common-lib-ids.md"),
            ),
            (
                "wiring-patterns.md",
                include_str!("../../../copperpour/assets/skills/kicad-schematic/references/wiring-patterns.md"),
            ),
        ],
    },
    SkillManifest {
        name: "kicad-pcb",
        content: include_str!("../../../copperpour/assets/skills/kicad-pcb/SKILL.md"),
        references: &[
            (
                "layer-reference.md",
                include_str!("../../../copperpour/assets/skills/kicad-pcb/references/layer-reference.md"),
            ),
            (
                "trace-width-table.md",
                include_str!("../../../copperpour/assets/skills/kicad-pcb/references/trace-width-table.md"),
            ),
            (
                "design-rules.md",
                include_str!("../../../copperpour/assets/skills/kicad-pcb/references/design-rules.md"),
            ),
        ],
    },
    SkillManifest {
        name: "kicad-manufacture",
        content: include_str!("../../../copperpour/assets/skills/kicad-manufacture/SKILL.md"),
        references: &[
            (
                "jlcpcb-rules.md",
                include_str!("../../../copperpour/assets/skills/kicad-manufacture/references/jlcpcb-rules.md"),
            ),
            (
                "gerber-layers.md",
                include_str!("../../../copperpour/assets/skills/kicad-manufacture/references/gerber-layers.md"),
            ),
        ],
    },
    SkillManifest {
        name: "kicad-review",
        content: include_str!("../../../copperpour/assets/skills/kicad-review/SKILL.md"),
        references: &[
            (
                "error-taxonomy.md",
                include_str!("../../../copperpour/assets/skills/kicad-review/references/error-taxonomy.md"),
            ),
            (
                "design-checklist.md",
                include_str!("../../../copperpour/assets/skills/kicad-review/references/design-checklist.md"),
            ),
        ],
    },
    SkillManifest {
        name: "kicad-library",
        content: include_str!("../../../copperpour/assets/skills/kicad-library/SKILL.md"),
        references: &[],
    },
];
