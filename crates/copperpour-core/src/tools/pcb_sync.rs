//! Synchronize a saved schematic design into a live PCB Editor board.

mod footprints;
mod live_board;
mod netlist;
mod plan;

#[cfg(test)]
mod chunk_tests;
#[cfg(test)]
mod tests;

pub(super) use footprints::apply_library_fields;
pub(crate) use live_board::{zone_net, ZoneNet};

use crate::hex::encode_hex;
use crate::mcp::protocol::{CallToolResult, ToolContent};
use crate::tools::library::PadFabricationProperty;
use crate::tools::library_footprint::{
    carries_kicad_10_0_7_fields, carries_net_ties, CarriedByKiCad,
};
use crate::tools::pcb_board::{attempt_ipc_write, BoardWrite};
use crate::tools::{BoundedList, ToolContext};
use anyhow::Result;
use footprints::{
    additions_missing_pads, carries_symbol_units, kicad_10_0_7_not_carried,
    leave_out_unconnected_pins, library_fields_not_carried, net_ties_not_carried,
    prepare_additions, prepare_replacements, replacements_that_cannot_be_built,
    symbol_units_not_carried, unsupported_features_not_carried, UnpreparedFootprint,
};
use live_board::{
    acknowledged_apply_evidence, build_mutation_items, connected_zone_pads, copper_net_updates,
    copper_not_moved, create_sync_items_in, footprint_shapes, footprints_still_on_board,
    kicad_version, snapshot_board, updates_not_carried, verify_board_matches_what_was_sent,
};
use netlist::{apply_saved_symbol_flags, parse_exported_netlist, saved_hierarchy_files};
use plan::{
    clear_changes, conflict, leave_out_updates, plan_copper_nets, plan_sync_acknowledging,
    planned_deletions, refresh_revision_with_staging, refuse_plan, restage_additions, skip_parts,
};
use serde::Serialize;
use sha2::{Digest, Sha256};
use std::borrow::Cow;
use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;

#[derive(Debug, Clone, PartialEq, Eq)]
struct ExportedDesign {
    /// UUID of the requested root schematic sheet, used to normalize
    /// root-prefixed PCB footprint paths (`/<root>/<sheet...>/<symbol>`)
    /// against root-relative netlist paths (`/<sheet...>/<symbol>`) (#82).
    root_sheet_uuid: Option<String>,
    components: Vec<DesignComponent>,
    skipped: Vec<SkippedComponent>,
    /// Components the export names but carries no `(footprint …)` for: their
    /// `Footprint` property is empty. Nothing can be placed for them, so they
    /// are reported rather than planned — and never fatal (#507).
    unassigned: Vec<UnassignedComponent>,
    /// Every net the export connects to a pin, footprint or not: the nets
    /// KiCad's Update PCB reads as the netlist's (`updateCopperZoneNets`,
    /// #862).
    net_names: BTreeSet<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct UnassignedComponent {
    reference: String,
    value: String,
    lib_id: Option<String>,
    symbol_path: String,
}

/// What the board holds for a component whose schematic footprint is empty.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
enum UnassignedBoardState {
    /// No footprint on the board: nothing is added, the part is reported.
    Absent,
    /// A footprint with this identity or reference already exists; it is left
    /// exactly as it is, the way eeschema skips "cannot update … no footprint
    /// assigned" and continues.
    Kept,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
struct UnassignedFootprint {
    reference: String,
    value: String,
    lib_id: Option<String>,
    symbol_path: String,
    board_state: UnassignedBoardState,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct SkippedComponent {
    reference: String,
    symbol_path: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct DesignComponent {
    reference: String,
    value: String,
    footprint_id: String,
    /// The first unit's path, the one an added footprint is linked to.
    symbol_path: String,
    /// The path of each other unit of a multi-unit symbol, in the order the
    /// netlist's `tstamps` lists them. A board footprint linked to any unit
    /// is this component's, as KiCad's Update PCB matches it.
    other_unit_paths: Vec<String>,
    dnp: bool,
    /// The symbol's Exclude from BOM: the part stays on the board, its
    /// footprint marked, as KiCad's Update PCB marks it (#875).
    exclude_from_bom: bool,
    pad_nets: BTreeMap<String, String>,
    /// The pins KiCad left unconnected: each alone on its net, which is an
    /// `unconnected-…` net or has a no-connect pin type. They connect
    /// nothing, so a footprint with no pad for one (a keyed header's key)
    /// takes every other net (`nets_for_pads`).
    unconnected_pins: BTreeSet<String>,
    /// The symbol fields KiCad's Update PCB copies onto the footprint, in the
    /// order the netlist lists them (#788).
    fields: Vec<SymbolField>,
    /// The sheet KiCad's Update PCB gives the footprint (#237).
    sheet: SymbolSheet,
}

impl DesignComponent {
    /// The path of each unit, the first unit's first.
    fn unit_paths(&self) -> impl Iterator<Item = &str> {
        std::iter::once(self.symbol_path.as_str())
            .chain(self.other_unit_paths.iter().map(String::as_str))
    }
}

/// A symbol field to copy onto the footprint. An empty value is still a field:
/// KiCad's netlist reader keeps it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
struct SymbolField {
    name: String,
    value: String,
}

/// The sheet a symbol is placed on, its library symbol's footprint filters
/// and its units' pins, which KiCad's Update PCB sets on the footprint. Rule
/// areas generated from sheets, and Repeat Layout with them, find a sheet's
/// footprints by `sheet_name` (#237).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
struct SymbolSheet {
    sheet_name: String,
    sheet_file: String,
    footprint_filters: String,
    /// Which pins belong to each unit, in the netlist's order, as KiCad's
    /// Update PCB compares and sets them (`updateComponentUnits`). KiCad's API
    /// carries them from 10.0.8 (#682).
    symbol_units: Vec<SymbolUnit>,
}

/// One unit of a symbol and the numbers of its pins.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
struct SymbolUnit {
    name: String,
    pin_numbers: Vec<String>,
}

impl SymbolSheet {
    fn of(footprint: &copperpour_ipc::gen::kiapi::board::types::FootprintInstance) -> Self {
        Self {
            sheet_name: footprint.symbol_sheet_name.clone(),
            sheet_file: footprint.symbol_sheet_filename.clone(),
            footprint_filters: footprint.symbol_footprint_filters.clone(),
            symbol_units: footprint
                .symbol_units
                .iter()
                .map(|unit| SymbolUnit {
                    name: unit.name.clone(),
                    pin_numbers: unit.pin_numbers.clone(),
                })
                .collect(),
        }
    }

    fn listed(&self) -> String {
        let units = self
            .symbol_units
            .iter()
            .map(|unit| format!("{:?} {:?}", unit.name, unit.pin_numbers))
            .collect::<Vec<_>>();
        format!(
            "sheet name {:?}, sheet file {:?}, footprint filters {:?}, symbol units [{}]",
            self.sheet_name,
            self.sheet_file,
            self.footprint_filters,
            units.join(", ")
        )
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
struct Point {
    x: f64,
    y: f64,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
struct Bounds {
    min_x: f64,
    min_y: f64,
    max_x: f64,
    max_y: f64,
}

#[derive(Debug, Clone, PartialEq)]
struct BoardFootprint {
    kiid: String,
    reference: String,
    value: String,
    footprint_id: String,
    symbol_path: Option<String>,
    pad_nets: BTreeMap<String, String>,
    /// Every pad the live footprint has, netted or not. `pad_nets` holds only
    /// pads that carry a net, so it cannot answer whether a pad exists.
    pad_numbers: BTreeSet<String>,
    position: Point,
    rotation: f64,
    layer: String,
    locked: bool,
    dnp: bool,
    exclude_from_bom: bool,
    not_in_schematic: bool,
    /// Every field but Reference and Value, by name: Datasheet, Description
    /// and the custom ones (#788).
    fields: BTreeMap<String, String>,
    /// The sheet the live footprint carries (#237).
    sheet: SymbolSheet,
}

#[derive(Debug, Clone, PartialEq)]
struct BoardState {
    footprints: Vec<BoardFootprint>,
    /// Net name to the number of routed copper objects (tracks, arcs, vias,
    /// and zones) carrying the net.
    routed_nets: BTreeMap<String, usize>,
    bounds: Bounds,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
enum PlanStatus {
    Ready,
    Noop,
    Conflict,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
struct CountPair {
    planned: usize,
    applied: usize,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
struct SyncCounts {
    added: CountPair,
    updated: CountPair,
    pads_reassigned: CountPair,
    board_only_preserved: CountPair,
    skipped_by_flag: CountPair,
    /// Schematic components with no footprint assigned: reported, never
    /// planned, never fatal (#507).
    unassigned_footprint: CountPair,
    /// Parts left out because the preflight found their footprint cannot be
    /// built or updated; listed in `skipped_footprints`, never fatal to the
    /// rest (`skip_parts`).
    skipped_footprint: CountPair,
    /// Footprints with no symbol deleted on request
    /// (`delete_footprints_without_symbols`, #861).
    deleted: CountPair,
    /// Vias and copper zones moved off a net that left the schematic (#862).
    copper_net_changes: CountPair,
    conflicts: CountPair,
}

/// A board footprint no schematic symbol matched, without the board-only
/// attribute, that the plan keeps (#861). KiCad's Update PCB deletes such a
/// footprint with **Delete footprints with no symbols**; a footprint with the
/// board-only attribute (a logo, a mounting hole) is never listed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
struct FootprintWithoutSymbol {
    reference: String,
    kiid: String,
    footprint_id: String,
    locked: bool,
    reason: WithoutSymbolReason,
}

/// Why no symbol matched a board footprint (#870).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
enum WithoutSymbolReason {
    /// No symbol in the schematic has the footprint's identity.
    SymbolRemoved,
    /// Its symbol is marked Exclude from board (`on_board no`).
    ExcludedFromBoard,
}

impl WithoutSymbolReason {
    fn describe(self) -> &'static str {
        match self {
            Self::SymbolRemoved => "has no symbol in the schematic",
            Self::ExcludedFromBoard => "has its symbol excluded from the board",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize)]
struct PreservedBoardState {
    position: Point,
    rotation: f64,
    layer: String,
    locked: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum PlannedChange {
    Add {
        reference: String,
        value: String,
        footprint_id: String,
        symbol_path: String,
        dnp: bool,
        exclude_from_bom: bool,
        pad_nets: BTreeMap<String, String>,
        /// The component's `unconnected_pins`, which the library footprint
        /// need not have (`leave_out_unconnected_pins`).
        #[serde(skip)]
        unconnected_pins: BTreeSet<String>,
        position: Point,
        /// Every symbol field the footprint receives.
        fields: Vec<SymbolField>,
        /// The sheet the footprint receives (#237).
        #[serde(flatten)]
        sheet: SymbolSheet,
    },
    Update {
        kiid: String,
        reference: String,
        value: String,
        symbol_path: String,
        dnp: bool,
        exclude_from_bom: bool,
        pad_nets: BTreeMap<String, String>,
        /// The component's `unconnected_pins`, which a replacement footprint
        /// need not have (`leave_out_unconnected_pins`).
        #[serde(skip)]
        unconnected_pins: BTreeSet<String>,
        preserve: PreservedBoardState,
        /// Only the symbol fields whose value the footprint lacks or differs on.
        fields: Vec<SymbolField>,
        /// The sheet the footprint takes, whether or not it differs (#237).
        #[serde(flatten)]
        sheet: SymbolSheet,
        /// The footprint differs from its symbol in its sheet or its units
        /// alone (#237, #682).
        #[serde(skip)]
        sheet_only: bool,
        /// The schematic names another footprint: the placed one is rebuilt
        /// from it in place (`pcb_footprint_update::replace_footprint`).
        #[serde(skip_serializing_if = "Option::is_none")]
        replace_footprint: Option<ReplacedFootprint>,
    },
    /// A footprint no symbol matched, deleted on request
    /// (`delete_footprints_without_symbols`, #861).
    Delete {
        kiid: String,
        reference: String,
        footprint_id: String,
        reason: WithoutSymbolReason,
    },
}

/// The footprint a planned update replaces, and the one it replaces it with.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
struct ReplacedFootprint {
    from: String,
    to: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
struct SyncDiagnostic {
    code: String,
    message: String,
    /// The one part the diagnostic concerns; `None` when it concerns several
    /// parts or the board as a whole.
    reference: Option<String>,
    /// Every part the diagnostic concerns. One unusable library footprint
    /// blocks each part that uses it, and the caller has to be told all of
    /// them to know what to substitute (#657).
    references: Vec<String>,
    /// The library footprint the diagnostic is about, when it is about one.
    footprint_id: Option<String>,
    /// The pad net change a `routed_pad_net_change` diagnostic is about, or
    /// that an acknowledgement let through (#38). Absent from every other
    /// diagnostic.
    #[serde(skip_serializing_if = "Option::is_none")]
    net_change: Option<NetChange>,
    /// The via or copper zone a note is about (#862). Absent from every
    /// other diagnostic.
    #[serde(skip_serializing_if = "Option::is_none")]
    kiid: Option<String>,
}

/// One pad's net change on a footprint the diagnostic names (#38).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
struct NetChange {
    pad: String,
    from: String,
    to: String,
}

/// A routed pad net change the caller accepts: a `routed_pad_net_change`
/// diagnostic's `reference` and its `net_change` (#38).
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct AcknowledgedNetChange {
    reference: String,
    pad: String,
    from: String,
    to: String,
}

/// The routed copper on one net an acknowledged change names: tracks, arcs,
/// vias and copper zones carrying it before the commit, and after it as KiCad
/// reads it back (#38).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
struct RoutedCopper {
    net: String,
    before_count: usize,
    after_count: usize,
}

/// A via or a copper zone.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
enum CopperKind {
    Via,
    Zone,
}

/// A via or a copper zone on the board, as the sync reads it to move the ones
/// whose net leaves the schematic (#862). Rule areas are not copper.
#[derive(Debug, Clone, PartialEq, Eq)]
struct CopperItem {
    kind: CopperKind,
    kiid: String,
    /// Empty for an item on no net.
    net: String,
    /// The zone's name; empty for a via or an unnamed zone.
    name: String,
}

/// A via or copper zone the plan moves off a net that left the schematic, as
/// KiCad's Update PCB moves it (#862).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
struct CopperNetChange {
    kind: CopperKind,
    kiid: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    name: String,
    from: String,
    to: String,
}

/// What an apply that took acknowledged net changes leaves for DRC (#38).
#[derive(Debug, Clone, PartialEq)]
struct AcknowledgedApply {
    routed_copper: Vec<RoutedCopper>,
    drc: serde_json::Value,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
struct SyncPlan {
    status: PlanStatus,
    plan_revision: String,
    counts: SyncCounts,
    changes: Vec<PlannedChange>,
    diagnostics: Vec<SyncDiagnostic>,
    /// Survives a conflict: it is a report about the schematic, not a change
    /// the plan would make.
    unassigned: Vec<UnassignedFootprint>,
    /// Why each skipped part was left out of the plan (`skip_parts`), with
    /// the preflight's own diagnostic. Part of the plan's identity: what was
    /// left out is part of what was reviewed.
    skipped: Vec<SyncDiagnostic>,
    /// The footprints on the board `plan_sync` found it cannot update, each
    /// with why. The handler skips them with the parts its own preflight
    /// refuses (`skip_parts`), so one of them does not stop the sync of
    /// every other part.
    #[serde(skip)]
    refused_updates: Vec<SyncDiagnostic>,
    /// The footprints no symbol matched that the plan keeps, apart from those
    /// with the board-only attribute (#861). Those it deletes are `Delete`
    /// changes instead.
    without_symbols: Vec<FootprintWithoutSymbol>,
    /// The vias and copper zones the plan moves off nets that leave the
    /// schematic (#862), set by the handler (`plan_copper_nets`). Part of the
    /// plan's identity.
    copper_net_changes: Vec<CopperNetChange>,
    /// What the live board's measured extent was, set from the snapshot the
    /// plan was made against, so the dry run and the apply report the same.
    /// Not part of the plan's identity: `sync_response` reports it.
    #[serde(skip)]
    staging: Option<StagingEvidence>,
    /// Set by an apply that took acknowledged net changes; `sync_response`
    /// reports it.
    #[serde(skip)]
    acknowledged_apply: Option<AcknowledgedApply>,
}

/// What added footprints were staged beside (#688).
///
/// Additions go to the right of the board's measured extent. KiCad 10.0.5
/// will not list tables or generators (KiCad 10.0.7+ lists tables and still
/// declines generators), so a live board is never measured whole: the evidence
/// says which classes were left out, and tells a board with no items apart from
/// one whose items could not be measured. Neither refuses the sync; staging is
/// a starting position the caller can move.
#[derive(Debug, Clone, PartialEq, Eq)]
struct StagingEvidence {
    /// KiCad measured at least one item.
    measured: bool,
    unavailable: Vec<copperpour_ipc::IpcUnavailableItemClass>,
}

impl StagingEvidence {
    fn of(bounds: &copperpour_ipc::IpcBoardBounds) -> Self {
        Self {
            measured: bounds.extents.is_some(),
            unavailable: bounds.unavailable.clone(),
        }
    }

    /// What the staged positions were computed from.
    fn basis(&self) -> &'static str {
        match (self.measured, self.unavailable.is_empty()) {
            // Every class listed and measured.
            (true, true) => "complete_geometry",
            // Beside the items KiCad measured; the listed classes are not in it.
            (true, false) => "partial_geometry",
            // Every class listed, and none held an item.
            (false, true) => "empty_board",
            // Nothing measured, but some classes could not be listed, so the
            // board is not known to be empty. Staged from the origin.
            (false, false) => "no_measured_geometry",
        }
    }

    fn json(&self) -> serde_json::Value {
        serde_json::json!({
            "basis": self.basis(),
            "unavailable_item_classes": self
                .unavailable
                .iter()
                .map(super::pcb_board::unavailable_item_class)
                .collect::<Vec<_>>(),
        })
    }
}

#[derive(Debug)]
struct LiveSnapshot {
    state: BoardState,
    /// What `state.bounds` was measured from.
    staging: StagingEvidence,
    items: BTreeMap<String, prost_types::Any>,
    net_codes: BTreeMap<String, i32>,
    /// Each net's routed copper as counted, before an unreadable zone marks
    /// every net routed in `state.routed_nets`.
    routed_copper: BTreeMap<String, usize>,
    /// The board's vias and copper zones, in KiCad's order, and each as KiCad
    /// listed it, by KIID (#862).
    copper: Vec<CopperItem>,
    copper_items: BTreeMap<String, prost_types::Any>,
    /// Each footprint pad's footprint KIID and pad number, by the pad's KIID.
    pad_owners: BTreeMap<String, (String, String)>,
    document: copperpour_ipc::gen::kiapi::common::types::DocumentSpecifier,
}

#[derive(Debug)]
struct PreparedFootprint {
    pads: Vec<copperpour_ipc::IpcPadDefinition>,
    graphics: Vec<copperpour_ipc::IpcGraphicDefinition>,
    fields: copperpour_ipc::IpcFieldPlacement,
    /// The library's `(attr …)`: mounting style and exclusion flags.
    attributes: copperpour_ipc::gen::kiapi::board::types::FootprintAttributes,
    /// The library's description and keywords, which KiCad keeps on the
    /// footprint definition.
    description_and_keywords: copperpour_ipc::gen::kiapi::board::types::FootprintAttributes,
    models: Vec<copperpour_ipc::gen::kiapi::board::types::Footprint3DModel>,
    /// The library's properties as typed fields, or why they could not be
    /// read. Unreadable properties never refuse the part (#789).
    library_fields: std::result::Result<super::library_footprint::LibraryProperties, String>,
    /// Each pad's fabrication property, in the order of `pads` (#149).
    pad_properties: Vec<PadFabricationProperty>,
    /// Each pad's zone connection and thermal relief settings, in the order of `pads`.
    pad_zones: Vec<super::library_footprint::PadZoneSettings>,
    /// Each pad's own solder mask and paste margins, in the order of `pads`.
    pad_margins: Vec<super::library_footprint::SolderMargins>,
    /// The footprint's own solder mask and paste margins.
    margins: super::library_footprint::SolderMargins,
    /// Reference points declared in the library footprint.
    points: Vec<super::library_footprint::LibraryPoint>,
    /// The library's net-tie pad groups, which KiCad's API keeps from 10.0.1
    /// (#85).
    net_ties: Vec<copperpour_ipc::gen::kiapi::board::types::NetTieDefinition>,
    /// Library footprint clauses or children that typed placement leaves out.
    unsupported: Vec<String>,
    width: f64,
    height: f64,
}

pub(crate) async fn handle_update_pcb_from_schematic(
    args: &serde_json::Value,
    ctx: &ToolContext,
) -> Result<CallToolResult> {
    let schematic = crate::tools::get_path(args, "schematic")?;
    let board = crate::tools::get_path(args, "board")?;
    let dry_run = args["dry_run"].as_bool().unwrap_or(true);
    let expected_revision = args["expected_plan_revision"].as_str().map(str::to_string);
    let delete_without_symbols = args["delete_footprints_without_symbols"]
        .as_bool()
        .unwrap_or(false);
    let options = ResponseOptions::from_args(args);
    if !dry_run && expected_revision.is_none() {
        return Ok(CallToolResult::error_kind(
            crate::mcp::error::ToolErrorKind::InvalidArgument {
                field: "expected_plan_revision".to_string(),
                reason: "required when dry_run is false".to_string(),
            },
            "Apply requires the plan revision returned by a current dry run.",
        ));
    }
    let acknowledged = match &args["acknowledged_net_changes"] {
        serde_json::Value::Null => BTreeSet::new(),
        value => match serde_json::from_value::<Vec<AcknowledgedNetChange>>(value.clone()) {
            Ok(changes) => changes.into_iter().collect::<BTreeSet<_>>(),
            Err(error) => {
                return Ok(CallToolResult::error_kind(
                    crate::mcp::error::ToolErrorKind::InvalidArgument {
                        field: "acknowledged_net_changes".to_string(),
                        reason: error.to_string(),
                    },
                    "Each acknowledged net change names a reference, pad, from and to, copied \
                     from a routed_pad_net_change diagnostic. Nothing was planned or applied.",
                ))
            }
        },
    };
    if !schematic.exists() || !board.exists() {
        let missing = if !schematic.exists() {
            &schematic
        } else {
            &board
        };
        return Ok(CallToolResult::error_kind(
            crate::mcp::error::ToolErrorKind::file_not_found(missing),
            format!("{} does not exist", missing.display()),
        ));
    }

    let hierarchy = match saved_hierarchy_files(&schematic) {
        Ok(files) => files,
        Err(error) => {
            return Ok(conflict_result(format!(
                "saved schematic preflight failed: {error:#}"
            )))
        }
    };
    // Refused before the board is read, as `run_drc` refuses its report path.
    // The hierarchy holds the root schematic and every sheet below it.
    if let Some(output) = &options.output {
        let sources = hierarchy
            .iter()
            .map(PathBuf::as_path)
            .chain([board.as_path()])
            .collect::<Vec<_>>();
        if super::drc::output_is_unusable(output, &sources) {
            return Ok(CallToolResult::error_kind(
                crate::mcp::error::ToolErrorKind::InvalidArgument {
                    field: "output".to_string(),
                    reason: "destination aliases the board or a schematic file, is a directory \
                             or has a non-directory ancestor"
                        .to_string(),
                },
                "Choose a writable file path for the response. Nothing was planned or applied.",
            ));
        }
    }
    let temp = tempfile::Builder::new().suffix(".net").tempfile()?;
    if let Err(error) =
        super::cli::export_netlist(&ctx.config.kicad_cli, &schematic, temp.path(), "kicadsexpr")
            .await
    {
        return Ok(conflict_result(format!(
            "KiCad netlist export failed: {error:#}"
        )));
    }
    let netlist_source = match std::fs::read_to_string(temp.path()) {
        Ok(source) => source,
        Err(error) => {
            return Ok(conflict_result(format!(
                "KiCad netlist export could not be read: {error}"
            )))
        }
    };
    let mut design = match parse_exported_netlist(&netlist_source) {
        Ok(design) => design,
        Err(error) => {
            return Ok(conflict_result(format!(
                "netlist preflight failed: {error:#}"
            )))
        }
    };
    if let Err(error) = apply_saved_symbol_flags(&hierarchy, &mut design) {
        return Ok(conflict_result(format!(
            "schematic flag preflight failed: {error:#}"
        )));
    }

    let what = if dry_run {
        "PCB sync dry run"
    } else {
        "PCB sync apply"
    };
    let ipc_board = board.clone();
    let library_board = board.clone();
    let drc_board = board.clone();
    let cli = ctx.config.kicad_cli.clone();
    let runtime = tokio::runtime::Handle::current();
    let result = attempt_ipc_write(
        ctx,
        &board,
        what,
        move |client| {
            let snapshot = snapshot_board(client, &ipc_board)?;
            let mut asked_version = None;
            // KiCad before 10.0.8 neither serves a footprint's units over its
            // API nor takes them, so the plan leaves them as the board has
            // them and notes each footprint it adds or updates (#682).
            let units_not_carried = if design
                .components
                .iter()
                .any(|component| !component.sheet.symbol_units.is_empty())
            {
                let kicad = kicad_version(client, &mut asked_version)?;
                (!carries_symbol_units(&kicad)).then_some(kicad)
            } else {
                None
            };
            let planned = match units_not_carried {
                None => Cow::Borrowed(&design),
                Some(_) => {
                    let mut without = design.clone();
                    for component in &mut without.components {
                        component.sheet.symbol_units.clear();
                    }
                    Cow::Owned(without)
                }
            };
            let mut plan = plan_sync_acknowledging(
                &netlist_source,
                &planned,
                &snapshot.state,
                &acknowledged,
                delete_without_symbols,
            );
            plan.staging = Some(snapshot.staging.clone());
            let (prepared, unprepared) = prepare_additions(&library_board, &plan);
            // Everything that would fail the apply is found here, so `ready`
            // means ready: each footprint that cannot be prepared, named with
            // the parts that need it, and each connected pad a prepared
            // footprint does not have. So is each updated footprint that would
            // lose what it owns, which the apply would not report. The parts
            // these name are skipped (`skip_parts`), not applied, and so is
            // each footprint on the board that lacks a connected pad, which
            // `plan_sync` found.
            let mut preflight = std::mem::take(&mut plan.refused_updates);
            preflight.extend(
                unprepared
                    .into_iter()
                    .map(UnpreparedFootprint::into_diagnostic),
            );
            preflight.extend(additions_missing_pads(&plan, &prepared));
            // Each replacement's new footprint is read and checked the same
            // way, so `ready` means the apply can build it.
            let replacing = plan.changes.iter().any(|change| {
                matches!(change, PlannedChange::Update { replace_footprint: Some(_), .. })
            });
            let (replacements, copper_layers) = if replacing {
                let kicad = kicad_version(client, &mut asked_version)?;
                let (replacements, refused) =
                    prepare_replacements(&library_board, &plan, &snapshot.state, &kicad);
                preflight.extend(refused);
                let copper = client
                    .get_enabled_layers_in(snapshot.document.clone())?
                    .copper_layer_count;
                preflight.extend(replacements_that_cannot_be_built(
                    &plan,
                    &replacements,
                    copper,
                    &snapshot,
                ));
                (replacements, copper)
            } else {
                (BTreeMap::new(), 0)
            };
            leave_out_unconnected_pins(&mut plan, &prepared, &replacements, &snapshot.state);
            let mut left_out = Vec::new();
            if plan
                .changes
                .iter()
                .any(|change| matches!(change, PlannedChange::Update { .. }))
            {
                let board_text = client.save_document_to_string_in(snapshot.document.clone())?;
                let found = updates_not_carried(&plan, &snapshot.state.footprints, &board_text);
                preflight.extend(found.refused);
                if found.unknown {
                    refuse_plan(&mut plan, preflight);
                    return Ok(sync_response(&plan, "conflict", hierarchy.len(), false, &options));
                }
                left_out = found.left_out;
            }
            // A part the preflight refuses is skipped, as KiCad's Update PCB
            // reports a part it cannot build and updates the rest; a plan
            // left with nothing to apply is refused whole, as before.
            if !preflight.is_empty() {
                let left_out_kiids = left_out
                    .iter()
                    .map(|(kiid, _)| kiid.as_str())
                    .collect::<BTreeSet<_>>();
                if !skip_parts(&mut plan, preflight, &snapshot.state, &left_out_kiids) {
                    return Ok(sync_response(&plan, "conflict", hierarchy.len(), false, &options));
                }
            }
            leave_out_updates(&mut plan, left_out);
            // After the preflight's skips, so a part it left out moves no
            // via or zone (#862).
            let zone_pads = connected_zone_pads(client, &snapshot, &plan, &design.net_names)?;
            if zone_pads.unread {
                plan.diagnostics.push(conflict(
                    "zone_pads_not_read",
                    format!(
                        "KiCad {} cannot list the pads connected to a zone (10.0.1+), so a zone \
                         whose net leaves the schematic follows only the net its old net's pads \
                         move to",
                        zone_pads.kicad
                    ),
                    None,
                ));
            }
            plan_copper_nets(
                &mut plan,
                &snapshot.state,
                &snapshot.copper,
                &design.net_names,
                &zone_pads.pads,
            );
            restage_additions(&mut plan, &prepared, snapshot.state.bounds);
            refresh_revision_with_staging(&mut plan);
            plan.diagnostics
                .extend(library_fields_not_carried(&plan, &prepared));
            plan.diagnostics
                .extend(unsupported_features_not_carried(&plan, &prepared));
            // Asked only when a part has a pad property, a reference point or
            // a net tie, so every other sync sends what it sent before #149.
            let needs_10_0_7 = prepared.values().any(|part| {
                super::library_footprint::needs_kicad_10_0_7(&part.pad_properties, &part.points)
            });
            let has_net_ties = prepared.values().any(|part| !part.net_ties.is_empty());
            let carried = if needs_10_0_7 || has_net_ties {
                let kicad = kicad_version(client, &mut asked_version)?;
                let carried = CarriedByKiCad {
                    fields_10_0_7: needs_10_0_7 && carries_kicad_10_0_7_fields(&kicad),
                    net_ties: carries_net_ties(&kicad),
                };
                if needs_10_0_7 && !carried.fields_10_0_7 {
                    plan.diagnostics
                        .extend(kicad_10_0_7_not_carried(&plan, &prepared, &kicad));
                    // The apply sends something else then, so a plan made
                    // on one side of 10.0.7 is stale on the other.
                    let mut hasher = Sha256::new();
                    hasher.update(plan.plan_revision.as_bytes());
                    hasher.update(b"pad_properties_not_carried");
                    plan.plan_revision = encode_hex(&hasher.finalize());
                }
                if has_net_ties && !carried.net_ties {
                    plan.diagnostics
                        .extend(net_ties_not_carried(&plan, &prepared, &kicad));
                    // Likewise on either side of 10.0.1.
                    let mut hasher = Sha256::new();
                    hasher.update(plan.plan_revision.as_bytes());
                    hasher.update(b"net_ties_not_carried");
                    plan.plan_revision = encode_hex(&hasher.finalize());
                }
                carried
            } else {
                CarriedByKiCad::default()
            };
            if let Some(kicad) = &units_not_carried {
                plan.diagnostics
                    .extend(symbol_units_not_carried(&plan, &design, kicad));
            }

            if dry_run || plan.status == PlanStatus::Conflict {
                let status = match plan.status {
                    PlanStatus::Ready => "ready",
                    PlanStatus::Noop => "noop",
                    PlanStatus::Conflict => "conflict",
                };
                return Ok(sync_response(&plan, status, hierarchy.len(), false, &options));
            }
            if expected_revision.as_deref() != Some(plan.plan_revision.as_str()) {
                plan.status = PlanStatus::Conflict;
                plan.counts.conflicts.planned += 1;
                plan.diagnostics.push(conflict(
                    "stale_plan_revision",
                    "The live board or saved schematic changed; rerun dry run and apply its new plan revision."
                        .to_string(),
                    None,
                ));
                clear_changes(&mut plan);
                return Ok(sync_response(&plan, "conflict", hierarchy.len(), false, &options));
            }
            if plan.status == PlanStatus::Noop {
                return Ok(sync_response(&plan, "noop", hierarchy.len(), false, &options));
            }

            let (creates, updates) = build_mutation_items(
                &plan,
                &prepared,
                &replacements,
                copper_layers,
                &snapshot,
                carried,
            )?;
            let deletions = planned_deletions(&plan);
            let copper_updates = copper_net_updates(&plan, &snapshot)?;
            // What we are about to send, so the board can be held to it.
            let expected = footprint_shapes(creates.iter().chain(updates.iter()));
            let commit = client.run_commit_recovering_in(snapshot.document.clone(), "Update PCB from saved schematic", |client| {
                create_sync_items_in(client, &snapshot.document, &creates)?;
                client.update_items_in(snapshot.document.clone(), updates)?;
                // After the footprints, whose pads bring any new net onto the
                // board; KiCad's Update PCB moves them after the pads too
                // (#862).
                if !copper_updates.is_empty() {
                    client.update_items_in(snapshot.document.clone(), copper_updates)?;
                }
                // In the same commit, after the updates, as KiCad's Update PCB
                // removes unused footprints last (#861).
                client.delete_items_in(
                    snapshot.document.clone(),
                    deletions.iter().map(|footprint| footprint.kiid.clone()).collect(),
                )?;
                Ok(())
            });
            if let Err(error) = commit {
                if error.downcast_ref::<copperpour_ipc::client::ItemRejection>().is_some() {
                    return Ok(crate::tools::ipc_failure_result(
                        copperpour_ipc::IpcFailure::from_error(error),
                    ));
                }
                return Err(error);
            }
            // The commit stands whatever a readback finds, so one that cannot
            // finish is reported on the applied response with `is_error`, never
            // as a refusal, as `acknowledged_apply_unchecked` is.
            let mut unchecked = false;
            match verify_board_matches_what_was_sent(client, &snapshot.document, &expected) {
                Ok(details) => {
                    for detail in details {
                        plan.diagnostics.push(conflict(
                            "board_readback_differs",
                            format!(
                                "the board KiCad wrote differs from what was sent — {detail}. \
                                 No pad was invented, so this is reported rather than \
                                 refused; check the footprint before relying on it."
                            ),
                            None,
                        ));
                    }
                }
                Err(error) => {
                    unchecked = true;
                    plan.diagnostics.push(readback_unchecked("the footprints it sent", &error));
                }
            }
            plan.counts.added.applied = plan.counts.added.planned;
            plan.counts.updated.applied = plan.counts.updated.planned;
            plan.counts.pads_reassigned.applied = plan.counts.pads_reassigned.planned;
            plan.counts.board_only_preserved.applied =
                plan.counts.board_only_preserved.planned;
            plan.counts.skipped_by_flag.applied = plan.counts.skipped_by_flag.planned;
            plan.counts.unassigned_footprint.applied = plan.counts.unassigned_footprint.planned;
            plan.counts.skipped_footprint.applied = plan.counts.skipped_footprint.planned;
            plan.counts.deleted.applied = plan.counts.deleted.planned;
            // KiCad before 10.0.7 answers a delete without per-item results
            // (10.0.7 added them, KiCad 77e2d53da2, and `delete_items_in`
            // checks them), so the board is read back to show each footprint
            // is gone. One KiCad still lists is counted and listed as kept.
            match footprints_still_on_board(client, &snapshot.document, &deletions) {
                Ok(kept) => {
                    for footprint in kept {
                        plan.diagnostics.push(conflict(
                            "board_readback_differs",
                            format!(
                                "{} ({}) is still on the board after the sync asked KiCad to \
                                 delete it; it is counted and listed as kept.",
                                footprint.reference, footprint.kiid
                            ),
                            Some(&footprint.reference),
                        ));
                        plan.counts.deleted.applied -= 1;
                        plan.counts.board_only_preserved.applied += 1;
                        plan.without_symbols.push(footprint);
                    }
                }
                Err(error) => {
                    unchecked = true;
                    plan.diagnostics.push(readback_unchecked(
                        "the footprints it deleted (KiCad accepted each deletion)",
                        &error,
                    ));
                }
            }
            // Each moved via and zone is read back, and one KiCad left on
            // another net is counted as not moved (#862).
            plan.counts.copper_net_changes.applied = plan.counts.copper_net_changes.planned;
            match copper_not_moved(client, &snapshot.document, &plan.copper_net_changes) {
                Ok(not_moved) => {
                    for (moved, net) in not_moved {
                        let mut note = conflict(
                            "board_readback_differs",
                            format!(
                                "the {} {} reads back on '{net}' after the sync moved it to \
                                 '{}'; it is counted as not moved.",
                                match moved.kind {
                                    CopperKind::Via => "via",
                                    CopperKind::Zone => "copper zone",
                                },
                                moved.kiid,
                                moved.to
                            ),
                            None,
                        );
                        note.kiid = Some(moved.kiid.clone());
                        plan.diagnostics.push(note);
                        plan.counts.copper_net_changes.applied -= 1;
                    }
                }
                Err(error) => {
                    unchecked = true;
                    plan.diagnostics
                        .push(readback_unchecked("the vias and zones it moved", &error));
                }
            }
            // Checked only when an acknowledged change is still planned: one
            // whose part was skipped went with the part, so nothing was made
            // that this would report on.
            if !plan
                .diagnostics
                .iter()
                .any(|note| note.code == "routed_pad_net_change_acknowledged")
            {
                return Ok(applied_response(&plan, hierarchy.len(), &options, unchecked));
            }
            // The commit stands whatever this finds, so a check that cannot
            // finish is reported on the applied response, never as a refusal.
            match acknowledged_apply_evidence(client, &snapshot, &plan, &runtime, &cli, &drc_board)
            {
                Ok(evidence) => {
                    plan.acknowledged_apply = Some(evidence);
                    Ok(applied_response(&plan, hierarchy.len(), &options, unchecked))
                }
                Err(error) => {
                    plan.diagnostics.push(conflict(
                        "acknowledged_apply_unchecked",
                        format!(
                            "the commit was applied, but reading back its copper or running DRC \
                             failed: {error:#}. Do not apply this plan again; check the board \
                             with run_drc and sync_live_board."
                        ),
                        None,
                    ));
                    let mut result =
                        sync_response(&plan, "applied", hierarchy.len(), true, &options);
                    result.is_error = true;
                    Ok(result)
                }
            }
        },
    )
    .await?;

    Ok(match result {
        BoardWrite::Ipc(result) => result,
        BoardWrite::File(reason) => conflict_result(format!(
            "{} update_pcb_from_schematic is live-IPC-only and never edits the board file \
             directly. Open the requested board in KiCad and retry.",
            reason.premise()
        )),
        BoardWrite::Refused(result) => {
            // Preserve the structured uncertain outcome instead of claiming
            // the sync was a preflight conflict with no applied changes.
            if matches!(
                crate::mcp::error::extract_error_kind(&result).as_deref(),
                Some("ipc_outcome_unknown" | "ipc_batch_recovered")
            ) {
                return Ok(result);
            }
            let message = result
                .content
                .into_iter()
                .find_map(|content| match content {
                    ToolContent::Text { text } => Some(text),
                    _ => None,
                })
                .unwrap_or_else(|| "KiCad refused the sync request".to_string());
            conflict_result(message)
        }
    })
}

/// How many planned changes a response lists, and where the whole response
/// goes (#69).
struct ResponseOptions {
    limit: usize,
    output: Option<PathBuf>,
}

impl Default for ResponseOptions {
    fn default() -> Self {
        Self {
            limit: Self::DEFAULT_LIMIT,
            output: None,
        }
    }
}

impl ResponseOptions {
    /// As many as `run_drc` lists violations when no `limit` is named.
    const DEFAULT_LIMIT: usize = 50;

    fn from_args(args: &serde_json::Value) -> Self {
        Self {
            // The schema holds `limit` to a non-negative integer, which JSON
            // may also write as `2.0`. A float casts to `usize` saturating.
            limit: args["limit"]
                .as_f64()
                .map_or(Self::DEFAULT_LIMIT, |limit| limit as usize),
            output: args["output"].as_str().map(PathBuf::from),
        }
    }
}

/// The sync's response, in the order a caller reads it: the verdict, the
/// plan revision and the counts first, the planned changes last (#69).
/// `json!` sorts its keys, which put `status` and `plan_revision` after every
/// change.
#[derive(Serialize)]
struct SyncResponse<'a> {
    status: &'a str,
    plan_revision: &'a str,
    coverage: serde_json::Value,
    /// `changes` holds the first `changes_shown` of `changes_count` planned
    /// changes, as `run_drc` reports `shown` and `truncated` beside its list.
    changes_count: usize,
    changes_shown: usize,
    changes_truncated: bool,
    /// What added footprints were staged beside: the live board as KiCad
    /// measured it, with the classes it would not list (#688). Null only when
    /// the plan never reached the board.
    staging: Option<serde_json::Value>,
    undo: Option<&'static str>,
    /// After an apply that took acknowledged net changes, each net they name
    /// with its routed copper before the commit and as KiCad reads it back
    /// after (#38). Absent from every other response.
    #[serde(skip_serializing_if = "Option::is_none")]
    routed_copper: Option<&'a [RoutedCopper]>,
    /// After such an apply, kicad-cli's DRC of the board KiCad then holds,
    /// under the project's rules (#38). Absent from every other response.
    #[serde(skip_serializing_if = "Option::is_none")]
    drc: Option<&'a serde_json::Value>,
    diagnostics: Cow<'a, [SyncDiagnostic]>,
    /// Schematic components with no footprint: reported per part with what
    /// the board holds for them, never planned and never fatal (#507). A
    /// collection takes a plural noun (docs/NAMING_CONVENTIONS.md); the
    /// `coverage` entry beside it is a count category and stays singular.
    unassigned_footprints: &'a [UnassignedFootprint],
    /// Parts the preflight left out, each with the diagnostic that says why;
    /// every other part is synced. Counted in `coverage.skipped_footprint`.
    skipped_footprints: &'a [SyncDiagnostic],
    /// Board footprints no symbol matched, without the board-only attribute,
    /// that the plan keeps (#861); counted in `coverage.board_only_preserved`
    /// with the board-only ones. `delete_footprints_without_symbols` plans
    /// the unlocked ones as `delete` changes instead.
    footprints_without_symbols: &'a [FootprintWithoutSymbol],
    /// The vias and copper zones the plan moves off nets that leave the
    /// schematic, as KiCad's Update PCB moves them (#862); counted in
    /// `coverage.copper_net_changes`.
    copper_net_changes: &'a [CopperNetChange],
    /// Bounded as rendered only: `plan.changes` stays whole, since the plan
    /// revision hashes it and the apply sends it.
    changes: &'a [PlannedChange],
}

impl<'a> SyncResponse<'a> {
    fn new(
        plan: &'a SyncPlan,
        status: &'a str,
        hierarchy_files: usize,
        applied: bool,
        limit: usize,
    ) -> Self {
        let changes = BoundedList::new(&plan.changes, limit);
        Self {
            status,
            plan_revision: &plan.plan_revision,
            changes_count: changes.count,
            changes_shown: changes.shown,
            changes_truncated: changes.truncated,
            changes: changes.items,
            coverage: serde_json::json!({
                "source": "saved_schematic_hierarchy",
                "hierarchy_files": hierarchy_files,
                "transport": "live_kicad_ipc",
                "atomicity": "single_kicad_undo_commit",
                "footprints_added": plan.counts.added,
                "footprints_updated": plan.counts.updated,
                "pads_reassigned": plan.counts.pads_reassigned,
                "board_only_preserved": plan.counts.board_only_preserved,
                "skipped_by_flag": plan.counts.skipped_by_flag,
                "unassigned_footprint": plan.counts.unassigned_footprint,
                "skipped_footprint": plan.counts.skipped_footprint,
                "footprints_deleted": plan.counts.deleted,
                "copper_net_changes": plan.counts.copper_net_changes,
                "conflicts": plan.counts.conflicts
            }),
            staging: plan.staging.as_ref().map(StagingEvidence::json),
            undo: applied.then_some("Ctrl-Z reverses the whole schematic-to-PCB update."),
            routed_copper: plan
                .acknowledged_apply
                .as_ref()
                .map(|apply| apply.routed_copper.as_slice()),
            drc: plan.acknowledged_apply.as_ref().map(|apply| &apply.drc),
            diagnostics: Cow::Borrowed(&plan.diagnostics),
            unassigned_footprints: &plan.unassigned,
            skipped_footprints: &plan.skipped,
            footprints_without_symbols: &plan.without_symbols,
            copper_net_changes: &plan.copper_net_changes,
        }
    }
}

fn sync_response(
    plan: &SyncPlan,
    status: &str,
    hierarchy_files: usize,
    applied: bool,
    options: &ResponseOptions,
) -> CallToolResult {
    let mut response = SyncResponse::new(plan, status, hierarchy_files, applied, options.limit);
    let Some(output) = &options.output else {
        return CallToolResult::json(&response);
    };
    let whole = SyncResponse::new(plan, status, hierarchy_files, applied, usize::MAX);
    let written = serde_json::to_string_pretty(&whole)
        .map_err(anyhow::Error::from)
        .and_then(|text| super::drc::write_report_blocking(output, &text));
    let Err(error) = written else {
        return CallToolResult::json(&response);
    };
    let reason = format!(
        "the full response was not written to {}: {error:#}",
        output.display()
    );
    // As `run_drc` answers a report it could not write: with nothing applied
    // the call fails and can be repeated; after an apply it fails carrying the
    // applied response, so the commit is not repeated blindly.
    if !applied {
        return CallToolResult::error_kind(
            crate::mcp::error::ToolErrorKind::HandlerError { reason },
            "Nothing was applied. Choose a writable output path and run the sync again.",
        );
    }
    response.diagnostics.to_mut().push(conflict(
        "output_not_written",
        format!("{reason}. The commit was applied; do not apply this plan again."),
        None,
    ));
    let mut result = CallToolResult::json(&response);
    result.is_error = true;
    result
}

/// A refusal before any plan exists: the saved hierarchy, the netlist export
/// or the IPC preflight failed. Its one diagnostic is built by `conflict`, the
/// constructor every planned diagnostic uses, so a caller reads the same
/// fields whichever stage refused.
fn conflict_result(message: String) -> CallToolResult {
    let value = serde_json::json!({
        "status": "conflict",
        "coverage": {
            "transport": "live_kicad_ipc",
            "footprints_added": CountPair::default(),
            "footprints_updated": CountPair::default(),
            "pads_reassigned": CountPair::default(),
            "board_only_preserved": CountPair::default(),
            "skipped_by_flag": CountPair::default(),
            "unassigned_footprint": CountPair::default(),
            "skipped_footprint": CountPair::default(),
            "footprints_deleted": CountPair::default(),
            "copper_net_changes": CountPair::default(),
            "conflicts": CountPair { planned: 1, applied: 0 }
        },
        "diagnostics": [conflict("preflight_conflict", message, None)]
    });
    CallToolResult {
        content: vec![ToolContent::Text {
            text: value.to_string(),
        }],
        is_error: true,
    }
}

/// The applied response, an error when a readback after the commit could not
/// finish.
fn applied_response(
    plan: &SyncPlan,
    hierarchy_files: usize,
    options: &ResponseOptions,
    unchecked: bool,
) -> CallToolResult {
    let mut result = sync_response(plan, "applied", hierarchy_files, true, options);
    result.is_error |= unchecked;
    result
}

/// The note for a readback after the commit that failed: the commit stands.
fn readback_unchecked(what: &str, error: &anyhow::Error) -> SyncDiagnostic {
    conflict(
        "apply_readback_unchecked",
        format!(
            "the commit was applied, but reading back {what} failed: {error:#}. Do not apply \
             this plan again; check the board with a new dry run, or run_drc with \
             sync_live_board."
        ),
        None,
    )
}
