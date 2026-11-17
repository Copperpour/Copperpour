//! `get_teardrop_settings` and `set_teardrop_settings`: KiCad's native pad and
//! via teardrop settings on the board open in KiCad (#555).
//!
//! Both need KiCad 10.0.7 or newer, read from the bound editor: earlier
//! releases neither send nor read the settings, and silently drop an update
//! carrying them. An unknown version is refused the same way, before any read
//! or write.
//!
//! What KiCad does with a change is recorded in `copperpour_ipc`'s
//! `client::teardrops` module and its live test: the settings commit itself
//! rebuilds the teardrops of the edited pads' footprints and of the edited
//! vias, in the same undo step. `refill_zones` is the opt-in, board-wide step:
//! KiCad's fill-all refills every zone and rebuilds every teardrop. The
//! response reports the settings, the commit's teardrop copper and the
//! teardrop copper observed after the refill request separately.

use crate::mcp::error::ToolErrorKind;
use crate::mcp::protocol::{CallToolResult, ToolContent};
use crate::tool;
use crate::tools::{
    get_path, ipc_failure_result, response_limit, with_bound_board_ipc_classified, BoardAccess,
    BoardBinding, BoundedList, ToolContext, ToolDef,
};
use copperpour_ipc::builders::{mm_to_nm, nm_to_mm};
use copperpour_ipc::client::{
    supports_teardrop_settings, IpcTeardropApplyOutcome, IpcTeardropEdit, IpcTeardropRefusal,
    IpcTeardropSettings, IpcTeardropTarget, IpcTeardropTargetKind, IpcTeardropZone, KiCadIpcClient,
    TEARDROP_SETTINGS_MIN_KICAD_VERSION,
};
use copperpour_ipc::gen::kiapi::common::types::DocumentSpecifier;
use copperpour_ipc::IpcKiCadVersion;
use serde_json::{json, Map, Value};
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

/// How many targets and zones a `set_teardrop_settings` response lists.
const LISTED: usize = 200;
const UNDO: &str = "One KiCad undo step reverts the settings and the teardrops KiCad rebuilt \
                    with them. A refill is a separate KiCad operation.";

/// The ratio ranges KiCad's pad properties dialog accepts
/// (`dialog_pad_properties_base.cpp`, 10.0.8).
const BEST_LENGTH_RATIO: (f64, f64) = (0.2, 2.0);
const BEST_WIDTH_RATIO: (f64, f64) = (0.6, 1.0);
const MAX_TRACK_WIDTH_RATIO: (f64, f64) = (0.0, 1.0);
/// The longest length limit accepted, in millimetres: far past any pad.
const MAX_LIMIT_MM: f64 = 1000.0;

fn min_version() -> String {
    let (major, minor, patch) = TEARDROP_SETTINGS_MIN_KICAD_VERSION;
    format!("{major}.{minor}.{patch}")
}

fn settings_schema() -> Value {
    json!({
        "type": "object",
        "description": "Settings to set. Omitted fields keep each target's current value; at least one is required.",
        "properties": {
            "enabled": { "type": "boolean" },
            "curved_edges": { "type": "boolean" },
            "allow_multiple_track_segments": { "type": "boolean", "description": "Let a teardrop span two track segments when the first is too short" },
            "prefer_zone_connection": { "type": "boolean", "description": "Skip the teardrop on a pad or via a copper zone of its net already connects" },
            "max_length_mm": { "type": ["number", "null"], "description": "Longest teardrop, > 0; null for no limit" },
            "max_width_mm": { "type": ["number", "null"], "description": "Widest teardrop, > 0; null for no limit" },
            "best_length_ratio": { "type": "number", "description": "Preferred length as a ratio of the pad or via size, 0.2 to 2" },
            "best_width_ratio": { "type": "number", "description": "Preferred width as a ratio of the pad or via size, 0.6 to 1" },
            "max_track_width_ratio": { "type": "number", "description": "Largest track-width to pad-size ratio that still gets a teardrop, 0 to 1" }
        },
        "minProperties": 1
    })
}

pub(crate) fn tools() -> Vec<ToolDef> {
    vec![
        tool!(
            "get_teardrop_settings",
            "Read KiCad's native teardrop settings of the pads and vias on the board open in \
             KiCad 10.0.7 or newer. Optional filters: references (pads of those footprints), \
             nets, kinds (pad, via). Returns each item's UUID, reference, pad number, net, \
             position and settings, total and truncated when limit cuts the list, the board's \
             teardrop zones by net, and whether the board uses KiCad's legacy teardrops. Older or unknown KiCad versions are refused with \
             unsupported_capability.",
            json!({
                "type": "object",
                "properties": {
                    "board": { "type": "string", "description": "Path to the .kicad_pcb open in KiCad" },
                    "references": { "type": "array", "items": { "type": "string" }, "description": "Only pads of these footprints; vias are left out" },
                    "nets": { "type": "array", "items": { "type": "string" } },
                    "kinds": { "type": "array", "items": { "type": "string", "enum": ["pad", "via"] } },
                    "limit": { "type": "integer", "minimum": 0, "description": "Most items listed (default 200)" }
                },
                "required": ["board"]
            }),
            |args, ctx| async move { handle_get(args, ctx).await }
        )
        .with_board_access(BoardAccess::LiveOnly),
        tool!(
            "set_teardrop_settings",
            "Enable, adjust or disable KiCad's native teardrops on selected pads and vias of the \
             board open in KiCad 10.0.7 or newer, in one KiCad undo step, without saving. KiCad \
             rebuilds the teardrops of the edited items in that step. targets is the union of \
             pads ({reference, pad_number} or {uuid}), vias (UUIDs), references (their pads), \
             nets (pads and vias), all_pads and all_vias; a selector matching nothing refuses \
             the call. settings fields left out keep their value. refill_zones (default false) \
             then requests a board-wide refill and reports observed teardrop copper, not \
             confirmation that the queued refill completed. dry_run reports \
             each target's settings before and after and writes nothing. Refuses older or \
             unknown KiCad versions (unsupported_capability), boards in legacy teardrop mode \
             and pads whose footprint owns groups (plan_blocked), with nothing written. Pads \
             are written inside their whole footprint; a change to anything but teardrops is \
             mutation_outcome_uncertain.",
            json!({
                "type": "object",
                "properties": {
                    "board": { "type": "string", "description": "Path to the .kicad_pcb open in KiCad" },
                    "targets": {
                        "type": "object",
                        "description": "Pads and vias to change: the union of every selector given. A selector that matches nothing on the board refuses the whole call.",
                        "properties": {
                            "pads": {
                                "type": "array",
                                "items": {
                                    "type": "object",
                                    "properties": {
                                        "reference": { "type": "string" },
                                        "pad_number": { "type": "string" },
                                        "uuid": { "type": "string", "description": "A pad UUID from get_teardrop_settings; picks exactly one pad" }
                                    }
                                }
                            },
                            "vias": { "type": "array", "items": { "type": "string" }, "description": "Via UUIDs" },
                            "references": { "type": "array", "items": { "type": "string" }, "description": "Every pad of these footprints (pads only)" },
                            "nets": { "type": "array", "items": { "type": "string" }, "description": "Every pad and via on these nets (pads and vias)" },
                            "all_pads": { "type": "boolean" },
                            "all_vias": { "type": "boolean" }
                        }
                    },
                    "settings": settings_schema(),
                    "refill_zones": { "type": "boolean", "default": false, "description": "After the settings, request a board-wide refill" },
                    "dry_run": { "type": "boolean", "default": false }
                },
                "required": ["board", "targets", "settings"]
            }),
            |args, ctx| async move { handle_set(args, ctx).await }
        )
        .with_board_access(BoardAccess::LiveOnly),
    ]
}

// ─── Arguments ────────────────────────────────────────────────────────────────

struct Fault {
    field: String,
    reason: String,
}

fn fault(field: impl Into<String>, reason: impl Into<String>) -> Fault {
    Fault {
        field: field.into(),
        reason: reason.into(),
    }
}

/// Every fault at once. The first fills the structured `invalid_argument`.
fn refusal(tool: &str, faults: &[Fault]) -> CallToolResult {
    let first = &faults[0];
    let listed: Vec<String> = faults
        .iter()
        .take(20)
        .map(|f| format!("{}: {}", f.field, f.reason))
        .collect();
    CallToolResult::error_kind(
        ToolErrorKind::InvalidArgument {
            field: first.field.clone(),
            reason: first.reason.clone(),
        },
        format!(
            "{tool} refused the request: {}. Nothing was written.",
            listed.join("; ")
        ),
    )
}

fn optional_strings(args: &Value, field: &str, faults: &mut Vec<Fault>) -> Vec<String> {
    match args.get(field) {
        None | Some(Value::Null) => Vec::new(),
        Some(Value::Array(items)) => items
            .iter()
            .enumerate()
            .filter_map(|(index, item)| match item.as_str() {
                Some(text) if !text.is_empty() => Some(text.to_string()),
                _ => {
                    faults.push(fault(
                        format!("{field}[{index}]"),
                        "must be a non-empty string",
                    ));
                    None
                }
            })
            .collect(),
        Some(_) => {
            faults.push(fault(field, "must be an array of strings"));
            Vec::new()
        }
    }
}

fn optional_bool(args: &Value, field: &str, faults: &mut Vec<Fault>) -> bool {
    match args.get(field) {
        None | Some(Value::Null) => false,
        Some(Value::Bool(value)) => *value,
        Some(_) => {
            faults.push(fault(field, "must be a boolean"));
            false
        }
    }
}

/// A requested settings change: `None` keeps the target's value. A length
/// limit is `Some(None)` for "no limit".
#[derive(Debug, Clone, Default, PartialEq)]
struct SettingsChange {
    enabled: Option<bool>,
    curved_edges: Option<bool>,
    allow_multiple_track_segments: Option<bool>,
    prefer_zone_connection: Option<bool>,
    max_length_nm: Option<Option<i64>>,
    max_width_nm: Option<Option<i64>>,
    best_length_ratio: Option<f64>,
    best_width_ratio: Option<f64>,
    max_track_width_ratio: Option<f64>,
}

impl SettingsChange {
    fn apply_to(&self, current: &IpcTeardropSettings) -> IpcTeardropSettings {
        IpcTeardropSettings {
            enabled: self.enabled.unwrap_or(current.enabled),
            curved_edges: self.curved_edges.unwrap_or(current.curved_edges),
            allow_multiple_track_segments: self
                .allow_multiple_track_segments
                .unwrap_or(current.allow_multiple_track_segments),
            prefer_zone_connection: self
                .prefer_zone_connection
                .unwrap_or(current.prefer_zone_connection),
            max_length_nm: self.max_length_nm.unwrap_or(current.max_length_nm),
            max_width_nm: self.max_width_nm.unwrap_or(current.max_width_nm),
            best_length_ratio: self.best_length_ratio.unwrap_or(current.best_length_ratio),
            best_width_ratio: self.best_width_ratio.unwrap_or(current.best_width_ratio),
            max_track_width_ratio: self
                .max_track_width_ratio
                .unwrap_or(current.max_track_width_ratio),
        }
    }
}

fn parse_settings(value: &Value, faults: &mut Vec<Fault>) -> SettingsChange {
    let mut change = SettingsChange::default();
    let Some(object) = value.as_object() else {
        faults.push(fault("settings", "must be an object"));
        return change;
    };
    if object.is_empty() {
        faults.push(fault("settings", "must name at least one setting"));
    }
    let flag = |name: &str, faults: &mut Vec<Fault>| -> Option<bool> {
        match object.get(name) {
            None => None,
            Some(Value::Bool(value)) => Some(*value),
            Some(_) => {
                faults.push(fault(format!("settings.{name}"), "must be a boolean"));
                None
            }
        }
    };
    change.enabled = flag("enabled", faults);
    change.curved_edges = flag("curved_edges", faults);
    change.allow_multiple_track_segments = flag("allow_multiple_track_segments", faults);
    change.prefer_zone_connection = flag("prefer_zone_connection", faults);
    let limit = |name: &str, faults: &mut Vec<Fault>| -> Option<Option<i64>> {
        match object.get(name) {
            None => None,
            Some(Value::Null) => Some(None),
            Some(value) => match value.as_f64() {
                Some(mm) if mm.is_finite() && mm > 0.0 && mm <= MAX_LIMIT_MM => {
                    let nm = mm_to_nm(mm);
                    if nm > 0 {
                        Some(Some(nm))
                    } else {
                        faults.push(fault(
                            format!("settings.{name}"),
                            "rounds to 0 nm; use null for no limit",
                        ));
                        None
                    }
                }
                _ => {
                    faults.push(fault(
                        format!("settings.{name}"),
                        format!("must be a number above 0 and at most {MAX_LIMIT_MM} mm, or null for no limit"),
                    ));
                    None
                }
            },
        }
    };
    change.max_length_nm = limit("max_length_mm", faults);
    change.max_width_nm = limit("max_width_mm", faults);
    let ratio = |name: &str, (low, high): (f64, f64), faults: &mut Vec<Fault>| -> Option<f64> {
        match object.get(name) {
            None => None,
            Some(value) => match value.as_f64() {
                Some(ratio) if ratio.is_finite() && (low..=high).contains(&ratio) => Some(ratio),
                _ => {
                    faults.push(fault(
                        format!("settings.{name}"),
                        format!(
                            "must be a number from {low} to {high}, as KiCad's pad dialog allows"
                        ),
                    ));
                    None
                }
            },
        }
    };
    change.best_length_ratio = ratio("best_length_ratio", BEST_LENGTH_RATIO, faults);
    change.best_width_ratio = ratio("best_width_ratio", BEST_WIDTH_RATIO, faults);
    change.max_track_width_ratio = ratio("max_track_width_ratio", MAX_TRACK_WIDTH_RATIO, faults);
    for key in object.keys() {
        if !matches!(
            key.as_str(),
            "enabled"
                | "curved_edges"
                | "allow_multiple_track_segments"
                | "prefer_zone_connection"
                | "max_length_mm"
                | "max_width_mm"
                | "best_length_ratio"
                | "best_width_ratio"
                | "max_track_width_ratio"
        ) {
            faults.push(fault(
                format!("settings.{key}"),
                "is not a teardrop setting",
            ));
        }
    }
    change
}

#[derive(Debug, Clone, PartialEq)]
enum PadSelector {
    Uuid(String),
    Number {
        reference: String,
        pad_number: String,
    },
}

#[derive(Debug, Clone, Default, PartialEq)]
struct Selection {
    pads: Vec<PadSelector>,
    vias: Vec<String>,
    references: Vec<String>,
    nets: Vec<String>,
    all_pads: bool,
    all_vias: bool,
}

fn parse_targets(value: &Value, faults: &mut Vec<Fault>) -> Selection {
    let mut selection = Selection::default();
    let Some(object) = value.as_object() else {
        faults.push(fault("targets", "must be an object"));
        return selection;
    };
    let before = faults.len();
    match object.get("pads") {
        None | Some(Value::Null) => {}
        Some(Value::Array(items)) => {
            for (index, item) in items.iter().enumerate() {
                let field = format!("targets.pads[{index}]");
                let text = |name: &str| item.get(name).and_then(Value::as_str);
                match (text("uuid"), text("reference"), text("pad_number")) {
                    (Some(uuid), None, None) if !uuid.is_empty() => {
                        selection.pads.push(PadSelector::Uuid(uuid.to_string()))
                    }
                    (None, Some(reference), Some(pad_number))
                        if !reference.is_empty() && !pad_number.is_empty() =>
                    {
                        selection.pads.push(PadSelector::Number {
                            reference: reference.to_string(),
                            pad_number: pad_number.to_string(),
                        })
                    }
                    _ => faults.push(fault(
                        field,
                        "must be {reference, pad_number} or {uuid}, with non-empty strings",
                    )),
                }
                if let Some(extra) = item.as_object().and_then(|o| {
                    o.keys()
                        .find(|k| !matches!(k.as_str(), "uuid" | "reference" | "pad_number"))
                }) {
                    faults.push(fault(
                        format!("targets.pads[{index}].{extra}"),
                        "is not a pad selector field",
                    ));
                }
            }
        }
        Some(_) => faults.push(fault("targets.pads", "must be an array")),
    }
    selection.vias = optional_strings(value, "vias", faults);
    selection.references = optional_strings(value, "references", faults);
    selection.nets = optional_strings(value, "nets", faults);
    selection.all_pads = optional_bool(value, "all_pads", faults);
    selection.all_vias = optional_bool(value, "all_vias", faults);
    for key in object.keys() {
        if !matches!(
            key.as_str(),
            "pads" | "vias" | "references" | "nets" | "all_pads" | "all_vias"
        ) {
            faults.push(fault(format!("targets.{key}"), "is not a target selector"));
        }
    }
    // Faults are prefixed `targets.`; the per-field helpers above name the bare
    // field, so qualify what they added.
    for added in faults.iter_mut().skip(before) {
        if !added.field.starts_with("targets") {
            added.field = format!("targets.{}", added.field);
        }
    }
    let empty = selection.pads.is_empty()
        && selection.vias.is_empty()
        && selection.references.is_empty()
        && selection.nets.is_empty()
        && !selection.all_pads
        && !selection.all_vias;
    if empty && faults.len() == before {
        faults.push(fault("targets", "selects nothing; name at least one pad, via, reference or net, or set all_pads or all_vias"));
    }
    selection
}

/// The targets a selection names, in board order, or every selector that
/// named nothing (or named an ambiguous footprint).
fn resolve<'a>(
    selection: &Selection,
    targets: &'a [IpcTeardropTarget],
) -> Result<Vec<&'a IpcTeardropTarget>, Vec<Fault>> {
    let mut chosen: BTreeSet<&str> = BTreeSet::new();
    let mut faults = Vec::new();
    let pads = || {
        targets
            .iter()
            .filter(|t| t.kind == IpcTeardropTargetKind::Pad)
    };
    let footprints_named = |reference: &str| -> BTreeSet<&str> {
        pads()
            .filter(|t| t.reference.as_deref() == Some(reference))
            .filter_map(|t| t.footprint_uuid.as_deref())
            .collect()
    };
    for (index, selector) in selection.pads.iter().enumerate() {
        let field = format!("targets.pads[{index}]");
        match selector {
            PadSelector::Uuid(uuid) => match pads().find(|t| &t.uuid == uuid) {
                Some(t) => {
                    chosen.insert(&t.uuid);
                }
                None => faults.push(fault(field, format!("no pad has UUID {uuid}"))),
            },
            PadSelector::Number {
                reference,
                pad_number,
            } => {
                if footprints_named(reference).len() > 1 {
                    faults.push(fault(
                        field,
                        format!("several footprints are {reference}; select its pads by uuid"),
                    ));
                    continue;
                }
                let matched: Vec<_> = pads()
                    .filter(|t| {
                        t.reference.as_deref() == Some(reference.as_str())
                            && t.pad_number.as_deref() == Some(pad_number.as_str())
                    })
                    .collect();
                if matched.is_empty() {
                    faults.push(fault(field, format!("{reference} has no pad {pad_number}")));
                }
                chosen.extend(matched.iter().map(|t| t.uuid.as_str()));
            }
        }
    }
    for (index, uuid) in selection.vias.iter().enumerate() {
        match targets
            .iter()
            .find(|t| t.kind == IpcTeardropTargetKind::Via && &t.uuid == uuid)
        {
            Some(t) => {
                chosen.insert(&t.uuid);
            }
            None => faults.push(fault(
                format!("targets.vias[{index}]"),
                format!("no via has UUID {uuid}"),
            )),
        }
    }
    for (index, reference) in selection.references.iter().enumerate() {
        let matched: Vec<_> = pads()
            .filter(|t| t.reference.as_deref() == Some(reference.as_str()))
            .collect();
        if matched.is_empty() {
            faults.push(fault(
                format!("targets.references[{index}]"),
                format!("no footprint {reference} with pads"),
            ));
        }
        chosen.extend(matched.iter().map(|t| t.uuid.as_str()));
    }
    for (index, net) in selection.nets.iter().enumerate() {
        let matched: Vec<_> = targets.iter().filter(|t| &t.net == net).collect();
        if matched.is_empty() {
            faults.push(fault(
                format!("targets.nets[{index}]"),
                format!("no pad or via is on net {net}"),
            ));
        }
        chosen.extend(matched.iter().map(|t| t.uuid.as_str()));
    }
    if selection.all_pads {
        chosen.extend(pads().map(|t| t.uuid.as_str()));
    }
    if selection.all_vias {
        chosen.extend(
            targets
                .iter()
                .filter(|t| t.kind == IpcTeardropTargetKind::Via)
                .map(|t| t.uuid.as_str()),
        );
    }
    if chosen.is_empty() && faults.is_empty() {
        faults.push(fault("targets", "selects no pad or via on this board"));
    }
    if !faults.is_empty() {
        return Err(faults);
    }
    Ok(targets
        .iter()
        .filter(|t| chosen.contains(t.uuid.as_str()))
        .collect())
}

// ─── Rendering ────────────────────────────────────────────────────────────────

fn limit_json(nm: Option<i64>) -> Value {
    nm.map_or(Value::Null, |nm| json!(nm_to_mm(nm)))
}

fn settings_json(settings: &IpcTeardropSettings) -> Value {
    json!({
        "enabled": settings.enabled,
        "curved_edges": settings.curved_edges,
        "allow_multiple_track_segments": settings.allow_multiple_track_segments,
        "prefer_zone_connection": settings.prefer_zone_connection,
        "max_length_mm": limit_json(settings.max_length_nm),
        "max_width_mm": limit_json(settings.max_width_nm),
        "best_length_ratio": settings.best_length_ratio,
        "best_width_ratio": settings.best_width_ratio,
        "max_track_width_ratio": settings.max_track_width_ratio,
    })
}

fn kind_text(kind: IpcTeardropTargetKind) -> &'static str {
    match kind {
        IpcTeardropTargetKind::Pad => "pad",
        IpcTeardropTargetKind::Via => "via",
    }
}

fn target_json(target: &IpcTeardropTarget) -> Map<String, Value> {
    let mut out = Map::new();
    out.insert("kind".into(), json!(kind_text(target.kind)));
    out.insert("uuid".into(), json!(target.uuid));
    if target.kind == IpcTeardropTargetKind::Pad {
        out.insert("reference".into(), json!(target.reference));
        out.insert("pad_number".into(), json!(target.pad_number));
        out.insert("footprint_uuid".into(), json!(target.footprint_uuid));
    }
    out.insert("net".into(), json!(target.net));
    out.insert(
        "position_mm".into(),
        json!({ "x": target.position.x, "y": target.position.y }),
    );
    out
}

fn zones_by_net(zones: &[IpcTeardropZone]) -> BTreeMap<String, usize> {
    let mut counts = BTreeMap::new();
    for zone in zones {
        *counts.entry(zone.net.clone()).or_insert(0) += 1;
    }
    counts
}

fn zone_json(zone: &IpcTeardropZone) -> Value {
    json!({ "uuid": zone.uuid, "net": zone.net, "layers": zone.layers, "type": zone.teardrop_type })
}

/// What changed in the board's teardrop zones between two reads. KiCad's
/// teardrop zone KIIDs are deterministic, so a rebuilt teardrop keeps its
/// KIID; `refilled` names those whose fill changed.
fn zone_changes(before: &[IpcTeardropZone], after: &[IpcTeardropZone]) -> Value {
    let find = |zones: &[IpcTeardropZone], uuid: &str| -> Option<IpcTeardropZone> {
        zones.iter().find(|z| z.uuid == uuid).cloned()
    };
    let added: Vec<Value> = after
        .iter()
        .filter(|z| find(before, &z.uuid).is_none())
        .map(zone_json)
        .collect();
    let removed: Vec<Value> = before
        .iter()
        .filter(|z| find(after, &z.uuid).is_none())
        .map(zone_json)
        .collect();
    let refilled: Vec<&str> = after
        .iter()
        .filter(|z| {
            find(before, &z.uuid).is_some_and(|old| {
                old.fill_digest != z.fill_digest || old.outline_digest != z.outline_digest
            })
        })
        .map(|z| z.uuid.as_str())
        .collect();
    json!({
        "teardrop_zones_before": before.len(),
        "teardrop_zones_after": after.len(),
        "added_count": added.len(),
        "removed_count": removed.len(),
        "rebuilt_count": refilled.len(),
        "added": added.into_iter().take(LISTED).collect::<Vec<_>>(),
        "removed": removed.into_iter().take(LISTED).collect::<Vec<_>>(),
        "rebuilt": refilled.into_iter().take(LISTED).collect::<Vec<_>>(),
        "by_net_after": zones_by_net(after),
    })
}

// ─── Version gate ─────────────────────────────────────────────────────────────

/// The bound editor's version if it carries the settings, else the refusal.
fn version_gate(client: &KiCadIpcClient, tool: &str) -> Result<IpcKiCadVersion, CallToolResult> {
    let version = client.get_kicad_version();
    match version {
        Ok(version) if supports_teardrop_settings(&version) => Ok(version),
        Ok(version) => Err(CallToolResult::error_kind(
            ToolErrorKind::UnsupportedCapability {
                capability: tool.to_string(),
                kicad_version: Some(version.full_version.clone()),
            },
            format!(
                "KiCad {} does not carry pad and via teardrop settings over IPC; {tool} needs \
                 KiCad {} or newer. Nothing was read or written.",
                if version.full_version.is_empty() {
                    "of unknown version"
                } else {
                    &version.full_version
                },
                min_version()
            ),
        )),
        Err(error) => Err(CallToolResult::error_kind(
            ToolErrorKind::UnsupportedCapability {
                capability: tool.to_string(),
                kicad_version: None,
            },
            format!(
                "KiCad did not report its version ({error:#}), so whether it carries teardrop \
                 settings is unknown; {tool} needs KiCad {} or newer. Nothing was read or \
                 written.",
                min_version()
            ),
        )),
    }
}

fn bound<T>(
    outcome: Result<BoardBinding<T>, copperpour_ipc::IpcFailure>,
) -> Result<T, CallToolResult> {
    match outcome {
        Ok(BoardBinding::Bound(value)) => Ok(value),
        Ok(BoardBinding::Unserved { absence, message }) => {
            Err(crate::tools::ipc_unserved_result(absence, &message))
        }
        Err(failure) => Err(ipc_failure_result(failure)),
    }
}

// ─── get_teardrop_settings ────────────────────────────────────────────────────

async fn handle_get(args: &Value, ctx: &ToolContext) -> anyhow::Result<CallToolResult> {
    let board = get_path(args, "board")?;
    let mut faults = Vec::new();
    let references = optional_strings(args, "references", &mut faults);
    let nets = optional_strings(args, "nets", &mut faults);
    let kinds = optional_strings(args, "kinds", &mut faults);
    for (index, kind) in kinds.iter().enumerate() {
        if kind != "pad" && kind != "via" {
            faults.push(fault(format!("kinds[{index}]"), "must be pad or via"));
        }
    }
    if !faults.is_empty() {
        return Ok(refusal("get_teardrop_settings", &faults));
    }
    let limit = match response_limit(args, 200) {
        Ok(limit) => limit,
        Err(error) => return Ok(error),
    };
    let board_text = board.display().to_string();
    let outcome = with_bound_board_ipc_classified(ctx, &board, move |client, document| {
        read_live(
            client,
            document,
            &board_text,
            &references,
            &nets,
            &kinds,
            limit,
        )
    })
    .await?;
    Ok(bound(outcome).unwrap_or_else(|error| error))
}

fn read_live(
    client: &KiCadIpcClient,
    document: DocumentSpecifier,
    board: &str,
    references: &[String],
    nets: &[String],
    kinds: &[String],
    limit: usize,
) -> anyhow::Result<CallToolResult> {
    let version = match version_gate(client, "get_teardrop_settings") {
        Ok(version) => version,
        Err(refused) => return Ok(refused),
    };
    let targets = client.get_teardrop_targets_in(document.clone())?;
    let zones = client.get_teardrop_zones_in(document.clone())?;
    let legacy = client.board_uses_legacy_teardrops_in(document)?;
    let wanted: Vec<&IpcTeardropTarget> = targets
        .iter()
        .filter(|t| kinds.is_empty() || kinds.iter().any(|k| k == kind_text(t.kind)))
        .filter(|t| {
            references.is_empty() || t.reference.as_ref().is_some_and(|r| references.contains(r))
        })
        .filter(|t| nets.is_empty() || nets.contains(&t.net))
        .collect();
    let listed = BoundedList::new(&wanted, limit);
    let items: Vec<Value> = listed
        .items
        .iter()
        .map(|t| {
            let mut item = target_json(t);
            item.insert("settings".into(), settings_json(&t.settings));
            Value::Object(item)
        })
        .collect();
    Ok(CallToolResult::json(&json!({
        "board": board,
        "source": "ipc",
        "kicad_version": version.full_version,
        "legacy_teardrops": legacy,
        "total": listed.count,
        "shown": listed.shown,
        "truncated": listed.truncated,
        "enabled_count": wanted.iter().filter(|t| t.settings.enabled).count(),
        "targets": items,
        "teardrop_zones": { "count": zones.len(), "by_net": zones_by_net(&zones) },
    })))
}

// ─── set_teardrop_settings ────────────────────────────────────────────────────

struct SetRequest {
    selection: Selection,
    change: SettingsChange,
    refill: bool,
    dry_run: bool,
}

fn parse_set(args: &Value) -> Result<SetRequest, Vec<Fault>> {
    let mut faults = Vec::new();
    let selection = parse_targets(&args["targets"], &mut faults);
    let change = parse_settings(&args["settings"], &mut faults);
    let refill = optional_bool(args, "refill_zones", &mut faults);
    let dry_run = optional_bool(args, "dry_run", &mut faults);
    if faults.is_empty() {
        Ok(SetRequest {
            selection,
            change,
            refill,
            dry_run,
        })
    } else {
        Err(faults)
    }
}

async fn handle_set(args: &Value, ctx: &ToolContext) -> anyhow::Result<CallToolResult> {
    let board = get_path(args, "board")?;
    let request = match parse_set(args) {
        Ok(request) => request,
        Err(faults) => return Ok(refusal("set_teardrop_settings", &faults)),
    };
    let board_path = board.clone();
    let outcome = with_bound_board_ipc_classified(ctx, &board, move |client, document| {
        set_live(client, document, &board_path, &request)
    })
    .await?;
    Ok(bound(outcome).unwrap_or_else(|error| error))
}

fn blocked(reasons: Vec<String>) -> CallToolResult {
    CallToolResult::error_kind(
        ToolErrorKind::PlanBlocked {
            operation: "set_teardrop_settings".to_string(),
            reasons: reasons.clone(),
        },
        format!(
            "set_teardrop_settings refused: {}. Nothing was written.",
            reasons.join("; ")
        ),
    )
}

fn legacy_reason() -> String {
    "the board uses KiCad's legacy teardrops, which no rebuild creates from these settings; \
     convert it in KiCad first"
        .to_string()
}

fn grouped_reason(footprint: &str, reference: &str, count: usize) -> String {
    format!(
        "footprint {reference} ({footprint}) owns {count} group(s), which the whole-footprint \
         update a pad change needs would drop; change its pads in KiCad, or ungroup them first"
    )
}

fn apply_refusal(board: &Path, refusal: IpcTeardropRefusal) -> CallToolResult {
    match refusal {
        IpcTeardropRefusal::LegacyTeardrops => blocked(vec![legacy_reason()]),
        IpcTeardropRefusal::FootprintOwnsGroups { footprints } => blocked(
            footprints
                .iter()
                .map(|(id, reference, count)| grouped_reason(id, reference, *count))
                .collect(),
        ),
        IpcTeardropRefusal::Stale { reason } => CallToolResult::error_kind(
            ToolErrorKind::StaleTarget {
                target: board.display().to_string(),
                reason: reason.clone(),
            },
            format!(
                "The board changed while set_teardrop_settings ran: {reason}. Nothing \
                 was written; read the board again and retry."
            ),
        ),
        IpcTeardropRefusal::Rejected { reason } => CallToolResult::error_kind(
            ToolErrorKind::HandlerError {
                reason: reason.clone(),
            },
            format!(
                "KiCad refused the teardrop settings ({reason}); a fresh read showed \
                 every item unchanged. Nothing was written."
            ),
        ),
    }
}

fn uncertain(board: &Path, reason: &str, applied: Option<Value>) -> CallToolResult {
    let body = json!({
        "message": format!(
            "set_teardrop_settings could not confirm what the board KiCad holds now contains: \
             {reason}. Do not repeat the call: read the board with get_teardrop_settings and \
             inspect it in KiCad first."
        ),
        "error": ToolErrorKind::MutationOutcomeUncertain {
            operation: "set_teardrop_settings".to_string(),
            path: board.display().to_string(),
            reason: reason.to_string(),
        },
        "applied": applied,
    });
    CallToolResult {
        content: vec![ToolContent::Text {
            text: body.to_string(),
        }],
        is_error: true,
    }
}

fn set_live(
    client: &KiCadIpcClient,
    document: DocumentSpecifier,
    board: &Path,
    request: &SetRequest,
) -> anyhow::Result<CallToolResult> {
    let version = match version_gate(client, "set_teardrop_settings") {
        Ok(version) => version,
        Err(refused) => return Ok(refused),
    };
    let targets = client.get_teardrop_targets_in(document.clone())?;
    let selected = match resolve(&request.selection, &targets) {
        Ok(selected) => selected,
        Err(faults) => return Ok(refusal("set_teardrop_settings", &faults)),
    };
    let planned: Vec<(&IpcTeardropTarget, IpcTeardropSettings)> = selected
        .iter()
        .map(|t| (*t, request.change.apply_to(&t.settings)))
        .collect();
    let edits: Vec<IpcTeardropEdit> = planned
        .iter()
        .filter(|(t, after)| t.settings != *after)
        .map(|(t, after)| IpcTeardropEdit {
            kind: t.kind,
            uuid: t.uuid.clone(),
            footprint_uuid: t.footprint_uuid.clone(),
            current: t.settings.clone(),
            settings: after.clone(),
        })
        .collect();

    if request.dry_run {
        if let Some(refusal) = client.teardrop_preflight_in(document.clone(), &edits)? {
            return Ok(apply_refusal(board, refusal));
        }
    }

    let listed: Vec<Value> = planned
        .iter()
        .take(LISTED)
        .map(|(t, after)| {
            let mut item = target_json(t);
            item.insert("before".into(), settings_json(&t.settings));
            item.insert("after".into(), settings_json(after));
            item.insert("changed".into(), json!(t.settings != *after));
            Value::Object(item)
        })
        .collect();
    let mut response = json!({
        "board": board.display().to_string(),
        "source": "ipc",
        "kicad_version": version.full_version,
        "dry_run": request.dry_run,
        "selected": planned.len(),
        "changed": edits.len(),
        "unchanged": planned.len() - edits.len(),
        "shown": listed.len(),
        "truncated": listed.len() < planned.len(),
        "targets": listed,
        "refill_zones": request.refill,
        "saved": false,
    });
    if request.dry_run {
        response["copper"] = json!({ "commit": null, "refill": null });
        return Ok(CallToolResult::json(&response));
    }

    // Even an unchanged request must pass preflight before it can refill.
    let applied = match client.set_teardrop_settings_in(document.clone(), &edits)? {
        IpcTeardropApplyOutcome::Applied(applied) => applied,
        IpcTeardropApplyOutcome::Refused(refusal) => return Ok(apply_refusal(board, refusal)),
        IpcTeardropApplyOutcome::Uncertain { reason } => {
            return Ok(uncertain(board, &reason, None))
        }
    };
    if edits.is_empty() {
        response["copper"] = json!({ "commit": null });
    } else {
        let readback: BTreeMap<&str, &IpcTeardropSettings> = applied
            .readback
            .iter()
            .map(|t| (t.uuid.as_str(), &t.settings))
            .collect();
        for item in response["targets"].as_array_mut().into_iter().flatten() {
            if let Some(held) = item["uuid"].as_str().and_then(|u| readback.get(u)) {
                item["after"] = settings_json(held);
            }
        }
        response["settings_source"] = json!("ipc_readback");
        response["verified_changes"] = json!({
            "count": applied.changed_fields.len(),
            "fields": applied.changed_fields.iter().take(LISTED).collect::<Vec<_>>(),
            "carried_pads_unchanged": applied.carried_pads,
        });
        response["undo"] = json!(UNDO);
        response["copper"] =
            json!({ "commit": zone_changes(&applied.zones_before, &applied.zones_after) });
    }
    response["copper"]["refill"] = Value::Null;
    if !request.refill {
        return Ok(CallToolResult::json(&response));
    }
    let observation = super::drc::request_refill_and_wait(client, document.clone())
        .and_then(|()| client.get_teardrop_zones_in(document));
    match observation {
        Ok(zones) => {
            response["copper"]["refill"] = zone_changes(&applied.zones_after, &zones);
            response["copper"]["refill"]["completion_verified"] = json!(false);
            Ok(CallToolResult::json(&response))
        }
        Err(error) => Ok(uncertain(
            board,
            &format!(
                "{}, but the board-wide refill or its copper read could not be confirmed: {error:#}",
                if edits.is_empty() {
                    "no settings needed changing"
                } else {
                    "the settings were applied and read back"
                }
            ),
            Some(response),
        )),
    }
}

#[cfg(test)]
mod tests {
    //! Against a KiCad double holding the KiCad-authored fixture of
    //! `copperpour-ipc/tests/fixtures/teardrops_kicad10.*`: KiCad 10.0.8's own
    //! `GetItems` answer for its pads and vias, and its saved board as the
    //! `SaveDocumentToString` answer. The double records every command, so a
    //! refusal can be shown to have sent nothing that writes.

    use super::*;
    use crate::tools::pcb_board::board_mock::{ctx_talking_to, spawn_kicad_holding_board};
    use copperpour_ipc::builders::{any_is, pack_any};
    use copperpour_ipc::gen::kiapi;
    use kiapi::board::types as board;
    use prost::Message;
    use std::sync::{Arc, Mutex};

    const CAPTURE: &[u8] =
        include_bytes!("../../../copperpour-ipc/tests/fixtures/teardrops_kicad10.ipc.bin");
    const BOARD: &str =
        include_str!("../../../copperpour-ipc/tests/fixtures/teardrops_kicad10.kicad_pcb");
    /// R1 pad 1, on VCC, at KiCad's defaults in the fixture.
    const R1_PAD_1: &str = "fb481b08-0972-41e5-a5d9-a4bab9f165c1";
    /// R1 pad 2, on GND, enabled in the fixture.
    const R1_PAD_2: &str = "80cbeaee-444b-4a13-95c2-2c277db1ac17";
    const R1: &str = "11111111-1111-4111-8111-111111111111";
    const VIA: &str = "db24f42e-63bc-4e53-a1a7-509928ef3cd6";
    const WRITES: [&str; 6] = [
        "BeginCommit",
        "EndCommit",
        "UpdateItems",
        "CreateItems",
        "DeleteItems",
        "RefillZones",
    ];

    struct Kicad {
        /// Applied to each footprint KiCad stores from an `UpdateItems`.
        tamper: Option<fn(&mut board::FootprintInstance)>,
        /// Answer `UpdateItems` with `ISC_OK` but keep nothing, as KiCad does
        /// with a pad update aimed at its stale cache.
        drop_updates: bool,
        /// Refuse every item of an `UpdateItems`, keeping nothing.
        reject_updates: bool,
        /// Run once, just before the second answer listing pads and vias:
        /// a change made in KiCad between the tool's read and its write.
        on_second_pad_read: Option<fn(&mut Kicad)>,
        pad_reads: usize,
        on_second_footprint_read: Option<fn(&mut Kicad)>,
        footprint_reads: usize,
        fail_refill_read: bool,
        version: Option<(u32, u32, u32)>,
        text: String,
        footprints: Vec<board::FootprintInstance>,
        vias: Vec<board::Via>,
        log: Vec<String>,
    }

    impl Kicad {
        fn pads(&self) -> Vec<board::Pad> {
            self.footprints
                .iter()
                .flat_map(|fp| fp.definition.iter().flat_map(|d| d.items.iter()))
                .filter(|item| any_is(item, "kiapi.board.types.Pad"))
                .map(|item| board::Pad::decode(item.value.as_slice()).unwrap())
                .collect()
        }

        /// One teardrop zone per enabled pad and via: enough for the double to
        /// show copper following the settings.
        fn zones(&self) -> Vec<prost_types::Any> {
            let enabled = |t: &Option<board::PadTeardropSettings>| {
                t.as_ref()
                    .is_some_and(|t| t.mode == board::PadTeardropMode::PtmEnabled as i32)
            };
            let zone = |id: &str, net: &Option<board::Net>| {
                pack_any(
                    &board::Zone {
                        id: Some(kiapi::common::types::Kiid {
                            value: format!("td-{id}"),
                        }),
                        r#type: board::ZoneType::ZtTeardrop as i32,
                        settings: Some(board::zone::Settings::CopperSettings(
                            board::CopperZoneSettings {
                                net: net.clone(),
                                teardrop: Some(board::ZoneTeardropSettings {
                                    r#type: board::ZoneTeardropType::ZtdtViaPad as i32,
                                }),
                                ..Default::default()
                            },
                        )),
                        filled: true,
                        ..Default::default()
                    },
                    "kiapi.board.types.Zone",
                )
            };
            let mut zones: Vec<_> = self
                .pads()
                .iter()
                .filter(|p| enabled(&p.teardrop))
                .map(|p| zone(&p.id.as_ref().unwrap().value, &p.net))
                .collect();
            zones.extend(
                self.vias
                    .iter()
                    .filter(|v| enabled(&v.teardrop))
                    .map(|v| zone(&v.id.as_ref().unwrap().value, &v.net)),
            );
            zones
        }

        fn writes(&self) -> Vec<String> {
            self.log
                .iter()
                .filter(|c| WRITES.contains(&c.as_str()))
                .cloned()
                .collect()
        }
    }

    fn fixture(version: Option<(u32, u32, u32)>) -> Kicad {
        let captured = kiapi::common::commands::GetItemsResponse::decode(CAPTURE).unwrap();
        let tree = copperpour_sexp::parse_sexp(BOARD).unwrap();
        let mut footprints = Vec::new();
        for node in tree.find_all("footprint") {
            let id = node.find_str("uuid").unwrap().to_string();
            let reference = node
                .find_all("property")
                .into_iter()
                .find(|p| p.get(1).and_then(|n| n.as_str()) == Some("Reference"))
                .and_then(|p| p.get(2).and_then(|n| n.as_str()))
                .unwrap()
                .to_string();
            let items = captured
                .items
                .iter()
                .filter(|item| any_is(item, "kiapi.board.types.Pad"))
                .filter(|item| {
                    board::Pad::decode(item.value.as_slice())
                        .unwrap()
                        .parent
                        .is_some_and(|p| p.value == id)
                })
                .cloned()
                .collect();
            footprints.push(board::FootprintInstance {
                id: Some(kiapi::common::types::Kiid { value: id }),
                reference_field: Some(board::Field {
                    name: "Reference".into(),
                    text: Some(board::BoardText {
                        text: Some(kiapi::common::types::Text {
                            text: reference,
                            ..Default::default()
                        }),
                        ..Default::default()
                    }),
                    ..Default::default()
                }),
                definition: Some(board::Footprint {
                    items,
                    ..Default::default()
                }),
                ..Default::default()
            });
        }
        let vias = captured
            .items
            .iter()
            .filter(|item| any_is(item, "kiapi.board.types.Via"))
            .map(|item| board::Via::decode(item.value.as_slice()).unwrap())
            .collect();
        Kicad {
            tamper: None,
            drop_updates: false,
            reject_updates: false,
            on_second_pad_read: None,
            pad_reads: 0,
            on_second_footprint_read: None,
            footprint_reads: 0,
            fail_refill_read: false,
            version,
            text: BOARD.to_string(),
            footprints,
            vias,
            log: Vec::new(),
        }
    }

    fn items_response(items: Vec<prost_types::Any>) -> prost_types::Any {
        pack_any(
            &kiapi::common::commands::GetItemsResponse {
                header: None,
                status: kiapi::common::types::ItemRequestStatus::IrsOk as i32,
                items,
            },
            "kiapi.common.commands.GetItemsResponse",
        )
    }

    fn answer(held: &mut Kicad, command: &prost_types::Any) -> Option<prost_types::Any> {
        use kiapi::common::commands;
        use kiapi::common::types::KiCadObjectType as Kot;
        let name = command.type_url.rsplit('.').next().unwrap().to_string();
        held.log.push(name.clone());
        match name.as_str() {
            "GetVersion" => held.version.map(|(major, minor, patch)| {
                pack_any(
                    &commands::GetVersionResponse {
                        version: Some(kiapi::common::types::KiCadVersion {
                            major,
                            minor,
                            patch,
                            full_version: format!("{major}.{minor}.{patch}"),
                        }),
                    },
                    "kiapi.common.commands.GetVersionResponse",
                )
            }),
            "SaveDocumentToString" => Some(pack_any(
                &commands::SavedDocumentResponse {
                    contents: held.text.clone(),
                    document: None,
                },
                "kiapi.common.commands.SavedDocumentResponse",
            )),
            "GetItems" => {
                let request = commands::GetItems::decode(command.value.as_slice()).unwrap();
                let wants = |kind: Kot| request.types.contains(&(kind as i32));
                let mut items = Vec::new();
                if wants(Kot::KotPcbPad) {
                    held.pad_reads += 1;
                    if held.pad_reads == 2 {
                        if let Some(change) = held.on_second_pad_read.take() {
                            change(held);
                        }
                    }
                    items.extend(
                        held.pads()
                            .iter()
                            .map(|p| pack_any(p, "kiapi.board.types.Pad")),
                    );
                }
                if wants(Kot::KotPcbVia) {
                    items.extend(
                        held.vias
                            .iter()
                            .map(|v| pack_any(v, "kiapi.board.types.Via")),
                    );
                }
                if wants(Kot::KotPcbFootprint) {
                    held.footprint_reads += 1;
                    if held.footprint_reads == 2 {
                        if let Some(change) = held.on_second_footprint_read.take() {
                            change(held);
                        }
                    }
                    items.extend(
                        held.footprints
                            .iter()
                            .map(|f| pack_any(f, "kiapi.board.types.FootprintInstance")),
                    );
                }
                if wants(Kot::KotPcbZone) {
                    if held.fail_refill_read && held.log.iter().any(|name| name == "RefillZones") {
                        return Some(pack_any(
                            &commands::GetItemsResponse {
                                status: kiapi::common::types::ItemRequestStatus::IrsDocumentNotFound
                                    as i32,
                                ..Default::default()
                            },
                            "kiapi.common.commands.GetItemsResponse",
                        ));
                    }
                    items.extend(held.zones());
                }
                Some(items_response(items))
            }
            "BeginCommit" => Some(pack_any(
                &commands::BeginCommitResponse {
                    id: Some(kiapi::common::types::Kiid {
                        value: "commit".into(),
                    }),
                },
                "kiapi.common.commands.BeginCommitResponse",
            )),
            "EndCommit" => Some(pack_any(
                &commands::EndCommitResponse {},
                "kiapi.common.commands.EndCommitResponse",
            )),
            "UpdateItems" => {
                let request = commands::UpdateItems::decode(command.value.as_slice()).unwrap();
                let mut updated = Vec::new();
                if held.reject_updates {
                    let refused = request
                        .items
                        .iter()
                        .map(|_| commands::ItemUpdateResult {
                            status: Some(commands::ItemStatus {
                                code: commands::ItemStatusCode::IscInvalidData as i32,
                                error_message: "refused by the test double".into(),
                            }),
                            item: None,
                        })
                        .collect();
                    return Some(pack_any(
                        &commands::UpdateItemsResponse {
                            header: None,
                            status: kiapi::common::types::ItemRequestStatus::IrsOk as i32,
                            updated_items: refused,
                        },
                        "kiapi.common.commands.UpdateItemsResponse",
                    ));
                }
                for item in &request.items {
                    if any_is(item, "kiapi.board.types.Via") {
                        let via = board::Via::decode(item.value.as_slice()).unwrap();
                        let slot = held.vias.iter_mut().find(|v| v.id == via.id).unwrap();
                        *slot = via;
                    } else if any_is(item, "kiapi.board.types.FootprintInstance") {
                        let mut fp =
                            board::FootprintInstance::decode(item.value.as_slice()).unwrap();
                        if let Some(tamper) = held.tamper {
                            tamper(&mut fp);
                        }
                        let drop_updates = held.drop_updates;
                        let slot = held.footprints.iter_mut().find(|f| f.id == fp.id).unwrap();
                        if !drop_updates {
                            *slot = fp;
                        }
                    } else {
                        panic!("unexpected update of {}", item.type_url);
                    }
                    updated.push(commands::ItemUpdateResult {
                        status: Some(commands::ItemStatus {
                            code: commands::ItemStatusCode::IscOk as i32,
                            error_message: String::new(),
                        }),
                        item: Some(item.clone()),
                    });
                }
                Some(pack_any(
                    &commands::UpdateItemsResponse {
                        header: None,
                        status: kiapi::common::types::ItemRequestStatus::IrsOk as i32,
                        updated_items: updated,
                    },
                    "kiapi.common.commands.UpdateItemsResponse",
                ))
            }
            _ => None,
        }
    }

    struct Scene {
        _dir: tempfile::TempDir,
        board: std::path::PathBuf,
        held: Arc<Mutex<Kicad>>,
        server: crate::test_support::MockIpcServer,
    }

    fn scene(kicad: Kicad) -> Scene {
        let dir = tempfile::tempdir().unwrap();
        let board = dir.path().join("teardrops_kicad10.kicad_pcb");
        std::fs::write(&board, BOARD).unwrap();
        let held = Arc::new(Mutex::new(kicad));
        let state = held.clone();
        let server = spawn_kicad_holding_board(&board, move |command| {
            answer(&mut state.lock().unwrap(), command)
        });
        Scene {
            _dir: dir,
            board,
            held,
            server,
        }
    }

    impl Scene {
        async fn get(&self, extra: Value) -> (bool, Value) {
            let mut args = json!({ "board": self.board });
            for (k, v) in extra.as_object().unwrap() {
                args[k] = v.clone();
            }
            let ctx = ctx_talking_to(self.server.address().to_string());
            body(handle_get(&args, &ctx).await.unwrap())
        }

        async fn set(&self, extra: Value) -> (bool, Value) {
            let mut args = json!({ "board": self.board });
            for (k, v) in extra.as_object().unwrap() {
                args[k] = v.clone();
            }
            let ctx = ctx_talking_to(self.server.address().to_string());
            body(handle_set(&args, &ctx).await.unwrap())
        }

        fn writes(&self) -> Vec<String> {
            self.held.lock().unwrap().writes()
        }

        fn pad(&self, uuid: &str) -> board::Pad {
            self.held
                .lock()
                .unwrap()
                .pads()
                .into_iter()
                .find(|p| p.id.as_ref().unwrap().value == uuid)
                .unwrap()
        }
    }

    fn body(result: CallToolResult) -> (bool, Value) {
        let ToolContent::Text { text } = &result.content[0] else {
            panic!("not text");
        };
        (
            result.is_error,
            serde_json::from_str(text).unwrap_or_else(|_| json!({ "message": text })),
        )
    }

    fn enable_r1_pad_1() -> Value {
        json!({
            "targets": { "pads": [{ "uuid": R1_PAD_1 }] },
            "settings": { "enabled": true, "best_length_ratio": 0.7 }
        })
    }

    #[tokio::test]
    async fn a_kicad_before_10_0_7_is_refused_before_any_read_or_write() {
        let scene = scene(fixture(Some((10, 0, 6))));
        let (error, body) = scene.set(enable_r1_pad_1()).await;
        assert!(error, "{body}");
        assert_eq!(body["error"]["kind"], "unsupported_capability");
        assert_eq!(body["error"]["kicad_version"], "10.0.6");
        let (error, body) = scene.get(json!({})).await;
        assert!(error);
        assert_eq!(body["error"]["kind"], "unsupported_capability");
        let log = scene.held.lock().unwrap().log.clone();
        assert!(
            !log.iter()
                .any(|c| c == "GetItems" || WRITES.contains(&c.as_str())),
            "{log:?}"
        );
    }

    #[tokio::test]
    async fn an_unknown_kicad_version_is_refused_before_any_write() {
        for version in [None, Some((0, 0, 0))] {
            let scene = scene(fixture(version));
            let (error, body) = scene
                .set(json!({
                    "targets": { "all_vias": true },
                    "settings": { "enabled": false },
                    "refill_zones": true
                }))
                .await;
            assert!(error, "{body}");
            assert_eq!(body["error"]["kind"], "unsupported_capability", "{body}");
            assert!(scene.writes().is_empty(), "{:?}", scene.writes());
        }
    }

    #[tokio::test]
    async fn settings_read_back_as_the_board_file_declares() {
        let scene = scene(fixture(Some((10, 0, 7))));
        let (error, body) = scene.get(json!({})).await;
        assert!(!error, "{body}");
        assert_eq!(body["total"], 7);
        assert_eq!(body["enabled_count"], 4);
        assert_eq!(body["legacy_teardrops"], false);
        let via = body["targets"]
            .as_array()
            .unwrap()
            .iter()
            .find(|t| t["uuid"] == VIA)
            .unwrap();
        assert_eq!(via["settings"]["max_length_mm"], 1.5);
        assert_eq!(via["settings"]["max_width_mm"], Value::Null);
        assert_eq!(via["settings"]["best_length_ratio"], 0.6);

        let (_, r1) = scene.get(json!({ "references": ["R1"] })).await;
        assert_eq!(r1["total"], 2, "{r1}");
        let (_, vias) = scene.get(json!({ "kinds": ["via"], "limit": 0 })).await;
        assert_eq!(vias["total"], 1);
        assert_eq!(vias["shown"], 0);
        assert_eq!(vias["truncated"], true);
        assert!(scene.writes().is_empty());
    }

    #[tokio::test]
    async fn a_dry_run_reports_before_and_after_and_writes_nothing() {
        let scene = scene(fixture(Some((10, 0, 8))));
        let mut args = enable_r1_pad_1();
        args["dry_run"] = json!(true);
        let (error, body) = scene.set(args).await;
        assert!(!error, "{body}");
        assert_eq!(body["selected"], 1);
        assert_eq!(body["changed"], 1);
        let target = &body["targets"][0];
        assert_eq!(target["before"]["enabled"], false);
        assert_eq!(target["after"]["enabled"], true);
        assert_eq!(target["after"]["best_length_ratio"], 0.7);
        assert_eq!(target["after"]["max_length_mm"], 1.0, "kept from the pad");
        assert!(scene.writes().is_empty(), "{:?}", scene.writes());
    }

    /// One pad by UUID: only that pad's settings change, inside its whole
    /// footprint, in one commit; the response is KiCad's read-back and the
    /// teardrop copper it built. No refill unless asked.
    #[tokio::test]
    async fn one_pad_is_changed_through_its_footprint_and_read_back() {
        let scene = scene(fixture(Some((10, 0, 8))));
        let neighbour = scene.pad(R1_PAD_2).teardrop;
        let (error, body) = scene.set(enable_r1_pad_1()).await;
        assert!(!error, "{body}");
        assert_eq!(body["changed"], 1);
        assert_eq!(body["settings_source"], "ipc_readback");
        assert_eq!(body["targets"][0]["after"]["enabled"], true);
        assert_eq!(
            scene.writes(),
            ["BeginCommit", "UpdateItems", "EndCommit"],
            "one commit, no refill"
        );
        let held = scene.pad(R1_PAD_1).teardrop.unwrap();
        assert_eq!(held.mode, board::PadTeardropMode::PtmEnabled as i32);
        assert_eq!(held.best_length_ratio, 0.7);
        assert_eq!(
            scene.pad(R1_PAD_2).teardrop,
            neighbour,
            "R1 pad 2 untouched"
        );
        assert_eq!(
            body["verified_changes"]["fields"],
            json!([format!("footprint {R1}: pad {R1_PAD_1} teardrop")]),
            "{body}"
        );
        assert_eq!(body["verified_changes"]["carried_pads_unchanged"], 1);
        let commit = &body["copper"]["commit"];
        assert_eq!(commit["added_count"], 1, "{commit}");
        assert_eq!(commit["added"][0]["uuid"], format!("td-{R1_PAD_1}"));
        assert_eq!(body["copper"]["refill"], Value::Null);
    }

    /// The whole footprint goes back to KiCad, so the read-back must show
    /// nothing but the teardrop changed: a KiCad that moves another pad on
    /// the way makes the outcome uncertain, naming the field.
    #[tokio::test]
    async fn a_footprint_that_comes_back_changed_elsewhere_is_uncertain() {
        let mut kicad = fixture(Some((10, 0, 8)));
        kicad.tamper = Some(|footprint| {
            let definition = footprint.definition.as_mut().unwrap();
            for item in definition.items.iter_mut() {
                if any_is(item, "kiapi.board.types.Pad") {
                    let mut pad = board::Pad::decode(item.value.as_slice()).unwrap();
                    if pad.id.as_ref().unwrap().value == R1_PAD_2 {
                        pad.position = Some(copperpour_ipc::builders::vec2(1.0, 1.0));
                        *item = pack_any(&pad, "kiapi.board.types.Pad");
                    }
                }
            }
        });
        let scene = scene(kicad);
        let (error, body) = scene.set(enable_r1_pad_1()).await;
        assert!(error, "{body}");
        assert_eq!(
            body["error"]["kind"], "mutation_outcome_uncertain",
            "{body}"
        );
        assert!(
            body["error"]["reason"]
                .as_str()
                .unwrap()
                .contains(&format!("pad {R1_PAD_2} position")),
            "{body}"
        );
    }

    /// A commit KiCad refuses, with the read-back showing nothing changed, is
    /// a refusal saying nothing was written: for a pad, and for a via alone,
    /// whose read-back is part of the same evidence.
    #[tokio::test]
    async fn a_commit_kicad_refuses_is_reported_as_nothing_written() {
        for args in [
            enable_r1_pad_1(),
            json!({ "targets": { "vias": [VIA] }, "settings": { "curved_edges": false } }),
        ] {
            let mut kicad = fixture(Some((10, 0, 8)));
            kicad.reject_updates = true;
            let scene = scene(kicad);
            let (error, body) = scene.set(args.clone()).await;
            assert!(error, "{args}: {body}");
            assert_eq!(body["error"]["kind"], "handler_error", "{args}: {body}");
            assert!(
                body["message"]
                    .as_str()
                    .unwrap()
                    .contains("Nothing was written"),
                "{body}"
            );
            assert!(!scene.writes().contains(&"RefillZones".to_string()));
        }
    }

    /// Settings changed in KiCad between the tool's read and its write: the
    /// batch is refused as stale before anything is sent.
    #[tokio::test]
    async fn settings_changed_since_the_read_are_refused_as_stale() {
        let mut kicad = fixture(Some((10, 0, 8)));
        kicad.on_second_pad_read = Some(|held| {
            let via = &mut held.vias[0];
            via.teardrop.as_mut().unwrap().curved_edges = false;
        });
        let scene = scene(kicad);
        let (error, body) = scene
            .set(json!({ "targets": { "vias": [VIA] }, "settings": { "best_length_ratio": 0.5 } }))
            .await;
        assert!(error, "{body}");
        assert_eq!(body["error"]["kind"], "stale_target", "{body}");
        assert!(scene.writes().is_empty(), "{:?}", scene.writes());
    }

    #[tokio::test]
    async fn pad_settings_changed_before_the_footprint_read_are_refused_as_stale() {
        let mut kicad = fixture(Some((10, 0, 8)));
        kicad.on_second_footprint_read = Some(|held| {
            for item in held
                .footprints
                .iter_mut()
                .flat_map(|fp| fp.definition.iter_mut().flat_map(|d| d.items.iter_mut()))
            {
                if any_is(item, "kiapi.board.types.Pad") {
                    let mut pad = board::Pad::decode(item.value.as_slice()).unwrap();
                    if pad.id.as_ref().unwrap().value == R1_PAD_1 {
                        pad.teardrop.as_mut().unwrap().curved_edges = true;
                        *item = pack_any(&pad, "kiapi.board.types.Pad");
                    }
                }
            }
        });
        let scene = scene(kicad);
        let (error, body) = scene.set(enable_r1_pad_1()).await;
        assert!(error, "{body}");
        assert_eq!(body["error"]["kind"], "stale_target", "{body}");
        assert!(scene.writes().is_empty(), "{:?}", scene.writes());
        assert!(scene.pad(R1_PAD_1).teardrop.unwrap().curved_edges);
    }

    /// KiCad answering `ISC_OK` but keeping nothing, as it does for a pad
    /// update aimed at its stale item cache, is caught by the read-back.
    #[tokio::test]
    async fn an_update_kicad_accepts_but_drops_is_uncertain() {
        let mut kicad = fixture(Some((10, 0, 8)));
        kicad.drop_updates = true;
        let scene = scene(kicad);
        let (error, body) = scene.set(enable_r1_pad_1()).await;
        assert!(error, "{body}");
        assert_eq!(
            body["error"]["kind"], "mutation_outcome_uncertain",
            "{body}"
        );
        assert!(
            body["error"]["reason"]
                .as_str()
                .unwrap()
                .contains(&format!("pad {R1_PAD_1} teardrop")),
            "{body}"
        );
    }

    #[tokio::test]
    async fn refill_is_opt_in_and_runs_after_the_commit() {
        let scene = scene(fixture(Some((10, 0, 8))));
        let mut args = enable_r1_pad_1();
        args["refill_zones"] = json!(true);
        let (error, body) = scene.set(args).await;
        assert!(!error, "{body}");
        assert_eq!(
            scene.writes(),
            ["BeginCommit", "UpdateItems", "EndCommit", "RefillZones"]
        );
        assert_eq!(body["copper"]["refill"]["teardrop_zones_after"], 5);
        assert_eq!(body["copper"]["refill"]["completion_verified"], false);
    }

    #[tokio::test]
    async fn a_failed_refill_read_keeps_the_applied_settings_evidence() {
        let mut kicad = fixture(Some((10, 0, 8)));
        kicad.fail_refill_read = true;
        let scene = scene(kicad);
        let mut args = enable_r1_pad_1();
        args["refill_zones"] = json!(true);
        let (error, body) = scene.set(args).await;
        assert!(error, "{body}");
        assert_eq!(
            body["error"]["kind"], "mutation_outcome_uncertain",
            "{body}"
        );
        assert_eq!(body["applied"]["settings_source"], "ipc_readback", "{body}");
        assert_eq!(body["applied"]["verified_changes"]["count"], 1, "{body}");
        assert_eq!(
            body["applied"]["copper"]["commit"]["added_count"], 1,
            "{body}"
        );
        assert!(body["applied"]["copper"]["refill"].is_null(), "{body}");
        assert_eq!(
            scene.writes(),
            ["BeginCommit", "UpdateItems", "EndCommit", "RefillZones"]
        );
    }

    #[tokio::test]
    async fn settings_that_already_hold_send_no_commit() {
        let scene = scene(fixture(Some((10, 0, 8))));
        let (error, body) = scene
            .set(json!({
                "targets": { "vias": [VIA] },
                "settings": { "enabled": true, "best_length_ratio": 0.6 }
            }))
            .await;
        assert!(!error, "{body}");
        assert_eq!(body["changed"], 0);
        assert_eq!(body["unchanged"], 1);
        assert!(scene.writes().is_empty(), "{:?}", scene.writes());
    }

    #[tokio::test]
    async fn a_legacy_teardrop_board_is_refused_with_nothing_written() {
        let mut kicad = fixture(Some((10, 0, 8)));
        kicad.text = kicad
            .text
            .replace("(legacy_teardrops no)", "(legacy_teardrops yes)");
        let scene = scene(kicad);
        let (error, body) = scene.set(enable_r1_pad_1()).await;
        assert!(error);
        assert_eq!(body["error"]["kind"], "plan_blocked", "{body}");
        for dry_run in [true, false] {
            let (error, unchanged) = scene
                .set(json!({
                    "targets": { "vias": [VIA] },
                    "settings": { "enabled": true, "best_length_ratio": 0.6 },
                    "refill_zones": true,
                    "dry_run": dry_run,
                }))
                .await;
            assert!(error, "{unchanged}");
            assert_eq!(unchanged, body);
        }
        assert!(scene.writes().is_empty());
        let (_, read) = scene.get(json!({})).await;
        assert_eq!(read["legacy_teardrops"], true);
    }

    /// A footprint-owned group is absent from KiCad 10's footprint message,
    /// so a whole-footprint update would drop it: its pads are refused, while
    /// a via on the same board is not.
    #[tokio::test]
    async fn pads_of_a_footprint_owning_a_group_are_refused_and_vias_are_not() {
        let mut kicad = fixture(Some((10, 0, 8)));
        let anchor = format!("(uuid \"{R1}\")");
        assert_eq!(kicad.text.matches(&anchor).count(), 1);
        kicad.text = kicad.text.replace(
            &anchor,
            &format!("{anchor}\n\t\t(group \"\" (uuid \"g1\") (members \"{R1_PAD_1}\"))"),
        );
        let scene = scene(kicad);
        let (error, body) = scene.set(enable_r1_pad_1()).await;
        assert!(error);
        assert_eq!(body["error"]["kind"], "plan_blocked", "{body}");
        assert!(body["message"].as_str().unwrap().contains("R1"));
        assert!(scene.writes().is_empty());

        let (error, body) = scene
            .set(json!({ "targets": { "vias": [VIA] }, "settings": { "curved_edges": false } }))
            .await;
        assert!(!error, "{body}");
        assert_eq!(scene.writes(), ["BeginCommit", "UpdateItems", "EndCommit"]);
    }

    #[tokio::test]
    async fn malformed_requests_are_refused_before_kicad_is_asked() {
        let scene = scene(fixture(Some((10, 0, 8))));
        for (args, field) in [
            (
                json!({ "targets": {}, "settings": { "enabled": true } }),
                "targets",
            ),
            (
                json!({ "targets": { "all_pads": true }, "settings": {} }),
                "settings",
            ),
            (
                json!({ "targets": { "all_pads": true }, "settings": { "best_width_ratio": 0.5 } }),
                "settings.best_width_ratio",
            ),
            (
                json!({ "targets": { "all_pads": true }, "settings": { "max_length_mm": 0 } }),
                "settings.max_length_mm",
            ),
            (
                json!({ "targets": { "pads": [{ "reference": "R1" }] }, "settings": { "enabled": true } }),
                "targets.pads[0]",
            ),
            (
                json!({ "targets": { "all_pads": true }, "settings": { "enabled": "yes" } }),
                "settings.enabled",
            ),
        ] {
            let (error, body) = scene.set(args.clone()).await;
            assert!(error, "{args}");
            assert_eq!(body["error"]["kind"], "invalid_argument", "{args}: {body}");
            assert_eq!(body["error"]["field"], field, "{args}: {body}");
        }
        assert!(scene.held.lock().unwrap().log.is_empty());
    }

    #[tokio::test]
    async fn a_selector_naming_nothing_on_the_board_is_refused() {
        let scene = scene(fixture(Some((10, 0, 8))));
        for (targets, field) in [
            (
                json!({ "pads": [{ "reference": "R1", "pad_number": "9" }] }),
                "targets.pads[0]",
            ),
            (json!({ "pads": [{ "uuid": VIA }] }), "targets.pads[0]"),
            (json!({ "vias": [R1_PAD_1] }), "targets.vias[0]"),
            (json!({ "references": ["U9"] }), "targets.references[0]"),
            (json!({ "nets": ["NOPE"] }), "targets.nets[0]"),
        ] {
            let (error, body) = scene
                .set(json!({ "targets": targets, "settings": { "enabled": true } }))
                .await;
            assert!(error, "{targets}");
            assert_eq!(body["error"]["kind"], "invalid_argument");
            assert_eq!(body["error"]["field"], field, "{body}");
        }
        assert!(scene.writes().is_empty());
    }

    /// By pad number every pad with that number is selected; by net, pads
    /// and vias alike.
    #[tokio::test]
    async fn selectors_union_by_number_reference_and_net() {
        let scene = scene(fixture(Some((10, 0, 8))));
        let (_, body) = scene
            .set(json!({
                "targets": { "pads": [{ "reference": "R1", "pad_number": "1" }], "nets": ["GND"] },
                "settings": { "enabled": true },
                "dry_run": true
            }))
            .await;
        // R1 pad 1 (VCC) plus the GND pads and the via.
        assert_eq!(body["selected"], 5, "{body}");
        assert_eq!(body["changed"], 1);
    }

    /// The tools are served through the catalogue in the `pcb_copper` toolset.
    #[tokio::test]
    async fn the_tools_are_served_in_pcb_copper() {
        let names: Vec<&str> = crate::tools::pcb_copper::tools()
            .iter()
            .map(|tool| tool.name)
            .collect();
        assert!(names.contains(&"get_teardrop_settings"));
        assert!(names.contains(&"set_teardrop_settings"));
        let scene = scene(fixture(Some((10, 0, 8))));
        let handler = crate::mcp::handler::McpHandler::new(crate::tools::ServerConfig {
            ipc_address: scene.server.address().to_string(),
            eager_toolsets: true,
            ..Default::default()
        })
        .await
        .unwrap();
        let response = handler
            .handle_message(json!({
                "jsonrpc": "2.0", "id": 1, "method": "tools/call",
                "params": { "name": "set_teardrop_settings", "arguments": {
                    "board": scene.board,
                    "targets": { "pads": [{ "uuid": R1_PAD_1 }] },
                    "settings": { "enabled": true }
                }}
            }))
            .await
            .unwrap();
        let result = response.result.unwrap();
        assert_eq!(result["isError"], false, "{result}");
        assert_eq!(scene.writes(), ["BeginCommit", "UpdateItems", "EndCommit"]);
    }

    /// The tools against a running KiCad holding a disposable copy of
    /// `teardrops_kicad10.kicad_pcb` (see that fixture's README for the
    /// setup). Ignored: needs `KICAD_API_SOCKET` and
    /// `COPPERPOUR_LIVE_TEARDROP_BOARD`. On a KiCad before 10.0.7 it checks
    /// the refusal instead. `COPPERPOUR_TEARDROP_TOOL_EVIDENCE` writes the
    /// responses as JSON.
    #[tokio::test]
    #[ignore = "requires a running KiCad GUI with its IPC API enabled"]
    async fn live_teardrop_tools_change_settings_and_copper() {
        let socket = std::env::var("KICAD_API_SOCKET").expect("KICAD_API_SOCKET");
        let board = std::env::var("COPPERPOUR_LIVE_TEARDROP_BOARD").expect("board");
        let ctx = ctx_talking_to(socket);
        let call = |args: Value, set: bool| {
            let ctx = &ctx;
            let mut args = args;
            args["board"] = json!(board);
            async move {
                let result = if set {
                    handle_set(&args, ctx).await.unwrap()
                } else {
                    handle_get(&args, ctx).await.unwrap()
                };
                body(result)
            }
        };
        let mut evidence = Map::new();
        let (error, read) = call(json!({}), false).await;
        if error {
            assert_eq!(read["error"]["kind"], "unsupported_capability", "{read}");
            let (error, set) = call(enable_r1_pad_1(), true).await;
            assert!(error);
            assert_eq!(set["error"]["kind"], "unsupported_capability", "{set}");
            evidence.insert("refused_get".into(), read);
            evidence.insert("refused_set".into(), set);
            write_live_evidence(evidence);
            return;
        }
        assert_eq!(read["total"], 7, "{read}");
        evidence.insert("read".into(), read.clone());
        let gnd_teardrops = read["teardrop_zones"]["by_net"]["GND"]
            .as_u64()
            .unwrap_or(0);
        assert!(
            gnd_teardrops > 0,
            "the fixture's GND teardrops are missing: {read}"
        );

        // Disable the GND pads and via: the commit itself removes their teardrops.
        let (error, off) = call(
            json!({ "targets": { "nets": ["GND"] }, "settings": { "enabled": false } }),
            true,
        )
        .await;
        assert!(!error, "{off}");
        assert_eq!(off["changed"], 4, "{off}");
        assert_eq!(off["copper"]["commit"]["teardrop_zones_after"], 0, "{off}");
        assert_eq!(off["copper"]["refill"], Value::Null);
        evidence.insert("disable".into(), off);

        // Enable them again and request a refill. The next copper read is an observation.
        let (error, on) = call(
            json!({
                "targets": { "nets": ["GND"] },
                "settings": { "enabled": true },
                "refill_zones": true
            }),
            true,
        )
        .await;
        assert!(!error, "{on}");
        assert_eq!(
            on["copper"]["commit"]["teardrop_zones_after"].as_u64(),
            Some(gnd_teardrops),
            "{on}"
        );
        assert!(
            on["copper"]["refill"]["teardrop_zones_after"].is_u64(),
            "{on}"
        );
        assert_eq!(on["copper"]["refill"]["completion_verified"], false, "{on}");
        let fields = on["verified_changes"]["fields"].as_array().unwrap();
        assert_eq!(fields.len(), 4, "three pads and the via: {on}");
        assert!(
            fields
                .iter()
                .all(|f| f.as_str().unwrap().ends_with(" teardrop")),
            "{on}"
        );
        evidence.insert("enable_with_refill".into(), on);

        // Two more edits of the same footprint's pads, one by number and one
        // by UUID: every edit lands, which pad-by-pad updates would not.
        for (index, (targets, ratio)) in [
            (
                json!({ "pads": [{ "reference": "R1", "pad_number": "2" }] }),
                0.8,
            ),
            (json!({ "pads": [{ "uuid": R1_PAD_2 }] }), 0.9),
        ]
        .into_iter()
        .enumerate()
        {
            let (error, edit) = call(
                json!({ "targets": targets, "settings": { "best_length_ratio": ratio } }),
                true,
            )
            .await;
            assert!(!error, "{edit}");
            assert_eq!(
                edit["targets"][0]["after"]["best_length_ratio"], ratio,
                "{edit}"
            );
            evidence.insert(format!("repeat_edit_{index}"), edit);
        }
        let (_, after) = call(json!({ "references": ["R1"] }), false).await;
        let r1_pad_1 = after["targets"]
            .as_array()
            .unwrap()
            .iter()
            .find(|t| t["uuid"] == R1_PAD_1)
            .unwrap();
        assert_eq!(r1_pad_1["settings"]["enabled"], false, "R1 pad 1 untouched");

        // `set_pad_net` shares the footprint update path, and both compare the
        // whole footprint after: a footprint KiCad rotated or flipped must
        // still come back exactly as sent. R2 at 30 degrees, J1 on the back.
        let handler = crate::mcp::handler::McpHandler::new(crate::tools::ServerConfig {
            ipc_address: std::env::var("KICAD_API_SOCKET").unwrap(),
            eager_toolsets: true,
            ..Default::default()
        })
        .await
        .unwrap();
        let tool = |name: &'static str, arguments: Value| {
            let handler = &handler;
            async move {
                let response = handler
                    .handle_message(json!({
                        "jsonrpc": "2.0", "id": 1, "method": "tools/call",
                        "params": { "name": name, "arguments": arguments }
                    }))
                    .await
                    .unwrap();
                let result = response.result.unwrap();
                assert_eq!(result["isError"], false, "{name}: {result}");
                serde_json::from_str::<Value>(result["content"][0]["text"].as_str().unwrap())
                    .unwrap_or(Value::Null)
            }
        };
        tool(
            "rotate_component",
            json!({ "board": board, "reference": "R2", "rotation": 30.0 }),
        )
        .await;
        tool(
            "flip_component",
            json!({ "board": board, "reference": "J1", "layer": "B.Cu" }),
        )
        .await;
        for (reference, pad, nets) in [
            ("R1", "1", ["GND", "VCC"]),
            ("R2", "2", ["GND", "VCC"]),
            ("J1", "1", ["GND", "VCC"]),
        ] {
            for net in nets {
                let reply = tool(
                    "set_pad_net",
                    json!({ "board": board, "reference": reference, "pad_number": pad, "net_name": net }),
                )
                .await;
                assert_eq!(reply["pads"][0]["net"], net, "{reply}");
                evidence.insert(format!("set_pad_net_{reference}_{pad}_{net}"), reply);
            }
        }
        let (error, edit) = call(
            json!({
                "targets": { "pads": [{ "uuid": R1_PAD_1 }], "references": ["R2", "J1"] },
                "settings": { "curved_edges": true }
            }),
            true,
        )
        .await;
        assert!(!error, "{edit}");
        // R1 pad 1 and the VCC pads of R2 and J1 change; their GND pads
        // already have curved edges from the fixture.
        assert_eq!(edit["selected"], 5, "{edit}");
        assert_eq!(edit["changed"], 3, "{edit}");
        evidence.insert("teardrop_after_pad_nets".into(), edit);
        write_live_evidence(evidence);
    }

    fn write_live_evidence(evidence: Map<String, Value>) {
        if let Ok(path) = std::env::var("COPPERPOUR_TEARDROP_TOOL_EVIDENCE") {
            std::fs::write(path, serde_json::to_string_pretty(&evidence).unwrap()).unwrap();
        }
    }
}
