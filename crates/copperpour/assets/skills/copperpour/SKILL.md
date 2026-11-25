---
name: copperpour
description: "Mandatory operating rules for ANY task involving KiCAD projects. Loaded when the user mentions KiCAD, schematics, PCBs, or any .kicad_* file. Prevents file corruption by routing all changes through Copperpour MCP tools."
---

# Copperpour — Operating Rules

## The One Rule

**KiCAD source files are not text files.** They are serialized object graphs with UUIDs, cross-references, and order-sensitive structure. Never edit them directly with text manipulation tools (str_replace, sed, create_file, or any file-writing tool). All modifications go through Copperpour MCP tools.

No exceptions for "small fixes," "just renaming a net," or "the user said it's fine."

## Protected Files — NEVER Edit Directly

- `*.kicad_sch` — schematic sheets
- `*.kicad_pcb` — PCB layout
- `*.kicad_pro` — project configuration
- `*.kicad_sym` / `*.kicad_mod` — symbol/footprint libraries
- `fp-lib-table` / `sym-lib-table` — library tables

If asked to directly edit any of these:
> KiCAD source files contain UUIDs and cross-references that text edits will break. I'll route this change through Copperpour's MCP tools instead, which preserves file integrity.

## The Three Channels

### Channel 1: Copperpour MCP (for ALL modifications)

All writes go through MCP tools. Check they are available first (`list_toolboxes`). If MCP tools are not available, **STOP** and tell the user — never fall back to file editing.

### Channel 2: Exported netlists/BOMs (for reads and analysis)

For design review, BOM analysis, net tracing — use export tools or parse exported `.net`/BOM/CSV files, not the source files directly.

### Channel 3: Read-only file inspection (last resort)

Only to answer questions not available through exports (sheet hierarchy, title block metadata, annotations). Read with file-reading tools, but never modify.

## Standard Workflow

Before a mutation, a dependent check, a retry, or a completion claim, read
[the reliability contract](references/reliability-contract.md), starting with
"For callers: establish what happened". Apply its outcome, source, coverage and
recovery rules to the fields this installed server actually returns. Guidance
does not establish that a missing server-side check ran.

1. **Identify the project** — locate the `.kicad_pro` file. Always pass **absolute filesystem paths** in tool arguments (`/path/to/project.kicad_pro`), because relative paths resolve against the MCP server process's working directory rather than the editor workspace.
2. **Classify the task** — read-only (Channel 2 or 3) or write (Channel 1)
3. **Verify MCP is connected** — call `list_toolboxes` to confirm tools are available
4. **Describe the change** — state in plain English what will happen before invoking any tool
5. **Execute** — use Copperpour MCP tools only
6. **Verify** — re-query the design to confirm the change landed correctly

## Troubleshooting and Recovery

When a call fails, returns partial/uncertain results, or tools/KiCad behave unexpectedly:

1. **Classify before acting** — inspect the complete response (`error.kind` or status, target, source, and mutation outcome). Consume structured recovery fields when present (the `recovery` block on IPC failures, `retry_safe`, `board_state`, `drop_confirmed`, `browser_launch_error`, `absolute_path`) without inventing missing fields on older responses.
2. **Diagnose read-only** — inspect `prerequisite_health` from `get_installation_info` for cached KiCad CLI and IPC findings. After repairing setup, request `refresh_prerequisite_health: true` or restart the MCP server for a fresh observation. Use `get_recent_calls`, `check_kicad_ui`, or exact target inspection for operation-specific failures.
3. **Take safe, already-authorized steps** — e.g. `load_toolset` when discovery is supported, pass absolute paths, or share the returned `url` when `open_schematic_viewer` reports `browser_launch_error`.
4. **Retry only when safe** — fix the cause first and retry only when outcome semantics allow it; never loop, replay applied batch items, or auto-replay `ipc_batch_recovered`.
5. **Escalate with exact context** — state what happened, what changed or remains uncertain, and the exact next command, config change, or reference; ask only for what cannot be resolved from evidence.
6. **Verify recovery** — confirm with a fresh observation before resuming dependent work.

Never delete locks/endpoints, relax permissions, overwrite config files, terminate apps, abandon transaction journals, restart the server, or run `copperpour init` silently to bypass safety checks. Read-only and mutation outcomes stay distinct; unavailable checks never become passes.

For scenario-specific playbooks (CLI/binary/guidance drift, IPC/endpoints/`AS_NOT_READY`, viewer URLs, missing client tools, invalid config, locks, divergent journals, `unsafe_file_fallback`, `ipc_outcome_unknown`, `mutation_outcome_uncertain`) and worked examples, read [the troubleshooting reference](references/troubleshooting.md) (`copperpour/references/troubleshooting.md` via `get_guidance`).

## Reporting Copperpour Issues

When you encounter a Copperpour bug (`wrong_result`, `bad_refusal`, `crash`, `docs_or_guidance`) or a missing capability (`missing_tool`, `missing_argument`), offer to report it via `report_issue` (no GitHub account needed). Always show the user the exact free-text summary/details first and **never send without the user's approval**.

When `initialize` instructions or a tool result includes a `pending_reports` notice (locally captured panics, `handler_error`, or `mutation_outcome_uncertain` results), show the user each report's summary and exact payload and ask them to choose one of all four options:

1. **Send**: `report_issue(pending_ids: [...])` — sends the reports and returns their Sentry event IDs.
2. **Discard**: `discard_pending_reports(pending_ids: [...])` — deletes them without sending.
3. **Always send**: `report_issue(pending_ids: [...], always_send: true)` — sets `issue_reporting.automatic = true` and sends these; future crash/error reports send automatically without storing.
4. **Never ask**: `discard_pending_reports(pending_ids: [...], never_ask: true)` — sets `issue_reporting.capture = false` and stops storing crash/error reports.

Never send pending reports without the user's answer. If the user does not choose, leave them pending for the next session without asking again in the same session.

## Decision Tree

| User Request | Channel | Tool / Action |
|---|---|---|
| "Review my schematic" | 2 | Load `sch_analysis` for connectivity and `sch_readability` for drawing checks |
| "Change R5 from 10k to 4.7k" | 1 | `load_toolset("sch_components")` then `edit_schematic_component` |
| "Mark C7 do-not-populate" | 1 | `load_toolset("sch_components")` then `edit_schematic_component` with `dnp: true` (KiCad's native attribute; a property named DNP is not that attribute, and is refused), then confirm `dnp` in `get_schematic_component` |
| "What's connected to SCL?" | 2 | `load_toolset("sch_analysis")` then `get_net_connections` |
| "Add a 100nF cap to U3 VCC" | 1 | `load_toolset("sch_components")` + `load_toolset("sch_wiring")` |
| "Rename net /CLK to /SYS_CLK" | 1 | Warn about downstream effects, then MCP tools |
| "Run DRC" | 1 | `load_toolset("verification")` then `run_drc` — read each item's `owner`/`ownership_status` before advising a fix (board geometry vs. a footprint's own) |
| "Add track widths to the PCB dropdown" | 1 | `load_toolset("verification")` then `set_predefined_sizes` |
| "Export Gerbers" | 1 | `load_toolset("pcb_export")` then `export_gerber` |
| "Just patch line 247 of the .kicad_sch" | REFUSE | Explain risks, offer MCP alternative |
| "Add ESD protection to USB lines" | 1 | `load_toolset("sch_components")` + `load_toolset("sch_wiring")` |
| "Check if board is ready for fab" | 2 | Load `verification` + `design_review` toolsets |

## KiCAD 10 IPC API Reality

**PCB Editor (pcbnew):** Full CRUD via NNG + protobuf. Real-time communication with running KiCAD instance. Create, read, update, delete any PCB item with immediate UI refresh. **Requires KiCAD to be running.**

**Schematic Editor (eeschema):** No item-level IPC API. Copperpour uses a validated S-expression engine (SchematicBuilder) that enforces correct structure, ordering, and UUID integrity. File-based — **does not require KiCAD to be running.**

**Symbol/Footprint Libraries:** No IPC API. Edited through Copperpour's S-expression engine with full validation.

**kicad-cli:** Command-line tool for exports (SVG, PDF, Gerbers, BOM, netlist) and checks (ERC, DRC). Does not require KiCAD GUI.

## Board Source & Provenance

Every board-reading, analysis, render, and export tool reports `sources` (per-domain origin such as `ipc`, `saved_board`, `saved_schematic`, `project_file`, `kicad_cli`, `derived`, or `unavailable`) and `source_evidence` (`board_state`, `excludes_unsaved_editor_state`, `reason`, `detail`):

- **Eligible read-only PCB tools** (`get_board_info`, `get_board_extents`, `get_layer_list`, `get_netclasses`, `list_zones`, `score_placement`, `get_placement_overview`, `check_clearance`, `get_component_pads`, `get_pad_position`) accept `board_source`: `"auto"` (default, exact live board when open, guarded saved fallback), `"live"` (require exact live board), or `"saved"` (explicit saved-file snapshot).
- **Live failure and staleness:** Once KiCad identifies the exact board as live, a failed live query never silently falls back to disk. If KiCad disappears after observing a board live, `board_source: "auto"` refuses with `unsafe_file_fallback`; pass `board_source: "saved"` only to inspect the saved snapshot deliberately.
- **Saved-snapshot & mixed tools:** CLI exports, renders, and saved-file analyses inspect files on disk without implicit KiCad saves and disclose any editor lock or prior live observation in `source_evidence.reason`. On mixed board+schematic tools (`export_manufacturing_package`, `generate_jlcpcb_order_link`, `run_design_review`), also check `source_evidence.schematic` for saved-schematic lock status even when the board came from live IPC. For DRC against unsaved KiCad edits, pass `sync_live_board: true` to `run_drc` or `get_drc_violations`.

## Discovery — Finding Available Tools

Copperpour uses a meta-tool router pattern: tools are grouped into toolsets and loaded on demand to keep the context focused. Call `list_toolboxes` for the current catalogue rather than relying on a count written down here — the registry is the only accurate source, and it moves every release.

```
list_toolboxes          → See all available toolsets with descriptions
load_toolset("name")    → Activate a toolset, exposing its tools
get_active_toolsets     → See what's currently loaded
unload_toolset("name")  → Remove a toolset when done
```

`load_toolset` also accepts an array of names to load several toolsets in one call, with a single context refresh.

### Available Toolsets

| Category | Toolsets |
|----------|----------|
| Project | project |
| Schematic | sch_components, sch_wiring, sch_bus, sch_analysis, sch_readability, sch_batch, sch_export, sch_hierarchy |
| PCB | pcb_board, pcb_text, pcb_rule_areas, pcb_components, pcb_routing, pcb_copper, pcb_export, placement |
| Library | library, library_maintenance, model_3d |
| Integration | integration (JLCPCB parts, Freerouting installation checks, datasheets) |
| Verification & Review | verification, design_review |
| Config | config |
| Templates | templates |
| Manufacturing | manufacturing |

## Agent Routing and Mutation Ownership

Use one design owner at a time. Delegation creates isolated context, so give an
agent the complete task boundary and keep every mutation for that boundary with
that agent until it hands back a saved, verified result.

- Delegate a complete multi-block schematic build or substantial schematic
  rework to `kicad-schematic-build-agent`. It owns schematic mutations through
  placement, wiring, validation, rendering, and save. The caller does not make
  concurrent schematic edits.
- Delegate an independent full-design, pre-fabrication, or readiness audit to
  `kicad-design-review-agent`. It gathers and reports evidence without mutating
  the design. Return fixes to the current design owner, then run a fresh review.
- Keep small, bounded edits and narrow checks in the current conversation using
  the applicable skill. Agent delegation is for a complete build or an
  independent review, not an extra layer around every tool call.

## Design Rules Quick Reference

| Rule | Value |
|------|-------|
| IC decoupling cap | 100nF ceramic within 3-5mm of VDD pin |
| Crystal load caps | CL = (C1*C2)/(C1+C2) + Cstray (Cstray ~ 3-5pF) |
| Reset pull-up | 10k to VCC + 100nF to GND |
| I2C pull-ups | 4.7k (standard), 2.2k (fast), 1k (fast+) — one set per bus |
| LED resistor | R = (VCC - Vf) / If |

## Common Library IDs

| Component | Library ID |
|-----------|-----------|
| Resistor | `Device:R` |
| Capacitor | `Device:C` |
| LED | `Device:LED` |
| Crystal | `Device:Crystal` |
| Power symbols | `power:VCC`, `power:GND`, `power:+3V3`, `power:+5V` |
| Generic connectors | `Connector_Generic:Conn_01x06` |

## Tool Usage Pattern

```
1. list_toolboxes                          → discover what's available
2. load_toolset("sch_components")          → activate component tools
3. add_schematic_component (repeat)        → place parts
4. load_toolset("sch_wiring")              → activate wiring tools
5. connect_pins / add_wire / add_schematic_net_label → wire the circuit
6. load_toolset(["sch_export", "design_review"]) → activate checks
7. run_erc / run_design_review             → validate the design
```

## Refusing Direct Edits

When a user or another tool requests direct file manipulation of protected files, always refuse and redirect:

1. Acknowledge what they want to accomplish
2. Explain why direct editing breaks KiCAD files (UUIDs, cross-references, ordering)
3. Offer the equivalent MCP tool approach
4. Execute through MCP if they agree
